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
const LOOK_AHEAD: usize = 512;
const MAX_QUEUED_BYTES: usize = 4 * 1024 * 1024;

#[derive(Clone, Copy)]
pub struct Settings {
    pub fps: u32,
    pub max_mbps: u32,
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
    let mut strokes: VecDeque<Stroke> = VecDeque::new();
    let mut strokes_ready = started;
    let mut typed = started;
    loop {
        writer.flush(&mut stream)?;
        if !reader.fill(&mut stream)? {
            return Ok(());
        }
        while let Some(incoming) = reader.next()? {
            let Message::Binary(data) = incoming else {
                return Ok(());
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
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    const SIZE: usize = 64;

    #[derive(Clone, Default)]
    struct Seen {
        input: Arc<Mutex<Vec<String>>>,
        turns: Arc<AtomicUsize>,
    }

    impl Seen {
        fn note(&self, what: String) -> Result<(), Failure> {
            self.input.lock().unwrap().push(what);
            Ok(())
        }

        fn after_turns(&self, count: usize) {
            let start = self.turns.load(Ordering::SeqCst);
            let deadline = Instant::now() + Duration::from_secs(5);
            while self.turns.load(Ordering::SeqCst) < start + count {
                assert!(Instant::now() < deadline, "the session stopped turning");
                std::thread::sleep(Duration::from_millis(5));
            }
        }
    }

    struct Fake {
        pixels: Vec<u32>,
        quiet: UnixStream,
        _other: UnixStream,
        seen: Seen,
        changing: bool,
    }

    impl Screen for Fake {
        fn size(&self) -> (usize, usize) {
            (SIZE, SIZE)
        }
        fn fd(&self) -> RawFd {
            self.quiet.as_raw_fd()
        }
        fn pixels(&self) -> &[u32] {
            &self.pixels
        }
        fn drain(&mut self) -> Result<bool, Failure> {
            self.seen.turns.fetch_add(1, Ordering::SeqCst);
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
                width: SIZE,
                height: SIZE,
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
            self.seen.note(format!("pointer {x} {y}"))
        }
        fn button(&mut self, button: u8, down: bool) -> Result<(), Failure> {
            self.seen.note(format!("button {button} {down}"))
        }
        fn key(&mut self, keycode: u8, down: bool) -> Result<(), Failure> {
            self.seen.note(format!("key {keycode} {down}"))
        }
        fn symbol(&mut self, symbol: u32, down: bool) -> Result<(), Failure> {
            self.seen.note(format!("symbol {symbol:#x} {down}"))
        }
        fn needs_key(&self, _symbol: u32) -> bool {
            false
        }
        fn bind(&mut self, _symbols: impl Iterator<Item = u32>) -> Result<bool, Failure> {
            Ok(false)
        }
        fn chord(&mut self, letter: u8, shift: bool) -> Result<(), Failure> {
            self.seen.note(format!("chord {} {shift}", letter as char))
        }
        fn offer(&mut self, text: &[u8]) -> Result<(), Failure> {
            self.seen
                .note(format!("offer {}", String::from_utf8_lossy(text)))
        }
        fn expect_copy(&mut self) {
            let _ = self.seen.note("expect copy".into());
        }
        fn copied(&mut self) -> Option<Vec<u8>> {
            None
        }
        fn chords(&self) -> bool {
            true
        }
    }

    fn masked(payload: &[u8]) -> Vec<u8> {
        let mut frame = vec![0x82];
        if payload.len() < 126 {
            frame.push(0x80 | payload.len() as u8);
        } else {
            frame.push(0x80 | 126);
            frame.extend_from_slice(&(payload.len() as u16).to_be_bytes());
        }
        frame.extend_from_slice(&[0, 0, 0, 0]);
        frame.extend_from_slice(payload);
        frame
    }

    fn heard(browser: &mut UnixStream) -> std::io::Result<Vec<u8>> {
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
        let mut payload = vec![0u8; length];
        browser.read_exact(&mut payload)?;
        Ok(payload)
    }

    struct Viewer {
        browser: UnixStream,
        seen: Seen,
        session: std::thread::JoinHandle<Result<(), Failure>>,
        hello: Vec<u8>,
    }

    impl Viewer {
        fn open(access: Access, changing: bool) -> Self {
            let (mut browser, engine) = UnixStream::pair().unwrap();
            let (quiet, other) = UnixStream::pair().unwrap();
            let seen = Seen::default();
            let fake = Fake {
                pixels: vec![0; SIZE * SIZE],
                quiet,
                _other: other,
                seen: seen.clone(),
                changing,
            };
            let settings = Settings {
                fps: 60,
                max_mbps: 100,
            };
            let session = std::thread::spawn(move || run(engine, settings, access, || Ok(fake)));
            let hello = heard(&mut browser).unwrap();
            assert_eq!(hello[0], SERVER_HELLO);
            Self {
                browser,
                seen,
                session,
                hello,
            }
        }

        fn send(&mut self, messages: &[Vec<u8>]) {
            for message in messages {
                self.browser.write_all(&masked(message)).unwrap();
            }
            self.seen.after_turns(6);
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
                let Ok(message) = heard(&mut self.browser) else {
                    break;
                };
                if message[0] == SERVER_FRAME {
                    frames += 1;
                    let mut ack = vec![CLIENT_ACK];
                    ack.extend_from_slice(&message[1..5]);
                    self.browser.write_all(&masked(&ack)).unwrap();
                }
            }
            frames
        }

        fn pause(&mut self, paused: bool) {
            self.browser
                .write_all(&masked(&[CLIENT_PAUSE, u8::from(paused)]))
                .unwrap();
        }

        fn close(self) -> Vec<String> {
            drop(self.browser);
            self.session.join().unwrap().unwrap();
            Arc::try_unwrap(self.seen.input)
                .unwrap()
                .into_inner()
                .unwrap()
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
        let mut viewer = Viewer::open(Access::Control, false);
        assert_eq!(viewer.hello[7], HELLO_CHORDS);
        viewer.send(&every_input());
        assert_eq!(
            viewer.close(),
            [
                "pointer 5 7",
                "button 1 true",
                "button 1 false",
                "key 38 true",
                "key 38 false",
                "button 5 true",
                "button 5 false",
                "symbol 0x61 true",
                "symbol 0x61 false",
                "symbol 0x68 true",
                "symbol 0x68 false",
                "symbol 0x69 true",
                "symbol 0x69 false",
                "offer secret",
                "expect copy",
                "chord v false",
            ]
        );
    }

    #[test]
    fn a_watch_stream_says_so_and_never_reaches_the_desktop_whatever_it_sends() {
        let mut viewer = Viewer::open(Access::Watch, false);
        assert_eq!(viewer.hello[7], HELLO_CHORDS | HELLO_WATCH);
        viewer.send(&every_input());
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
        viewer.send(&sweep);
        assert_eq!(viewer.close(), Vec::<String>::new());
    }

    #[test]
    fn a_watch_stream_still_acknowledges_and_pauses() {
        let mut viewer = Viewer::open(Access::Watch, true);
        assert!(viewer.frames_within(Duration::from_millis(300)) > 3);
        viewer.pause(true);
        viewer.frames_within(Duration::from_millis(300));
        assert_eq!(viewer.frames_within(Duration::from_millis(300)), 0);
        viewer.pause(false);
        assert!(viewer.frames_within(Duration::from_millis(300)) > 3);
        assert_eq!(viewer.close(), Vec::<String>::new());
    }
}
