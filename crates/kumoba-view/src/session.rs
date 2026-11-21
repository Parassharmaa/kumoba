use crate::flow::Flow;
use crate::ws::{Message, Reader, Writer};
use crate::x11::Screen;
use kumoba_view_codec::{Encoder, Rect};
use std::collections::VecDeque;
use std::net::TcpStream;
use std::os::fd::AsRawFd;
use std::time::{Duration, Instant};

type Failure = Box<dyn std::error::Error + Send + Sync>;

pub const SERVER_HELLO: u8 = 1;
pub const SERVER_FRAME: u8 = 2;
const SERVER_CURSOR: u8 = 3;
const SERVER_POINTER: u8 = 4;
const CLIENT_ACK: u8 = 1;
const CLIENT_POINTER: u8 = 2;
const CLIENT_BUTTON: u8 = 3;
const CLIENT_KEY: u8 = 4;
const CLIENT_WHEEL: u8 = 5;
const CLIENT_PAUSE: u8 = 6;
const CLIENT_SYMBOL: u8 = 7;

const STALL: Duration = Duration::from_secs(5);
const POINTER_EVERY: Duration = Duration::from_millis(33);
const IDLE_WAKE: Duration = Duration::from_millis(100);
const MAX_QUEUED_BYTES: usize = 4 * 1024 * 1024;

#[derive(Clone, Copy)]
pub struct Settings {
    pub fps: u32,
    pub max_mbps: u32,
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

pub fn run(mut stream: TcpStream, settings: Settings) -> Result<(), Failure> {
    stream.set_nonblocking(true)?;
    stream.set_nodelay(true)?;
    let mut screen = Screen::open()?;
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
                    screen.pointer(x as i16, y as i16)?;
                }
                Some(&CLIENT_BUTTON) if data.len() >= 3 && (1..=9).contains(&data[1]) => {
                    screen.button(data[1], data[2] != 0)?;
                }
                Some(&CLIENT_KEY) if data.len() >= 4 => {
                    let code = u16::from_le_bytes([data[1], data[2]]);
                    if let Ok(keycode) = u8::try_from(code + 8) {
                        screen.key(keycode, data[3] != 0)?;
                    }
                }
                Some(&CLIENT_WHEEL) if data.len() >= 3 => {
                    let (dx, dy) = (data[1] as i8, data[2] as i8);
                    for (delta, negative, positive) in [(dy, 4u8, 5u8), (dx, 6, 7)] {
                        let button = if delta < 0 { negative } else { positive };
                        for _ in 0..delta.unsigned_abs().min(10) {
                            screen.button(button, true)?;
                            screen.button(button, false)?;
                        }
                    }
                }
                Some(&CLIENT_PAUSE) if data.len() >= 2 => paused = data[1] != 0,
                Some(&CLIENT_SYMBOL) if data.len() >= 6 => {
                    let symbol = u32::from_le_bytes([data[1], data[2], data[3], data[4]]);
                    screen.symbol(symbol, data[5] != 0)?;
                }
                _ => {}
            }
        }
        let now = Instant::now();
        if flight
            .front()
            .is_some_and(|sent| now.duration_since(sent.at) > STALL)
        {
            return Err("the viewer stopped acknowledging frames".into());
        }
        screen.drain()?;
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
        let wanted = dirty || encoder.pending();
        let open = !paused && flight.len() < flow.window() && writer.queued() < MAX_QUEUED_BYTES;
        let writable = writer.queued() > 0;
        if !(wanted && open) {
            let timeout = if paused { None } else { Some(IDLE_WAKE) };
            poll(screen.fd(), stream.as_raw_fd(), writable, timeout)?;
            continue;
        }
        let wait = pace.wait(now);
        if !wait.is_zero() {
            poll(screen.fd(), stream.as_raw_fd(), writable, Some(wait))?;
            continue;
        }
        let Some(budget) = flow.budget(started.elapsed()) else {
            let wait = flow.wait().max(Duration::from_millis(1));
            poll(screen.fd(), stream.as_raw_fd(), writable, Some(wait))?;
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
        let stats = encoder.encode(screen.pixels(), area, &[], budget, &mut payload);
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
