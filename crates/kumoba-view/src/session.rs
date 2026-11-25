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

const STALL: Duration = Duration::from_secs(5);
const POINTER_EVERY: Duration = Duration::from_millis(33);
const IDLE_WAKE: Duration = Duration::from_millis(100);
const KEYMAP_SETTLE: Duration = Duration::from_millis(50);
const KEYMAP_RESETTLE: Duration = Duration::from_millis(150);
const MAX_STROKES: usize = 8192;
const LOOK_AHEAD: usize = 512;
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

enum Stroke {
    Code(u8, bool),
    Symbol(u32, bool),
    Pointer(i16, i16),
    Button(u8, bool),
    Chord(u8, bool),
    Offer(Vec<u8>),
    ExpectCopy,
}

fn queue(strokes: &mut VecDeque<Stroke>, stroke: Stroke) -> Result<(), Failure> {
    if strokes.len() >= MAX_STROKES {
        return Err("the viewer sent input faster than the desktop takes it".into());
    }
    strokes.push_back(stroke);
    Ok(())
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
    hello.push(u8::from(screen.chords()));
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
    let mut strokes: VecDeque<Stroke> = VecDeque::new();
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
                    if let Some(Stroke::Pointer(last_x, last_y)) = strokes.back_mut() {
                        (*last_x, *last_y) = (x as i16, y as i16);
                    } else {
                        queue(&mut strokes, Stroke::Pointer(x as i16, y as i16))?;
                    }
                }
                Some(&CLIENT_BUTTON) if data.len() >= 3 && (1..=9).contains(&data[1]) => {
                    queue(&mut strokes, Stroke::Button(data[1], data[2] != 0))?;
                }
                Some(&CLIENT_KEY) if data.len() >= 4 => {
                    let code = u16::from_le_bytes([data[1], data[2]]);
                    if let Ok(keycode) = u8::try_from(code + 8) {
                        queue(&mut strokes, Stroke::Code(keycode, data[3] != 0))?;
                    }
                }
                Some(&CLIENT_WHEEL) if data.len() >= 3 => {
                    let (dx, dy) = (data[1] as i8, data[2] as i8);
                    for (delta, negative, positive) in [(dy, 4u8, 5u8), (dx, 6, 7)] {
                        let button = if delta < 0 { negative } else { positive };
                        for _ in 0..delta.unsigned_abs().min(10) {
                            queue(&mut strokes, Stroke::Button(button, true))?;
                            queue(&mut strokes, Stroke::Button(button, false))?;
                        }
                    }
                }
                Some(&CLIENT_PAUSE) if data.len() >= 2 => paused = data[1] != 0,
                Some(&CLIENT_SYMBOL) if data.len() >= 6 => {
                    let symbol = u32::from_le_bytes([data[1], data[2], data[3], data[4]]);
                    queue(&mut strokes, Stroke::Symbol(symbol, data[5] != 0))?;
                }
                Some(&CLIENT_TEXT) => {
                    if let Ok(text) = std::str::from_utf8(&data[1..]) {
                        let room = MAX_STROKES.saturating_sub(strokes.len()) / 2;
                        for symbol in text.chars().filter_map(symbol_of).take(room) {
                            strokes.push_back(Stroke::Symbol(symbol, true));
                            strokes.push_back(Stroke::Symbol(symbol, false));
                        }
                    }
                }
                Some(&CLIENT_CLIPBOARD) => queue(&mut strokes, Stroke::Offer(data[1..].to_vec()))?,
                Some(&CLIENT_COPY) => queue(&mut strokes, Stroke::ExpectCopy)?,
                Some(&CLIENT_CHORD) if data.len() >= 3 && b"cvx".contains(&data[1]) => {
                    queue(&mut strokes, Stroke::Chord(data[1], data[2] != 0))?;
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
        while now >= strokes_ready {
            match strokes.front() {
                None => break,
                Some(&Stroke::Code(keycode, down)) => screen.key(keycode, down)?,
                Some(&Stroke::Symbol(symbol, down)) => {
                    if down && screen.needs_key(symbol) {
                        let settled = typed + KEYMAP_RESETTLE;
                        if now < settled {
                            strokes_ready = settled;
                            break;
                        }
                        let ahead =
                            strokes
                                .iter()
                                .take(LOOK_AHEAD)
                                .filter_map(|stroke| match stroke {
                                    &Stroke::Symbol(symbol, true) => Some(symbol),
                                    _ => None,
                                });
                        let reused = screen.bind(ahead)?;
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
                }
                Some(&Stroke::Pointer(x, y)) => screen.pointer(x, y)?,
                Some(&Stroke::Button(button, down)) => screen.button(button, down)?,
                Some(&Stroke::Chord(letter, shift)) => screen.chord(letter, shift)?,
                Some(Stroke::Offer(text)) => screen.offer(text)?,
                Some(Stroke::ExpectCopy) => screen.expect_copy(),
            }
            strokes.pop_front();
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
        let typing = (!strokes.is_empty()).then(|| {
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
    use crate::screen::Cursor;
    use std::io::{Read, Write};
    use std::os::fd::{AsRawFd, RawFd};
    use std::os::unix::net::UnixStream;
    use std::sync::mpsc;

    const SIDE: usize = 64;

    struct Still {
        wake: UnixStream,
        pixels: Vec<u32>,
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
            Ok(false)
        }
        fn resized(&self) -> bool {
            false
        }
        fn grab(&mut self, _full: bool) -> Result<Rect, Failure> {
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
        fn pointer(&mut self, _x: i16, _y: i16) -> Result<(), Failure> {
            Ok(())
        }
        fn button(&mut self, _button: u8, _down: bool) -> Result<(), Failure> {
            Ok(())
        }
        fn key(&mut self, _keycode: u8, _down: bool) -> Result<(), Failure> {
            Ok(())
        }
        fn symbol(&mut self, _symbol: u32, _down: bool) -> Result<(), Failure> {
            Ok(())
        }
        fn needs_key(&self, _symbol: u32) -> bool {
            false
        }
        fn bind(&mut self, _symbols: impl Iterator<Item = u32>) -> Result<bool, Failure> {
            Ok(false)
        }
        fn chord(&mut self, _letter: u8, _shift: bool) -> Result<(), Failure> {
            Ok(())
        }
        fn offer(&mut self, _text: &[u8]) -> Result<(), Failure> {
            Ok(())
        }
        fn expect_copy(&mut self) {}
        fn copied(&mut self) -> Option<Vec<u8>> {
            None
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
        _wake: UnixStream,
    }

    fn start(settings: Settings) -> Session {
        let (browser, engine) = UnixStream::pair().unwrap();
        let (wake, screen_end) = UnixStream::pair().unwrap();
        let (done, ended) = mpsc::channel();
        std::thread::spawn(move || {
            let result = run(engine, settings, || {
                Ok(Still {
                    wake: screen_end,
                    pixels: vec![0x0033_6699; SIDE * SIDE],
                })
            });
            let _ = done.send(result.map_err(|error| error.to_string()));
        });
        Session {
            browser,
            ended,
            _wake: wake,
        }
    }

    fn masked(opcode: u8, body: &[u8]) -> Vec<u8> {
        let mask = [3u8, 1, 4, 1];
        let mut frame = vec![0x80 | opcode, 0x80 | u8::try_from(body.len()).unwrap()];
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
}
