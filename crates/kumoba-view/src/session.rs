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
pub struct Settings {
    pub fps: u32,
    pub max_mbps: u32,
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
