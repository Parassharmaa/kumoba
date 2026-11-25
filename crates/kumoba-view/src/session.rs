use crate::flow::Flow;
use crate::http::Link;
use crate::screen::{Failure, Screen};
use crate::ws::{Message, Reader, Writer};
use kumoba_view_codec::{Encoder, Rect};
use std::collections::VecDeque;
use std::time::{Duration, Instant};

pub const SERVER_HELLO: u8 = 1;
pub const SERVER_FRAME: u8 = 2;
const SERVER_CURSOR: u8 = 3;
const SERVER_POINTER: u8 = 4;
const SERVER_CLIPBOARD: u8 = 5;
const CLIENT_ACK: u8 = 1;
const CLIENT_POINTER: u8 = 2;
const CLIENT_BUTTON: u8 = 3;
const CLIENT_KEY: u8 = 4;
const CLIENT_WHEEL: u8 = 5;
const CLIENT_PAUSE: u8 = 6;
const CLIENT_SYMBOL: u8 = 7;
const CLIENT_TEXT: u8 = 8;
const CLIENT_CLIPBOARD: u8 = 9;
const CLIENT_COPY: u8 = 10;
const CLIENT_CHORD: u8 = 11;
const HELLO_CHORDS: u8 = 1;
const HELLO_WATCH: u8 = 2;

const STALL: Duration = Duration::from_secs(5);
const POINTER_EVERY: Duration = Duration::from_millis(33);
const IDLE_WAKE: Duration = Duration::from_millis(100);
const KEYMAP_SETTLE: Duration = Duration::from_millis(50);
const KEYMAP_RESETTLE: Duration = Duration::from_millis(150);
const MAX_STROKES: usize = 8192;
const STEPS_PER_TURN: usize = 8192;
const MAX_WAITING_TEXT: usize = 256 * 1024;
const LOOK_AHEAD: usize = 256;
const MAX_QUEUED_BYTES: usize = 4 * 1024 * 1024;

#[derive(Clone, Copy)]
pub struct Keepalive {
    pub ping: Duration,
    pub silence: Duration,
}

pub const KEEPALIVE: Keepalive = Keepalive {
    ping: Duration::from_secs(15),
    silence: Duration::from_secs(45),
};

#[derive(Clone, Copy)]
pub struct Settings {
    pub fps: u32,
    pub max_mbps: u32,
    pub keepalive: Keepalive,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Access {
    Control,
    Watch,
}

enum Stroke {
    Code(u8, bool),
    Symbol(u32, bool),
    Pointer(i16, i16),
    Button(u8, bool),
    Chord(u8, bool),
    Offer(Vec<u8>),
    ExpectCopy,
    Text(Vec<u32>, usize),
}

struct Strokes {
    queued: VecDeque<Stroke>,
    text: usize,
}

impl Strokes {
    fn push(&mut self, stroke: Stroke) -> Result<(), Failure> {
        if self.queued.len() >= MAX_STROKES {
            return Err("the viewer sent input faster than the desktop takes it".into());
        }
        if let Stroke::Text(symbols, _) = &stroke {
            if self.text + symbols.len() > MAX_WAITING_TEXT {
                return Err("the viewer sent more text than the desktop can type".into());
            }
            self.text += symbols.len();
        }
        self.queued.push_back(stroke);
        Ok(())
    }

    fn typing(&self) -> Option<(u32, bool)> {
        match self.queued.front()? {
            &Stroke::Symbol(symbol, down) => Some((symbol, down)),
            Stroke::Text(symbols, step) => Some((symbols[step / 2], step % 2 == 0)),
            _ => None,
        }
    }

    fn ahead(&self) -> impl Iterator<Item = u32> + '_ {
        self.queued
            .iter()
            .flat_map(|stroke| match stroke {
                Stroke::Symbol(symbol, true) => std::slice::from_ref(symbol),
                Stroke::Text(symbols, step) => &symbols[step / 2..],
                _ => &[],
            })
            .copied()
            .take(LOOK_AHEAD)
    }

    fn advance(&mut self) {
        if let Some(Stroke::Text(symbols, step)) = self.queued.front_mut() {
            *step += 1;
            if *step < symbols.len() * 2 {
                return;
            }
            self.text -= symbols.len();
        }
        self.queued.pop_front();
    }
}

fn symbol_of(character: char) -> Option<u32> {
    match character {
        '\n' | '\r' => Some(0xff0d),
        '\t' => Some(0xff09),
        other if (other as u32) < 0x20 || other as u32 == 0x7f => None,
        other if other as u32 <= 0xff => Some(other as u32),
        other => Some(0x0100_0000 + other as u32),
    }
}

struct Sent {
    sequence: u32,
    at: Instant,
}

struct Pace {
    interval: Duration,
    credit: Duration,
    updated: Instant,
}

impl Pace {
    fn new(fps: u32, now: Instant) -> Self {
        let interval = Duration::from_nanos(1_000_000_000 / u64::from(fps.max(1)));
        Self {
            interval,
            credit: interval * 2,
            updated: now,
        }
    }

    fn refill(&mut self, now: Instant) {
        self.credit = (self.credit + now.duration_since(self.updated)).min(self.interval * 2);
        self.updated = now;
    }

    fn wait(&mut self, now: Instant) -> Duration {
        self.refill(now);
        self.interval.saturating_sub(self.credit)
    }

    fn spend(&mut self, now: Instant) {
        self.refill(now);
        self.credit = self.credit.saturating_sub(self.interval);
    }
}

fn poll(
    screen: i32,
    socket: i32,
    writable: bool,
    timeout: Option<Duration>,
) -> std::io::Result<()> {
    let mut fds = [
        libc::pollfd {
            fd: screen,
            events: libc::POLLIN,
            revents: 0,
        },
        libc::pollfd {
            fd: socket,
            events: libc::POLLIN | if writable { libc::POLLOUT } else { 0 },
            revents: 0,
        },
    ];
    let wait = timeout.map_or(-1, |value| {
        i32::try_from(value.as_micros().div_ceil(1000)).unwrap_or(i32::MAX)
    });
    let result = unsafe { libc::poll(fds.as_mut_ptr(), 2, wait) };
    if result < 0 {
        let error = std::io::Error::last_os_error();
        if error.kind() != std::io::ErrorKind::Interrupted {
            return Err(error);
        }
    }
    Ok(())
}

fn sooner(timeout: Option<Duration>, other: Option<Duration>) -> Option<Duration> {
    match (timeout, other) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (a, b) => a.or(b),
    }
}

pub fn run<S: Screen>(
    mut stream: impl Link,
    settings: Settings,
    access: Access,
    open: impl FnOnce() -> Result<S, Failure>,
) -> Result<(), Failure> {
    stream.streaming()?;
    let mut screen = open()?;
    let (width, height) = screen.size();
    let mut encoder = Encoder::new(width, height);
    let mut reader = Reader::default();
    let mut writer = Writer::default();
    let mut hello = vec![SERVER_HELLO];
    hello.extend_from_slice(&(width as u16).to_le_bytes());
    hello.extend_from_slice(&(height as u16).to_le_bytes());
    hello.extend_from_slice(&(settings.fps as u16).to_le_bytes());
    let mut flags = if screen.chords() { HELLO_CHORDS } else { 0 };
    if access == Access::Watch {
        flags |= HELLO_WATCH;
    }
    hello.push(flags);
    writer.push(&hello);
    let started = Instant::now();
    let mut pace = Pace::new(settings.fps, started);
    let mut flow = Flow::new(settings.max_mbps, settings.fps, Duration::ZERO);
    let mut flight: VecDeque<Sent> = VecDeque::new();
    let mut sequence = 0u32;
    let mut payload = Vec::new();
    let mut message = Vec::new();
    let mut full = true;
    let mut paused = false;
    let mut pointer = (u16::MAX, u16::MAX);
    let mut pointer_checked = started;
    let mut strokes = Strokes {
        queued: VecDeque::new(),
        text: 0,
    };
    let mut strokes_ready = started;
    let mut typed = started;
    let mut heard = started;
    let mut next_ping = started + settings.keepalive.ping;
    loop {
        writer.flush(&mut stream)?;
        if !reader.fill(&mut stream)? {
            return Ok(());
        }
        let mut answered = false;
        while let Some(incoming) = reader.next()? {
            answered = true;
            let data = match incoming {
                Message::Binary(data) => data,
                Message::Ping(payload) => {
                    writer.pong(&payload);
                    continue;
                }
                Message::Pong => continue,
                Message::Close => return Ok(()),
            };
            if access == Access::Watch
                && !matches!(data.first(), Some(&(CLIENT_ACK | CLIENT_PAUSE)))
            {
                continue;
            }
            match data.first() {
                Some(&CLIENT_ACK) if data.len() >= 5 => {
                    let acked = u32::from_le_bytes([data[1], data[2], data[3], data[4]]);
                    let waited = if data.len() >= 9 {
                        Duration::from_micros(u64::from(u32::from_le_bytes([
                            data[5], data[6], data[7], data[8],
                        ])))
                    } else {
                        Duration::ZERO
                    };
                    while let Some(front) = flight.front() {
                        if front.sequence > acked {
                            break;
                        }
                        if front.sequence == acked {
                            let trip = front.at.elapsed().saturating_sub(waited);
                            flow.acked(trip, started.elapsed());
                        }
                        flight.pop_front();
                    }
                }
                Some(&CLIENT_POINTER) if data.len() >= 5 => {
                    let x = u16::from_le_bytes([data[1], data[2]]).min(width as u16 - 1);
                    let y = u16::from_le_bytes([data[3], data[4]]).min(height as u16 - 1);
                    if let Some(Stroke::Pointer(last_x, last_y)) = strokes.queued.back_mut() {
                        (*last_x, *last_y) = (x as i16, y as i16);
                    } else {
                        strokes.push(Stroke::Pointer(x as i16, y as i16))?;
                    }
                }
                Some(&CLIENT_BUTTON) if data.len() >= 3 && (1..=9).contains(&data[1]) => {
                    strokes.push(Stroke::Button(data[1], data[2] != 0))?;
                }
                Some(&CLIENT_KEY) if data.len() >= 4 => {
                    let code = u16::from_le_bytes([data[1], data[2]]);
                    if let Ok(keycode) = u8::try_from(code + 8) {
                        strokes.push(Stroke::Code(keycode, data[3] != 0))?;
                    }
                }
                Some(&CLIENT_WHEEL) if data.len() >= 3 => {
                    let (dx, dy) = (data[1] as i8, data[2] as i8);
                    for (delta, negative, positive) in [(dy, 4u8, 5u8), (dx, 6, 7)] {
                        let button = if delta < 0 { negative } else { positive };
                        for _ in 0..delta.unsigned_abs().min(10) {
                            strokes.push(Stroke::Button(button, true))?;
                            strokes.push(Stroke::Button(button, false))?;
                        }
                    }
                }
                Some(&CLIENT_PAUSE) if data.len() >= 2 => paused = data[1] != 0,
                Some(&CLIENT_SYMBOL) if data.len() >= 6 => {
                    let symbol = u32::from_le_bytes([data[1], data[2], data[3], data[4]]);
                    strokes.push(Stroke::Symbol(symbol, data[5] != 0))?;
                }
                Some(&CLIENT_TEXT) => {
                    if let Ok(text) = std::str::from_utf8(&data[1..]) {
                        let symbols: Vec<u32> = text.chars().filter_map(symbol_of).collect();
                        if !symbols.is_empty() {
                            strokes.push(Stroke::Text(symbols, 0))?;
                        }
                    }
                }
                Some(&CLIENT_CLIPBOARD) => strokes.push(Stroke::Offer(data[1..].to_vec()))?,
                Some(&CLIENT_COPY) => strokes.push(Stroke::ExpectCopy)?,
                Some(&CLIENT_CHORD) if data.len() >= 3 && b"cvx".contains(&data[1]) => {
                    strokes.push(Stroke::Chord(data[1], data[2] != 0))?;
                }
                _ => {}
            }
        }
        let now = Instant::now();
        if answered {
            heard = now;
        }
        if now.duration_since(heard) >= settings.keepalive.silence {
            return Err("the viewer stopped answering".into());
        }
        if now >= next_ping {
            writer.ping();
            next_ping = now + settings.keepalive.ping;
        }
        let mut steps = 0;
        while now >= strokes_ready && steps < STEPS_PER_TURN {
            steps += 1;
            if let Some((symbol, down)) = strokes.typing() {
                if down && screen.needs_key(symbol) {
                    let settled = typed + KEYMAP_RESETTLE;
                    if now < settled {
                        strokes_ready = settled;
                        break;
                    }
                    let reused = screen.bind(strokes.ahead())?;
                    strokes_ready = now
                        + if reused {
                            KEYMAP_RESETTLE
                        } else {
                            KEYMAP_SETTLE
                        };
                    break;
                }
                screen.symbol(symbol, down)?;
                typed = now;
            } else {
                match strokes.queued.front() {
                    None => break,
                    Some(&Stroke::Code(keycode, down)) => screen.key(keycode, down)?,
                    Some(&Stroke::Pointer(x, y)) => screen.pointer(x, y)?,
                    Some(&Stroke::Button(button, down)) => screen.button(button, down)?,
                    Some(&Stroke::Chord(letter, shift)) => screen.chord(letter, shift)?,
                    Some(Stroke::Offer(text)) => screen.offer(text)?,
                    Some(Stroke::ExpectCopy) => screen.expect_copy(),
                    Some(Stroke::Symbol(..) | Stroke::Text(..)) => {}
                }
            }
            strokes.advance();
        }
        if flight
            .front()
            .is_some_and(|sent| now.duration_since(sent.at) > STALL)
        {
            return Err("the viewer stopped acknowledging frames".into());
        }
        screen.drain()?;
        if let Some(text) = screen.copied() {
            message.clear();
            message.push(SERVER_CLIPBOARD);
            message.extend_from_slice(&text);
            writer.push(&message);
        }
        if !paused {
            if let Some(cursor) = screen.cursor()? {
                message.clear();
                message.push(SERVER_CURSOR);
                for value in [cursor.width, cursor.height, cursor.hot_x, cursor.hot_y] {
                    message.extend_from_slice(&value.to_le_bytes());
                }
                message.extend_from_slice(&cursor.rgba);
                writer.push(&message);
            }
            if now.duration_since(pointer_checked) >= POINTER_EVERY {
                pointer_checked = now;
                let at = screen.position()?;
                if at != pointer {
                    pointer = at;
                    message.clear();
                    message.push(SERVER_POINTER);
                    message.extend_from_slice(&at.0.to_le_bytes());
                    message.extend_from_slice(&at.1.to_le_bytes());
                    writer.push(&message);
                }
            }
        }
        let dirty = screen.drain()? || full;
        if screen.resized() {
            return Ok(());
        }
        let wanted = dirty || encoder.pending();
        let open = !paused && flight.len() < flow.window() && writer.queued() < MAX_QUEUED_BYTES;
        let writable = writer.queued() > 0;
        let typing = (!strokes.queued.is_empty()).then(|| {
            strokes_ready
                .saturating_duration_since(now)
                .max(Duration::from_millis(1))
        });
        let alive = next_ping
            .min(heard + settings.keepalive.silence)
            .saturating_duration_since(now)
            .max(Duration::from_millis(1));
        let typing = sooner(typing, Some(alive));
        if !(wanted && open) {
            let timeout = if paused { None } else { Some(IDLE_WAKE) };
            poll(
                screen.fd(),
                stream.as_raw_fd(),
                writable,
                sooner(timeout, typing),
            )?;
            continue;
        }
        let wait = pace.wait(now);
        if !wait.is_zero() {
            poll(
                screen.fd(),
                stream.as_raw_fd(),
                writable,
                sooner(Some(wait), typing),
            )?;
            continue;
        }
        let Some(budget) = flow.budget(started.elapsed()) else {
            let wait = flow.wait().max(Duration::from_millis(1));
            poll(
                screen.fd(),
                stream.as_raw_fd(),
                writable,
                sooner(Some(wait), typing),
            )?;
            continue;
        };
        let captured = started.elapsed();
        let area = if dirty {
            screen.grab(full)?
        } else {
            Rect {
                x: 0,
                y: 0,
                width: 0,
                height: 0,
            }
        };
        let moves = screen.moves();
        let stats = encoder.encode(screen.pixels(), area, &moves, budget, &mut payload);
        pace.spend(Instant::now());
        if !stats.changed {
            continue;
        }
        full = false;
        sequence += 1;
        message.clear();
        message.push(SERVER_FRAME);
        message.extend_from_slice(&sequence.to_le_bytes());
        message.extend_from_slice(&(captured.as_micros() as u64).to_le_bytes());
        message.extend_from_slice(&payload);
        writer.push(&message);
        flow.sent(message.len(), stats.lossy_blocks > 0);
        flight.push_back(Sent {
            sequence,
            at: Instant::now(),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keymap::{Keymap, Place, SHIFT_LEFT};
    use crate::screen::Cursor;
    use std::collections::HashMap;
    use std::io::{Read, Write};
    use std::os::fd::{AsRawFd, RawFd};
    use std::os::unix::net::UnixStream;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex, mpsc};

    const SIDE: usize = 64;

    #[derive(Clone, Debug, PartialEq)]
    enum Seen {
        Symbol(u32, bool),
        Key(u8, bool),
        Pointer(i16, i16),
        Button(u8, bool),
        Chord(u8, bool),
        Offer(String),
        ExpectCopy,
    }

    struct Layout {
        keys: Keymap,
        bound: HashMap<u8, u32>,
    }

    fn layout() -> Layout {
        let mut symbols = vec![0u32; 2 * 16];
        symbols[0..2].copy_from_slice(&[0x61, 0x41]);
        symbols[2..4].copy_from_slice(&[0x62, 0x42]);
        symbols[4..6].copy_from_slice(&[SHIFT_LEFT, 0]);
        Layout {
            keys: Keymap::new(8, 2, &symbols, &[]),
            bound: HashMap::new(),
        }
    }

    struct Still {
        wake: UnixStream,
        pixels: Vec<u32>,
        seen: Arc<Mutex<Vec<Seen>>>,
        layout: Option<Layout>,
        turns: Arc<AtomicUsize>,
        changing: bool,
    }

    impl Still {
        fn saw(&self, seen: Seen) {
            self.seen.lock().unwrap().push(seen);
        }
    }

    impl Screen for Still {
        fn size(&self) -> (usize, usize) {
            (SIDE, SIDE)
        }
        fn fd(&self) -> RawFd {
            self.wake.as_raw_fd()
        }
        fn pixels(&self) -> &[u32] {
            &self.pixels
        }
        fn drain(&mut self) -> Result<bool, Failure> {
            self.turns.fetch_add(1, Ordering::SeqCst);
            Ok(self.changing)
        }
        fn resized(&self) -> bool {
            false
        }
        fn grab(&mut self, _full: bool) -> Result<Rect, Failure> {
            self.pixels[0] = self.pixels[0].wrapping_add(0x0001_0101);
            Ok(Rect {
                x: 0,
                y: 0,
                width: SIDE,
                height: SIDE,
            })
        }
        fn moves(&mut self) -> Vec<(i32, i32)> {
            Vec::new()
        }
        fn cursor(&mut self) -> Result<Option<Cursor>, Failure> {
            Ok(None)
        }
        fn position(&mut self) -> Result<(u16, u16), Failure> {
            Ok((0, 0))
        }
        fn pointer(&mut self, x: i16, y: i16) -> Result<(), Failure> {
            self.saw(Seen::Pointer(x, y));
            Ok(())
        }
        fn button(&mut self, button: u8, down: bool) -> Result<(), Failure> {
            self.saw(Seen::Button(button, down));
            Ok(())
        }
        fn key(&mut self, keycode: u8, down: bool) -> Result<(), Failure> {
            self.saw(Seen::Key(keycode, down));
            Ok(())
        }
        fn symbol(&mut self, symbol: u32, down: bool) -> Result<(), Failure> {
            let typed = match self.layout.as_mut() {
                Some(layout) if down => match layout.keys.place(symbol) {
                    Some(Place::Key(code, _)) => layout.bound.get(&code).copied().unwrap_or(symbol),
                    _ => return Ok(()),
                },
                _ => symbol,
            };
            self.saw(Seen::Symbol(typed, down));
            Ok(())
        }
        fn needs_key(&self, symbol: u32) -> bool {
            self.layout
                .as_ref()
                .is_some_and(|layout| layout.keys.needs_key(symbol))
        }
        fn bind(&mut self, symbols: impl Iterator<Item = u32>) -> Result<bool, Failure> {
            let Some(layout) = self.layout.as_mut() else {
                return Ok(false);
            };
            let binds = layout.keys.plan(symbols);
            for &(code, symbol, _) in &binds {
                layout.bound.insert(code, symbol);
            }
            Ok(binds.iter().any(|&(_, _, reused)| reused))
        }
        fn chord(&mut self, letter: u8, shift: bool) -> Result<(), Failure> {
            self.saw(Seen::Chord(letter, shift));
            Ok(())
        }
        fn offer(&mut self, text: &[u8]) -> Result<(), Failure> {
            self.saw(Seen::Offer(String::from_utf8_lossy(text).into_owned()));
            Ok(())
        }
        fn expect_copy(&mut self) {
            self.saw(Seen::ExpectCopy);
        }
        fn copied(&mut self) -> Option<Vec<u8>> {
            None
        }
        fn chords(&self) -> bool {
            true
        }
    }

    const QUICK: Keepalive = Keepalive {
        ping: Duration::from_millis(50),
        silence: Duration::from_millis(300),
    };

    fn settings() -> Settings {
        Settings {
            fps: 30,
            max_mbps: 10,
            keepalive: QUICK,
        }
    }

    struct Session {
        browser: UnixStream,
        ended: mpsc::Receiver<Result<(), String>>,
        seen: Arc<Mutex<Vec<Seen>>>,
        turns: Arc<AtomicUsize>,
        _wake: UnixStream,
    }

    fn start(settings: Settings) -> Session {
        open_with(settings, None)
    }

    fn open_with(settings: Settings, layout: Option<Layout>) -> Session {
        open_as(settings, layout, Access::Control, false)
    }

    fn open_as(
        settings: Settings,
        layout: Option<Layout>,
        access: Access,
        changing: bool,
    ) -> Session {
        let (browser, engine) = UnixStream::pair().unwrap();
        let (wake, screen_end) = UnixStream::pair().unwrap();
        let (done, ended) = mpsc::channel();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let turns = Arc::new(AtomicUsize::new(0));
        let screen = Still {
            wake: screen_end,
            pixels: vec![0x0033_6699; SIDE * SIDE],
            seen: Arc::clone(&seen),
            layout,
            turns: Arc::clone(&turns),
            changing,
        };
        std::thread::spawn(move || {
            let result = run(engine, settings, access, || Ok(screen));
            let _ = done.send(result.map_err(|error| error.to_string()));
        });
        Session {
            browser,
            ended,
            seen,
            turns,
            _wake: wake,
        }
    }

    fn masked(opcode: u8, body: &[u8]) -> Vec<u8> {
        let mask = [3u8, 1, 4, 1];
        let mut frame = vec![0x80 | opcode];
        match body.len() {
            short @ ..126 => frame.push(0x80 | short as u8),
            wide @ ..=0xffff => {
                frame.push(0x80 | 126);
                frame.extend_from_slice(&(wide as u16).to_be_bytes());
            }
            long => {
                frame.push(0x80 | 127);
                frame.extend_from_slice(&(long as u64).to_be_bytes());
            }
        }
        frame.extend_from_slice(&mask);
        frame.extend(body.iter().enumerate().map(|(i, b)| b ^ mask[i & 3]));
        frame
    }

    fn try_frame(browser: &mut UnixStream) -> std::io::Result<(u8, Vec<u8>)> {
        let mut head = [0u8; 2];
        browser.read_exact(&mut head)?;
        let length = match head[1] & 0x7f {
            126 => {
                let mut wide = [0u8; 2];
                browser.read_exact(&mut wide)?;
                usize::from(u16::from_be_bytes(wide))
            }
            127 => {
                let mut wide = [0u8; 8];
                browser.read_exact(&mut wide)?;
                usize::try_from(u64::from_be_bytes(wide)).unwrap()
            }
            short => usize::from(short),
        };
        let mut body = vec![0u8; length];
        browser.read_exact(&mut body)?;
        Ok((head[0], body))
    }

    fn read_until_frame(browser: &mut UnixStream) -> u32 {
        loop {
            let (first, body) = try_frame(browser).unwrap();
            if first == 0x82 && body.first() == Some(&SERVER_FRAME) {
                return u32::from_le_bytes([body[1], body[2], body[3], body[4]]);
            }
        }
    }

    fn ack(sequence: u32) -> Vec<u8> {
        let mut body = vec![CLIENT_ACK];
        body.extend_from_slice(&sequence.to_le_bytes());
        masked(0x2, &body)
    }

    fn assert_let_go(session: &Session) {
        let ended = session
            .ended
            .recv_timeout(QUICK.silence * 10)
            .expect("the session was still open");
        assert_eq!(ended, Err("the viewer stopped answering".to_string()));
    }

    #[test]
    fn a_paused_viewer_that_goes_silent_is_let_go() {
        let mut session = start(settings());
        session
            .browser
            .write_all(&masked(0x2, &[CLIENT_PAUSE, 1]))
            .unwrap();
        assert_let_go(&session);
    }

    #[test]
    fn a_viewer_of_a_still_desktop_that_goes_silent_is_let_go() {
        let mut session = start(settings());
        let sequence = read_until_frame(&mut session.browser);
        session.browser.write_all(&ack(sequence)).unwrap();
        assert_let_go(&session);
    }

    #[test]
    fn a_viewer_that_answers_pings_stays_open_while_watching_and_while_paused() {
        let session = start(settings());
        let mut browser = session.browser.try_clone().unwrap();
        let (pause, paused) = mpsc::channel::<()>();
        let pings = std::thread::spawn(move || {
            let (mut watching, mut hidden) = (0, 0);
            let mut is_paused = false;
            while let Ok((first, body)) = try_frame(&mut browser) {
                let mut reply = Vec::new();
                if first == 0x89 {
                    if is_paused {
                        hidden += 1;
                    } else {
                        watching += 1;
                    }
                    reply.extend(masked(0xa, &body));
                } else if first == 0x82 && body.first() == Some(&SERVER_FRAME) {
                    reply.extend(ack(u32::from_le_bytes([
                        body[1], body[2], body[3], body[4],
                    ])));
                }
                if !is_paused && paused.try_recv().is_ok() {
                    is_paused = true;
                    reply.extend(masked(0x2, &[CLIENT_PAUSE, 1]));
                }
                if browser.write_all(&reply).is_err() {
                    break;
                }
            }
            (watching, hidden)
        });
        std::thread::sleep(QUICK.silence * 4);
        assert!(
            session.ended.try_recv().is_err(),
            "a watching viewer was let go"
        );
        pause.send(()).unwrap();
        std::thread::sleep(QUICK.silence * 4);
        assert!(
            session.ended.try_recv().is_err(),
            "a paused viewer was let go"
        );
        session.browser.shutdown(std::net::Shutdown::Both).unwrap();
        assert_eq!(
            session.ended.recv_timeout(Duration::from_secs(5)).unwrap(),
            Ok(())
        );
        let (watching, hidden) = pings.join().unwrap();
        assert!(watching >= 10, "only {watching} pings while watching");
        assert!(hidden >= 10, "only {hidden} pings while paused");
    }

    #[test]
    fn answers_a_viewer_ping_with_the_same_payload() {
        let mut session = start(settings());
        session
            .browser
            .write_all(&masked(0x9, b"still there?"))
            .unwrap();
        loop {
            let (first, body) = try_frame(&mut session.browser).unwrap();
            if first == 0x8a {
                assert_eq!(body, b"still there?");
                break;
            }
        }
    }

    fn steady() -> Settings {
        Settings {
            fps: 30,
            max_mbps: 10,
            keepalive: KEEPALIVE,
        }
    }

    fn answering(session: &Session) {
        let mut browser = session.browser.try_clone().unwrap();
        std::thread::spawn(move || {
            while let Ok((first, body)) = try_frame(&mut browser) {
                let reply = if first == 0x89 {
                    masked(0xa, &body)
                } else if first == 0x82 && body.first() == Some(&SERVER_FRAME) {
                    ack(u32::from_le_bytes([body[1], body[2], body[3], body[4]]))
                } else {
                    continue;
                };
                if browser.write_all(&reply).is_err() {
                    break;
                }
            }
        });
    }

    fn text(body: &str) -> Vec<u8> {
        let mut message = vec![CLIENT_TEXT];
        message.extend_from_slice(body.as_bytes());
        masked(0x2, &message)
    }

    fn pointer_to(x: u16, y: u16) -> Vec<u8> {
        let mut message = vec![CLIENT_POINTER];
        message.extend_from_slice(&x.to_le_bytes());
        message.extend_from_slice(&y.to_le_bytes());
        masked(0x2, &message)
    }

    fn off_layout(count: usize, distinct: usize) -> String {
        (0..count)
            .map(|index| char::from_u32(0x4e00 + (index % distinct) as u32).unwrap())
            .collect()
    }

    fn typed(text: &str) -> Vec<Seen> {
        text.chars()
            .filter_map(symbol_of)
            .flat_map(|symbol| [Seen::Symbol(symbol, true), Seen::Symbol(symbol, false)])
            .collect()
    }

    fn wait_for(session: &Session, count: usize) -> Vec<Seen> {
        let until = Instant::now() + Duration::from_secs(20);
        loop {
            let seen = session.seen.lock().unwrap().clone();
            if seen.len() >= count || Instant::now() >= until {
                return seen;
            }
            if let Ok(ended) = session.ended.try_recv() {
                panic!("the view ended: {ended:?}");
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn long_typed_text_arrives_in_full_and_before_the_input_sent_after_it() {
        let mut session = open_with(steady(), None);
        let long = "the quick brown fox jumps over the lazy dog\n".repeat(120);
        assert!(long.chars().count() > 5000);
        session.browser.write_all(&text(&long)).unwrap();
        session
            .browser
            .write_all(&masked(0x2, &[CLIENT_KEY, 30, 0, 1]))
            .unwrap();
        answering(&session);
        let mut expected = typed(&long);
        expected.push(Seen::Key(38, true));
        assert_eq!(wait_for(&session, expected.len()), expected);
    }

    #[test]
    fn pointer_moves_during_long_off_layout_text_wait_for_it_and_keep_the_view_open() {
        let mut session = open_with(steady(), Some(layout()));
        let long = off_layout(5000, 100);
        session.browser.write_all(&text(&long)).unwrap();
        session.browser.write_all(&pointer_to(10, 20)).unwrap();
        session.browser.write_all(&pointer_to(30, 40)).unwrap();
        answering(&session);
        std::thread::sleep(Duration::from_secs(1));
        assert_eq!(session.ended.try_recv(), Err(mpsc::TryRecvError::Empty));
        let seen = session.seen.lock().unwrap().clone();
        assert!(seen.len() >= 16, "only {} strokes typed", seen.len());
        assert_eq!(seen, typed(&long)[..seen.len()]);
    }

    #[test]
    fn clicks_and_keys_after_off_layout_text_happen_after_all_of_it() {
        let mut session = open_with(steady(), Some(layout()));
        let mixed: String = off_layout(30, 30)
            .chars()
            .enumerate()
            .flat_map(|(index, character)| [character, if index % 2 == 0 { 'a' } else { 'B' }])
            .collect();
        session.browser.write_all(&text(&mixed)).unwrap();
        session.browser.write_all(&pointer_to(5, 6)).unwrap();
        for message in [[CLIENT_BUTTON, 1, 1], [CLIENT_BUTTON, 1, 0]] {
            session.browser.write_all(&masked(0x2, &message)).unwrap();
        }
        session
            .browser
            .write_all(&masked(0x2, &[CLIENT_KEY, 30, 0, 1]))
            .unwrap();
        answering(&session);
        let mut expected = typed(&mixed);
        expected.extend([
            Seen::Pointer(5, 6),
            Seen::Button(1, true),
            Seen::Button(1, false),
            Seen::Key(38, true),
        ]);
        assert_eq!(wait_for(&session, expected.len()), expected);
    }

    #[test]
    fn a_viewer_with_more_text_waiting_than_allowed_is_let_go() {
        let mut session = open_with(steady(), Some(layout()));
        let part = off_layout(80_000, 1000);
        for _ in 0..4 {
            if session.browser.write_all(&text(&part)).is_err() {
                break;
            }
        }
        answering(&session);
        assert_eq!(
            session.ended.recv_timeout(Duration::from_secs(10)),
            Ok(Err(
                "the viewer sent more text than the desktop can type".to_string()
            ))
        );
    }

    fn watching(access: Access, changing: bool) -> (Session, u8) {
        let settings = Settings {
            fps: 60,
            max_mbps: 100,
            keepalive: KEEPALIVE,
        };
        let mut session = open_as(settings, None, access, changing);
        let (first, hello) = try_frame(&mut session.browser).unwrap();
        assert_eq!((first, hello[0]), (0x82, SERVER_HELLO));
        (session, hello[7])
    }

    impl Session {
        fn send(&mut self, messages: &[Vec<u8>]) {
            for message in messages {
                self.browser.write_all(&masked(0x2, message)).unwrap();
            }
            let start = self.turns.load(Ordering::SeqCst);
            let deadline = Instant::now() + Duration::from_secs(5);
            while self.turns.load(Ordering::SeqCst) < start + 6 {
                assert!(Instant::now() < deadline, "the session stopped turning");
                std::thread::sleep(Duration::from_millis(5));
            }
        }

        fn pause(&mut self, paused: bool) {
            self.browser
                .write_all(&masked(0x2, &[CLIENT_PAUSE, u8::from(paused)]))
                .unwrap();
        }

        fn frames_within(&mut self, wait: Duration) -> usize {
            let deadline = Instant::now() + wait;
            let mut frames = 0;
            loop {
                let left = deadline.saturating_duration_since(Instant::now());
                if left.is_zero() {
                    break;
                }
                self.browser.set_read_timeout(Some(left)).unwrap();
                let Ok((first, body)) = try_frame(&mut self.browser) else {
                    break;
                };
                if first == 0x82 && body.first() == Some(&SERVER_FRAME) {
                    frames += 1;
                    let sequence = u32::from_le_bytes([body[1], body[2], body[3], body[4]]);
                    self.browser.write_all(&ack(sequence)).unwrap();
                }
            }
            frames
        }

        fn close(self) -> Vec<Seen> {
            self.browser.shutdown(std::net::Shutdown::Both).unwrap();
            assert_eq!(
                self.ended.recv_timeout(Duration::from_secs(5)).unwrap(),
                Ok(())
            );
            self.seen.lock().unwrap().clone()
        }
    }

    fn every_input() -> Vec<Vec<u8>> {
        vec![
            vec![CLIENT_POINTER, 5, 0, 7, 0],
            vec![CLIENT_BUTTON, 1, 1],
            vec![CLIENT_BUTTON, 1, 0],
            vec![CLIENT_KEY, 30, 0, 1],
            vec![CLIENT_KEY, 30, 0, 0],
            vec![CLIENT_WHEEL, 0, 1],
            vec![CLIENT_SYMBOL, 0x61, 0, 0, 0, 1],
            vec![CLIENT_SYMBOL, 0x61, 0, 0, 0, 0],
            [&[CLIENT_TEXT][..], b"hi"].concat(),
            [&[CLIENT_CLIPBOARD][..], b"secret"].concat(),
            vec![CLIENT_COPY],
            vec![CLIENT_CHORD, b'v', 0],
        ]
    }

    #[test]
    fn a_stream_with_control_passes_every_kind_of_input_to_the_desktop() {
        let (mut session, flags) = watching(Access::Control, false);
        assert_eq!(flags, HELLO_CHORDS);
        session.send(&every_input());
        let mut expected = vec![
            Seen::Pointer(5, 7),
            Seen::Button(1, true),
            Seen::Button(1, false),
            Seen::Key(38, true),
            Seen::Key(38, false),
            Seen::Button(5, true),
            Seen::Button(5, false),
            Seen::Symbol(0x61, true),
            Seen::Symbol(0x61, false),
        ];
        expected.extend(typed("hi"));
        expected.extend([
            Seen::Offer("secret".into()),
            Seen::ExpectCopy,
            Seen::Chord(b'v', false),
        ]);
        assert_eq!(session.close(), expected);
    }

    #[test]
    fn a_watch_stream_says_so_and_never_reaches_the_desktop_whatever_it_sends() {
        let (mut session, flags) = watching(Access::Watch, false);
        assert_eq!(flags, HELLO_CHORDS | HELLO_WATCH);
        session.send(&every_input());
        let shapes: [&[u8]; 4] = [
            &[],
            &[1, 1, 1, 1, 1, 1, 1, 1],
            &[b'v', 1, 0, 0, 1, 0, 0, 0],
            b"some typed text",
        ];
        let mut sweep: Vec<Vec<u8>> = (0..=u8::MAX)
            .flat_map(|kind| shapes.map(|shape| [&[kind][..], shape].concat()))
            .collect();
        sweep.push(vec![CLIENT_PAUSE, 0]);
        session.send(&sweep);
        assert_eq!(session.close(), Vec::new());
    }

    #[test]
    fn a_watch_stream_still_acknowledges_and_pauses() {
        let (mut session, _) = watching(Access::Watch, true);
        assert!(session.frames_within(Duration::from_millis(300)) > 3);
        session.pause(true);
        session.frames_within(Duration::from_millis(300));
        assert_eq!(session.frames_within(Duration::from_millis(300)), 0);
        session.pause(false);
        assert!(session.frames_within(Duration::from_millis(300)) > 3);
        assert_eq!(session.close(), Vec::new());
    }
}
