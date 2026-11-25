use crate::mackeys::{self, COMMAND, SHIFT, SHIFT_RIGHT};
use crate::screen::{Cursor, Failure, Screen};
use kumoba_protocol::screen::{
    CLIPBOARD, CLIPBOARD_SET, COPY, CURSOR, DAMAGE, GRAB, GRABBED, HELLO, KEY, LEFT, MAX_SIDE,
    MIDDLE, PASTE, POINTER, Parser, REFUSED, RIGHT, WHEEL, frame, pair, text,
};
use kumoba_view_codec::Rect;
use std::collections::HashMap;
use std::io::{self, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::{Duration, Instant};

const OPEN_WAIT: Duration = Duration::from_secs(10);
const GRAB_WAIT: Duration = Duration::from_secs(5);
const COLOUR: u32 = 0x00ff_ffff;
const LIVELY: u32 = 30;
const STILL_LOOK: Duration = Duration::from_millis(100);
const COPY_WINDOW: Duration = Duration::from_secs(2);
const GATHER: Duration = Duration::from_millis(150);
const NOTHING: Rect = Rect {
    x: 0,
    y: 0,
    width: 0,
    height: 0,
};

fn receive(socket: &UnixStream, wait: bool, parser: &mut Parser) -> io::Result<Option<OwnedFd>> {
    let mut data = [0u8; 4096];
    let mut control = [0u64; 8];
    let mut part = libc::iovec {
        iov_base: data.as_mut_ptr().cast(),
        iov_len: data.len(),
    };
    let mut header: libc::msghdr = unsafe { std::mem::zeroed() };
    header.msg_iov = &raw mut part;
    header.msg_iovlen = 1;
    header.msg_control = control.as_mut_ptr().cast();
    header.msg_controllen = size_of_val(&control) as _;
    let flags = if wait { 0 } else { libc::MSG_DONTWAIT };
    let count = unsafe { libc::recvmsg(socket.as_raw_fd(), &raw mut header, flags) };
    if count < 0 {
        let error = io::Error::last_os_error();
        return match error.kind() {
            io::ErrorKind::Interrupted => Ok(None),
            io::ErrorKind::WouldBlock if !wait => Ok(None),
            io::ErrorKind::WouldBlock => Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "the sandbox's screen stopped answering",
            )),
            _ => Err(error),
        };
    }
    if count == 0 {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "the sandbox closed its screen",
        ));
    }
    parser.push(&data[..count as usize]);
    let mut passed = None;
    let mut entry = unsafe { libc::CMSG_FIRSTHDR(&raw const header) };
    while !entry.is_null() {
        let found = unsafe { &*entry };
        if found.cmsg_level == libc::SOL_SOCKET && found.cmsg_type == libc::SCM_RIGHTS {
            let fd = unsafe { libc::CMSG_DATA(entry).cast::<RawFd>().read_unaligned() };
            passed = Some(unsafe { OwnedFd::from_raw_fd(fd) });
        }
        entry = unsafe { libc::CMSG_NXTHDR(&raw const header, entry) };
    }
    Ok(passed)
}

struct Shared {
    start: *const u32,
    bytes: usize,
}

impl Shared {
    fn map(fd: &OwnedFd, pixels: usize) -> io::Result<Self> {
        let bytes = pixels * 4;
        let mut status: libc::stat = unsafe { std::mem::zeroed() };
        if unsafe { libc::fstat(fd.as_raw_fd(), &raw mut status) } != 0 {
            return Err(io::Error::last_os_error());
        }
        if usize::try_from(status.st_size).unwrap_or(0) < bytes {
            return Err(io::Error::other(
                "the shared screen is smaller than it says",
            ));
        }
        let start = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                bytes,
                libc::PROT_READ,
                libc::MAP_SHARED,
                fd.as_raw_fd(),
                0,
            )
        };
        if start == libc::MAP_FAILED {
            return Err(io::Error::last_os_error());
        }
        Ok(Self {
            start: start.cast(),
            bytes,
        })
    }

    fn pixels(&self) -> &[u32] {
        unsafe { std::slice::from_raw_parts(self.start, self.bytes / 4) }
    }
}

impl Drop for Shared {
    fn drop(&mut self) {
        unsafe { libc::munmap(self.start.cast_mut().cast(), self.bytes) };
    }
}

pub fn changed(old: &mut [u32], new: &[u32], width: usize) -> Rect {
    let (mut top, mut bottom, mut left, mut right) = (usize::MAX, 0, usize::MAX, 0);
    for (row, (was, now)) in old
        .chunks_exact_mut(width)
        .zip(new.chunks_exact(width))
        .enumerate()
    {
        let differs = |(a, b): (&u32, &u32)| *a != *b & COLOUR;
        let Some(first) = was.iter().zip(now).position(differs) else {
            continue;
        };
        let last = was.iter().zip(now).rposition(differs).unwrap_or(first);
        top = top.min(row);
        bottom = row;
        left = left.min(first);
        right = right.max(last);
        for (kept, seen) in was.iter_mut().zip(now) {
            *kept = *seen & COLOUR;
        }
    }
    if top == usize::MAX {
        return NOTHING;
    }
    Rect {
        x: left,
        y: top,
        width: right - left + 1,
        height: bottom - top + 1,
    }
}

pub struct Remote {
    socket: UnixStream,
    parser: Parser,
    shared: Shared,
    width: u16,
    height: u16,
    pixels: Vec<u32>,
    dirty: bool,
    still: u32,
    look: Instant,
    resized: bool,
    at: (u16, u16),
    buttons: u8,
    shifts: [bool; 2],
    pressed: [bool; 2],
    typed: HashMap<u32, u16>,
    down: Vec<u16>,
    pending: String,
    pending_at: Instant,
    copy_until: Option<Instant>,
    copied: Option<Vec<u8>>,
    shape: Option<Cursor>,
}

unsafe impl Send for Remote {}

impl Remote {
    pub fn open(path: &Path) -> Result<Self, Failure> {
        let socket = UnixStream::connect(path).map_err(|error| {
            format!(
                "the sandbox shares no screen: {error}, at {}",
                path.display()
            )
        })?;
        socket.set_read_timeout(Some(OPEN_WAIT))?;
        let mut parser = Parser::default();
        let mut passed = None;
        let (width, height) = loop {
            if let Some(fd) = receive(&socket, true, &mut parser)? {
                passed = Some(fd);
            }
            if let Some((kind, payload)) = parser.take()? {
                if kind == REFUSED {
                    return Err(String::from_utf8_lossy(&payload).into_owned().into());
                }
                if kind != HELLO {
                    return Err("the sandbox's screen did not introduce itself".into());
                }
                break pair(&payload).ok_or("the sandbox's screen sent no size")?;
            }
        };
        if width == 0 || height == 0 || width > MAX_SIDE || height > MAX_SIDE {
            return Err(
                format!("the sandbox's screen has an unusable size {width}x{height}").into(),
            );
        }
        let passed = passed.ok_or("the sandbox's screen shared no memory")?;
        let pixels = usize::from(width) * usize::from(height);
        let shared = Shared::map(&passed, pixels)?;
        socket.set_read_timeout(Some(GRAB_WAIT))?;
        Ok(Self {
            socket,
            parser,
            shared,
            width,
            height,
            pixels: vec![0; pixels],
            dirty: true,
            still: 0,
            look: Instant::now(),
            resized: false,
            at: (0, 0),
            buttons: 0,
            shifts: [false; 2],
            pressed: [false; 2],
            typed: HashMap::new(),
            down: Vec::new(),
            pending: String::new(),
            pending_at: Instant::now(),
            copy_until: None,
            copied: None,
            shape: None,
        })
    }

    fn send(&mut self, kind: u8, payload: &[u8]) -> Result<(), Failure> {
        if kind != GRAB {
            self.still = 0;
            self.look = Instant::now();
        }
        self.socket.write_all(&frame(kind, payload))?;
        Ok(())
    }

    fn looked(&mut self, area: Rect) -> Rect {
        let now = Instant::now();
        if area == NOTHING {
            self.still = self.still.saturating_add(1);
            self.look = if self.still < LIVELY {
                now
            } else {
                now + STILL_LOOK
            };
        } else {
            self.still = 0;
            self.look = now;
        }
        area
    }

    fn press(&mut self, key: u16, down: bool) -> Result<(), Failure> {
        self.down.retain(|held| *held != key);
        if down {
            self.down.push(key);
        }
        self.raw(key, down)
    }

    fn point(&mut self) -> Result<(), Failure> {
        let (x, y) = (self.at.0.to_le_bytes(), self.at.1.to_le_bytes());
        self.send(POINTER, &[x[0], x[1], y[0], y[1], self.buttons])
    }

    fn shift(&mut self, wanted: bool) -> Result<(), Failure> {
        if wanted == (self.pressed != [false; 2]) {
            return Ok(());
        }
        let target = match (wanted, self.shifts) {
            (false, _) => [false; 2],
            (true, [false, false]) => [true, false],
            (true, held) => held,
        };
        self.shifted(target)
    }

    fn shifted(&mut self, target: [bool; 2]) -> Result<(), Failure> {
        for (index, key) in [SHIFT, SHIFT_RIGHT].into_iter().enumerate() {
            if self.pressed[index] != target[index] {
                self.pressed[index] = target[index];
                self.press(key, target[index])?;
            }
        }
        Ok(())
    }

    fn settle(&mut self) -> Result<(), Failure> {
        if self.typed.is_empty() {
            self.shifted(self.shifts)?;
        }
        Ok(())
    }

    fn note(&mut self, kind: u8, payload: Vec<u8>) {
        match kind {
            DAMAGE => self.dirty = true,
            CURSOR => self.shape = shape(&payload),
            CLIPBOARD if self.copy_until.is_some_and(|until| Instant::now() <= until) => {
                self.copy_until = None;
                self.copied = Some(payload);
            }
            _ => {}
        }
    }

    fn flush(&mut self) -> Result<(), Failure> {
        if self.pending.is_empty() {
            return Ok(());
        }
        let typed = std::mem::take(&mut self.pending);
        let held = self.down.clone();
        for key in held.iter().rev() {
            self.raw(*key, false)?;
        }
        self.send(PASTE, typed.as_bytes())?;
        for key in held {
            self.raw(key, true)?;
        }
        Ok(())
    }

    fn raw(&mut self, key: u16, down: bool) -> Result<(), Failure> {
        let code = key.to_le_bytes();
        self.send(KEY, &[code[0], code[1], u8::from(down)])
    }

    fn watch_copy(&mut self) -> Result<(), Failure> {
        self.copy_until = Some(Instant::now() + COPY_WINDOW);
        self.copied = None;
        self.send(COPY, &[])
    }
}

fn shape(payload: &[u8]) -> Option<Cursor> {
    let (width, height) = pair(payload)?;
    let (hot_x, hot_y) = pair(payload.get(4..)?)?;
    let rgba = payload.get(8..)?.to_vec();
    (rgba.len() == usize::from(width) * usize::from(height) * 4).then_some(Cursor {
        width,
        height,
        hot_x,
        hot_y,
        rgba,
    })
}

impl Screen for Remote {
    fn size(&self) -> (usize, usize) {
        (usize::from(self.width), usize::from(self.height))
    }

    fn fd(&self) -> RawFd {
        self.socket.as_raw_fd()
    }

    fn pixels(&self) -> &[u32] {
        &self.pixels
    }

    fn drain(&mut self) -> Result<bool, Failure> {
        if !self.pending.is_empty() && self.pending_at.elapsed() >= GATHER {
            self.flush()?;
        }
        loop {
            while let Some((kind, payload)) = self.parser.take()? {
                self.note(kind, payload);
            }
            let mut probe = [libc::pollfd {
                fd: self.socket.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            }];
            if unsafe { libc::poll(probe.as_mut_ptr(), 1, 0) } <= 0 {
                let gathered = !self.pending.is_empty() && self.pending_at.elapsed() >= GATHER;
                return Ok(self.dirty || gathered || Instant::now() >= self.look);
            }
            receive(&self.socket, false, &mut self.parser)?;
        }
    }

    fn resized(&self) -> bool {
        self.resized
    }

    fn grab(&mut self, full: bool) -> Result<Rect, Failure> {
        self.dirty = false;
        self.send(GRAB, &[])?;
        let size = loop {
            match self.parser.take()? {
                Some((GRABBED, payload)) => break pair(&payload),
                Some((kind, payload)) => self.note(kind, payload),
                None => {
                    receive(&self.socket, true, &mut self.parser)?;
                }
            }
        };
        if size != Some((self.width, self.height)) {
            self.resized = true;
            return Ok(NOTHING);
        }
        let width = usize::from(self.width);
        if full {
            for (kept, seen) in self.pixels.iter_mut().zip(self.shared.pixels()) {
                *kept = *seen & COLOUR;
            }
            return Ok(Rect {
                x: 0,
                y: 0,
                width,
                height: usize::from(self.height),
            });
        }
        let area = changed(&mut self.pixels, self.shared.pixels(), width);
        Ok(self.looked(area))
    }

    fn moves(&mut self) -> Vec<(i32, i32)> {
        Vec::new()
    }

    fn cursor(&mut self) -> Result<Option<Cursor>, Failure> {
        Ok(self.shape.take())
    }

    fn position(&mut self) -> Result<(u16, u16), Failure> {
        Ok(self.at)
    }

    fn pointer(&mut self, x: i16, y: i16) -> Result<(), Failure> {
        self.flush()?;
        self.at = (x.max(0) as u16, y.max(0) as u16);
        self.point()
    }

    fn button(&mut self, button: u8, down: bool) -> Result<(), Failure> {
        self.flush()?;
        let bit = match button {
            1 => LEFT,
            2 => MIDDLE,
            3 => RIGHT,
            4..=7 if down => {
                let (dx, dy): (i16, i16) = match button {
                    4 => (0, 1),
                    5 => (0, -1),
                    6 => (1, 0),
                    _ => (-1, 0),
                };
                let (dx, dy) = (dx.to_le_bytes(), dy.to_le_bytes());
                return self.send(WHEEL, &[dx[0], dx[1], dy[0], dy[1]]);
            }
            _ => return Ok(()),
        };
        if down {
            self.buttons |= bit;
        } else {
            self.buttons &= !bit;
        }
        self.point()
    }

    fn key(&mut self, keycode: u8, down: bool) -> Result<(), Failure> {
        self.flush()?;
        let Some(key) = mackeys::for_code(u16::from(keycode).saturating_sub(8)) else {
            return Ok(());
        };
        let side = match key {
            SHIFT => 0,
            SHIFT_RIGHT => 1,
            _ => return self.press(key, down),
        };
        self.shifts[side] = down;
        self.settle()
    }

    fn symbol(&mut self, symbol: u32, down: bool) -> Result<(), Failure> {
        if !down {
            if let Some(key) = self.typed.remove(&symbol) {
                self.press(key, false)?;
            }
            return self.settle();
        }
        let character = match symbol {
            0..=0xff => char::from_u32(symbol),
            0x0100_0000.. => char::from_u32(symbol - 0x0100_0000),
            _ => None,
        }
        .filter(|character| !character.is_control());
        let layout = mackeys::for_symbol(symbol);
        let gathering = !self.pending.is_empty() && character.is_some();
        let (Some((key, shifted)), false) = (layout, gathering) else {
            if let Some(character) = character {
                self.pending.push(character);
                self.pending_at = Instant::now();
            }
            return Ok(());
        };
        self.flush()?;
        if let Some(held) = self.typed.insert(symbol, key) {
            self.press(held, false)?;
        }
        self.shift(shifted)?;
        self.press(key, true)
    }

    fn needs_key(&self, _symbol: u32) -> bool {
        false
    }

    fn bind(&mut self, _symbols: impl Iterator<Item = u32>) -> Result<bool, Failure> {
        Ok(false)
    }

    fn chord(&mut self, letter: u8, shift: bool) -> Result<(), Failure> {
        let Some((key, _)) = mackeys::for_symbol(u32::from(letter)) else {
            return Ok(());
        };
        self.flush()?;
        if letter != b'v' {
            self.watch_copy()?;
        }
        self.press(COMMAND, true)?;
        self.shift(shift)?;
        self.press(key, true)?;
        self.press(key, false)?;
        self.settle()?;
        self.press(COMMAND, false)
    }

    fn offer(&mut self, offered: &[u8]) -> Result<(), Failure> {
        self.flush()?;
        self.send(CLIPBOARD_SET, text(offered))
    }

    fn expect_copy(&mut self) {
        let _ = self.watch_copy();
    }

    fn copied(&mut self) -> Option<Vec<u8>> {
        self.copied.take()
    }

    fn chords(&self) -> bool {
        true
    }
}

impl Drop for Remote {
    fn drop(&mut self) {
        for key in std::mem::take(&mut self.down) {
            let _ = self.press(key, false);
        }
        if self.buttons != 0 {
            self.buttons = 0;
            let _ = self.point();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;
    use std::os::unix::net::UnixListener;

    const WIDTH: u16 = 4;
    const HEIGHT: u16 = 3;

    struct Sandbox {
        peer: UnixStream,
        memory: std::fs::File,
        parser: Parser,
        path: std::path::PathBuf,
    }

    impl Sandbox {
        fn paint(&mut self, pixels: &[u32]) {
            use std::io::{Seek, SeekFrom};
            let bytes: Vec<u8> = pixels
                .iter()
                .flat_map(|pixel| pixel.to_ne_bytes())
                .collect();
            self.memory.seek(SeekFrom::Start(0)).unwrap();
            self.memory.write_all(&bytes).unwrap();
        }

        fn say(&mut self, kind: u8, payload: &[u8]) {
            self.peer.write_all(&frame(kind, payload)).unwrap();
        }

        fn heard(&mut self) -> (u8, Vec<u8>) {
            let mut chunk = [0u8; 64];
            loop {
                if let Some(message) = self.parser.take().unwrap() {
                    return message;
                }
                let count = self.peer.read(&mut chunk).unwrap();
                assert!(count > 0, "the engine hung up");
                self.parser.push(&chunk[..count]);
            }
        }

        fn keys(&mut self, count: usize) -> Vec<(u16, bool)> {
            (0..count)
                .map(|_| {
                    let (kind, payload) = self.heard();
                    assert_eq!(kind, KEY);
                    (
                        u16::from_le_bytes([payload[0], payload[1]]),
                        payload[2] != 0,
                    )
                })
                .collect()
        }
    }

    impl Drop for Sandbox {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.path);
            let _ = std::fs::remove_file(self.path.with_extension("pixels"));
        }
    }

    fn pass(socket: &UnixStream, message: &[u8], fd: RawFd) {
        let mut part = libc::iovec {
            iov_base: message.as_ptr().cast_mut().cast(),
            iov_len: message.len(),
        };
        let mut control = [0u64; 4];
        let mut header: libc::msghdr = unsafe { std::mem::zeroed() };
        header.msg_iov = &raw mut part;
        header.msg_iovlen = 1;
        header.msg_control = control.as_mut_ptr().cast();
        header.msg_controllen = unsafe { libc::CMSG_SPACE(4) } as _;
        unsafe {
            let entry = libc::CMSG_FIRSTHDR(&raw const header);
            (*entry).cmsg_level = libc::SOL_SOCKET;
            (*entry).cmsg_type = libc::SCM_RIGHTS;
            (*entry).cmsg_len = libc::CMSG_LEN(4) as _;
            libc::CMSG_DATA(entry).cast::<RawFd>().write_unaligned(fd);
            assert!(libc::sendmsg(socket.as_raw_fd(), &raw const header, 0) > 0);
        }
    }

    fn connected(name: &str, size: (u16, u16)) -> (Result<Remote, Failure>, Sandbox) {
        offered(name, size, (WIDTH, HEIGHT))
    }

    fn offered(
        name: &str,
        size: (u16, u16),
        shared: (u16, u16),
    ) -> (Result<Remote, Failure>, Sandbox) {
        let path = std::env::temp_dir().join(format!("kv-{}-{name}.sock", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let listener = UnixListener::bind(&path).unwrap();
        let memory = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(true)
            .open(path.with_extension("pixels"))
            .unwrap();
        memory
            .set_len(u64::from(shared.0) * u64::from(shared.1) * 4)
            .unwrap();
        let fd = memory.as_raw_fd();
        let greeting = frame(HELLO, &kumoba_protocol::screen::size(size.0, size.1));
        let accept = std::thread::spawn(move || {
            let (peer, _) = listener.accept().unwrap();
            pass(&peer, &greeting, fd);
            peer
        });
        let remote = Remote::open(&path);
        let peer = accept.join().unwrap();
        (
            remote,
            Sandbox {
                peer,
                memory,
                parser: Parser::default(),
                path,
            },
        )
    }

    fn answering(sandbox: &mut Sandbox, size: (u16, u16)) {
        assert_eq!(sandbox.heard().0, GRAB);
        sandbox.say(GRABBED, &kumoba_protocol::screen::size(size.0, size.1));
    }

    #[test]
    fn a_grab_returns_the_shared_pixels_and_then_only_what_changed() {
        let (remote, mut sandbox) = connected("grab", (WIDTH, HEIGHT));
        let mut remote = remote.unwrap();
        assert_eq!(remote.size(), (4, 3));
        let mut picture = vec![5u32; 12];
        sandbox.paint(&picture);
        let grabbing = std::thread::spawn(move || {
            let first = remote.grab(true).unwrap();
            let seen = remote.pixels().to_vec();
            let told = remote.dirty;
            (remote, first, seen, told)
        });
        answering(&mut sandbox, (WIDTH, HEIGHT));
        let (mut remote, first, seen, told) = grabbing.join().unwrap();
        assert_eq!((first.width, first.height), (4, 3));
        assert_eq!(seen, picture);
        assert!(!told);
        picture[6] = 9;
        sandbox.paint(&picture);
        sandbox.say(DAMAGE, &[]);
        let grabbing = std::thread::spawn(move || {
            while !remote.dirty {
                remote.drain().unwrap();
                std::thread::sleep(Duration::from_millis(5));
            }
            let area = remote.grab(false).unwrap();
            (area, remote.pixels().to_vec())
        });
        answering(&mut sandbox, (WIDTH, HEIGHT));
        let (area, seen) = grabbing.join().unwrap();
        assert_eq!(
            area,
            Rect {
                x: 2,
                y: 1,
                width: 1,
                height: 1
            }
        );
        assert_eq!(seen, picture);
    }

    #[test]
    fn a_screen_that_changed_size_ends_the_session_instead_of_being_read() {
        let (remote, mut sandbox) = connected("resize", (WIDTH, HEIGHT));
        let mut remote = remote.unwrap();
        let grabbing = std::thread::spawn(move || {
            let area = remote.grab(true).unwrap();
            (area, remote.resized())
        });
        answering(&mut sandbox, (WIDTH * 2, HEIGHT));
        let (area, resized) = grabbing.join().unwrap();
        assert_eq!(area, NOTHING);
        assert!(resized);
    }

    #[test]
    fn a_screen_larger_than_its_memory_is_refused() {
        let (remote, _sandbox) = connected("short", (WIDTH * 2, HEIGHT * 2));
        assert!(remote.is_err());
    }

    fn started(name: &str, size: (u16, u16)) -> (Result<Remote, Failure>, Vec<u8>) {
        let (remote, _sandbox) = offered(name, size, size);
        let remote = remote.unwrap();
        let (mut browser, mut engine) = UnixStream::pair().unwrap();
        let starting = std::thread::spawn(move || {
            let started = crate::session::start(&mut engine, move || Ok(remote));
            drop(engine);
            started
        });
        let mut said = Vec::new();
        browser.read_to_end(&mut said).unwrap();
        drop(browser);
        (starting.join().unwrap(), said)
    }

    #[test]
    fn a_screen_wider_or_taller_than_the_live_view_shows_is_refused_before_hello() {
        for size in [(4097, 1), (1, 4097), (5120, 1440)] {
            let (started, said) = started("large", size);
            let error = started.err().unwrap().to_string();
            let expected = format!(
                "the desktop is {}x{} pixels; the live view shows at most 4096x4096",
                size.0, size.1
            );
            assert_eq!(error, expected);
            assert_eq!(said[..4], [0x88, said[1], 0x03, 0xf3]);
            let mut reason = expected.clone();
            reason.replace_range(..1, "T");
            assert_eq!(std::str::from_utf8(&said[4..]).unwrap(), reason);
        }
    }

    #[test]
    fn a_screen_up_to_the_live_view_limit_starts_without_a_word() {
        for size in [(4096, 1), (1, 4096)] {
            let (started, said) = started("widest", size);
            assert_eq!(
                started.unwrap().size(),
                (usize::from(size.0), usize::from(size.1))
            );
            assert!(said.is_empty());
        }
    }

    #[test]
    fn typed_characters_arrive_as_keys_with_the_shift_they_need() {
        let (remote, mut sandbox) = connected("keys", (WIDTH, HEIGHT));
        let mut remote = remote.unwrap();
        remote.symbol(u32::from('a'), true).unwrap();
        remote.symbol(u32::from('a'), false).unwrap();
        assert_eq!(sandbox.keys(2), [(0, true), (0, false)]);
        remote.symbol(u32::from('?'), true).unwrap();
        remote.symbol(u32::from('?'), false).unwrap();
        assert_eq!(
            sandbox.keys(4),
            [(SHIFT, true), (44, true), (44, false), (SHIFT, false)]
        );
        remote.chord(b'v', false).unwrap();
        assert_eq!(
            sandbox.keys(4),
            [(COMMAND, true), (9, true), (9, false), (COMMAND, false)]
        );
    }

    #[test]
    fn a_held_shift_is_lifted_only_while_a_character_that_forbids_it_is_down() {
        let (remote, mut sandbox) = connected("shift", (WIDTH, HEIGHT));
        let mut remote = remote.unwrap();
        remote.key(54 + 8, true).unwrap();
        remote.symbol(u32::from('/'), true).unwrap();
        assert_eq!(
            sandbox.keys(3),
            [(SHIFT_RIGHT, true), (SHIFT_RIGHT, false), (44, true)]
        );
        remote.symbol(u32::from('/'), false).unwrap();
        assert_eq!(sandbox.keys(2), [(44, false), (SHIFT_RIGHT, true)]);
        remote.symbol(u32::from('A'), true).unwrap();
        remote.key(54 + 8, false).unwrap();
        remote.symbol(u32::from('A'), false).unwrap();
        assert_eq!(
            sandbox.keys(3),
            [(0, true), (0, false), (SHIFT_RIGHT, false)]
        );
    }

    #[test]
    fn a_change_announced_while_a_grab_is_answered_is_not_forgotten() {
        let (remote, mut sandbox) = connected("wakeup", (WIDTH, HEIGHT));
        let mut remote = remote.unwrap();
        let grabbing = std::thread::spawn(move || {
            remote.grab(true).unwrap();
            remote.drain().unwrap();
            let pending = remote.dirty;
            (remote, pending)
        });
        assert_eq!(sandbox.heard().0, GRAB);
        sandbox.say(DAMAGE, &[]);
        sandbox.say(GRABBED, &kumoba_protocol::screen::size(WIDTH, HEIGHT));
        let (mut remote, pending) = grabbing.join().unwrap();
        assert!(pending);
        let grabbing = std::thread::spawn(move || {
            remote.grab(false).unwrap();
            remote.drain().unwrap();
            remote.dirty
        });
        answering(&mut sandbox, (WIDTH, HEIGHT));
        assert!(!grabbing.join().unwrap());
    }

    #[test]
    fn a_screen_that_changes_without_saying_so_is_still_seen_and_a_still_one_is_looked_at_less() {
        let (remote, mut sandbox) = connected("still", (WIDTH, HEIGHT));
        let mut remote = remote.unwrap();
        let mut picture = vec![1u32; 12];
        sandbox.paint(&picture);
        let (asked, answered) = std::sync::mpsc::channel::<()>();
        let sandbox = std::thread::spawn(move || {
            let mut looks = 0;
            while sandbox.heard().0 == GRAB {
                looks += 1;
                if looks == 2 {
                    picture[3] = 7;
                    sandbox.paint(&picture);
                }
                sandbox.say(GRABBED, &kumoba_protocol::screen::size(WIDTH, HEIGHT));
                if answered.recv().is_err() {
                    break;
                }
            }
            sandbox
        });
        let grab = |remote: &mut Remote, full: bool| {
            let area = remote.grab(full).unwrap();
            asked.send(()).unwrap();
            area
        };
        grab(&mut remote, true);
        assert!(remote.drain().unwrap());
        assert_eq!(grab(&mut remote, false).width, 1);
        for _ in 0..LIVELY {
            assert!(remote.drain().unwrap());
            assert_eq!(grab(&mut remote, false), NOTHING);
        }
        assert!(!remote.drain().unwrap());
        std::thread::sleep(STILL_LOOK);
        assert!(remote.drain().unwrap());
        assert_eq!(grab(&mut remote, false), NOTHING);
        assert!(!remote.drain().unwrap());
        remote.pointer(1, 1).unwrap();
        assert!(remote.drain().unwrap());
        drop(remote);
        drop(asked);
        drop(sandbox);
    }

    #[test]
    fn a_sandbox_that_refuses_says_why() {
        let path = std::env::temp_dir().join(format!("kv-{}-refuse.sock", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let listener = UnixListener::bind(&path).unwrap();
        let accept = std::thread::spawn(move || {
            let (mut peer, _) = listener.accept().unwrap();
            peer.write_all(&frame(REFUSED, b"no desktop session"))
                .unwrap();
        });
        let refusal = Remote::open(&path).err().map(|error| error.to_string());
        accept.join().unwrap();
        let _ = std::fs::remove_file(&path);
        assert_eq!(refusal.as_deref(), Some("no desktop session"));
    }

    #[test]
    fn a_run_of_text_that_starts_off_the_layout_is_typed_through_the_clipboard_in_one_go() {
        let (remote, mut sandbox) = connected("pasted", (WIDTH, HEIGHT));
        let mut remote = remote.unwrap();
        for (symbol, down) in [
            (u32::from('a'), true),
            (u32::from('a'), false),
            (0xe9, true),
            (0xe9, false),
            (0x0100_0000 + 0x65e5, true),
            (0x0100_0000 + 0x65e5, false),
            (u32::from('b'), true),
            (u32::from('b'), false),
            (0xfc, true),
            (0xfc, false),
        ] {
            remote.symbol(symbol, down).unwrap();
        }
        remote.drain().unwrap();
        assert_eq!(sandbox.keys(2), [(0, true), (0, false)]);
        std::thread::sleep(GATHER);
        remote.drain().unwrap();
        assert_eq!(sandbox.heard(), (PASTE, "é日bü".as_bytes().to_vec()));
        remote.symbol(0xff0d, true).unwrap();
        remote.symbol(0xff0d, false).unwrap();
        remote.symbol(0x0100_0000 + 0x3042, true).unwrap();
        remote.symbol(0xff0d, true).unwrap();
        remote.symbol(0xff0d, false).unwrap();
        remote.symbol(0x0a, true).unwrap();
        assert_eq!(sandbox.keys(2), [(36, true), (36, false)]);
        assert_eq!(sandbox.heard(), (PASTE, "あ".as_bytes().to_vec()));
        assert_eq!(sandbox.keys(2), [(36, true), (36, false)]);
    }

    #[test]
    fn keys_held_down_are_lifted_while_text_is_pasted_and_pressed_again_after() {
        let (remote, mut sandbox) = connected("held", (WIDTH, HEIGHT));
        let mut remote = remote.unwrap();
        remote.key(42 + 8, true).unwrap();
        remote.symbol(0xdc, true).unwrap();
        remote.symbol(0xdc, false).unwrap();
        std::thread::sleep(GATHER);
        remote.drain().unwrap();
        assert_eq!(sandbox.keys(2), [(SHIFT, true), (SHIFT, false)]);
        assert_eq!(sandbox.heard(), (PASTE, "Ü".as_bytes().to_vec()));
        assert_eq!(sandbox.keys(1), [(SHIFT, true)]);
    }

    #[test]
    fn a_paste_offers_the_text_and_a_copy_brings_back_only_what_arrives_in_time() {
        let (remote, mut sandbox) = connected("clipboard", (WIDTH, HEIGHT));
        let mut remote = remote.unwrap();
        assert!(remote.chords());
        remote.offer("from the host".as_bytes()).unwrap();
        remote.chord(b'v', false).unwrap();
        assert_eq!(sandbox.heard(), (CLIPBOARD_SET, b"from the host".to_vec()));
        assert_eq!(
            sandbox.keys(4),
            [(COMMAND, true), (9, true), (9, false), (COMMAND, false)]
        );
        sandbox.say(CLIPBOARD, b"not asked for");
        std::thread::sleep(Duration::from_millis(50));
        remote.drain().unwrap();
        assert_eq!(remote.copied(), None);
        remote.chord(b'c', false).unwrap();
        assert_eq!(sandbox.heard(), (COPY, Vec::new()));
        assert_eq!(
            sandbox.keys(4),
            [(COMMAND, true), (8, true), (8, false), (COMMAND, false)]
        );
        sandbox.say(CLIPBOARD, "copied in the sandbox".as_bytes());
        std::thread::sleep(Duration::from_millis(50));
        remote.drain().unwrap();
        assert_eq!(remote.copied(), Some(b"copied in the sandbox".to_vec()));
        sandbox.say(CLIPBOARD, b"a second answer");
        std::thread::sleep(Duration::from_millis(50));
        remote.drain().unwrap();
        assert_eq!(remote.copied(), None);
    }

    #[test]
    fn the_sandbox_pointer_shape_reaches_the_viewer_once() {
        let (remote, mut sandbox) = connected("shape", (WIDTH, HEIGHT));
        let mut remote = remote.unwrap();
        let rgba = vec![200u8; 2 * 3 * 4];
        sandbox
            .peer
            .write_all(&kumoba_protocol::screen::cursor(2, 3, 1, 2, &rgba).unwrap())
            .unwrap();
        sandbox.say(CURSOR, &[9, 9]);
        std::thread::sleep(Duration::from_millis(50));
        remote.drain().unwrap();
        assert!(remote.cursor().unwrap().is_none());
        sandbox
            .peer
            .write_all(&kumoba_protocol::screen::cursor(2, 3, 1, 2, &rgba).unwrap())
            .unwrap();
        std::thread::sleep(Duration::from_millis(50));
        remote.drain().unwrap();
        let shown = remote.cursor().unwrap().unwrap();
        assert_eq!(
            (shown.width, shown.height, shown.hot_x, shown.hot_y),
            (2, 3, 1, 2)
        );
        assert_eq!(shown.rgba, rgba);
        assert!(remote.cursor().unwrap().is_none());
    }

    #[test]
    fn pointer_buttons_and_wheel_arrive_and_everything_held_is_released_at_the_end() {
        let (remote, mut sandbox) = connected("pointer", (WIDTH, HEIGHT));
        let mut remote = remote.unwrap();
        remote.pointer(3, 2).unwrap();
        remote.button(1, true).unwrap();
        remote.button(5, true).unwrap();
        remote.key(30 + 8, true).unwrap();
        assert_eq!(sandbox.heard(), (POINTER, vec![3, 0, 2, 0, 0]));
        assert_eq!(sandbox.heard(), (POINTER, vec![3, 0, 2, 0, LEFT]));
        assert_eq!(sandbox.heard(), (WHEEL, vec![0, 0, 0xff, 0xff]));
        assert_eq!(sandbox.keys(1), [(0, true)]);
        drop(remote);
        assert_eq!(sandbox.keys(1), [(0, false)]);
        assert_eq!(sandbox.heard(), (POINTER, vec![3, 0, 2, 0, 0]));
    }

    #[test]
    fn an_unchanged_screen_reports_nothing() {
        let new = vec![7u32; 12];
        let mut old = new.clone();
        assert_eq!(changed(&mut old, &new, 4), NOTHING);
    }

    #[test]
    fn only_colour_counts_so_the_unused_byte_of_a_pixel_never_reaches_the_encoder() {
        let mut old = vec![0x0012_3456u32; 8];
        let new = vec![0xff12_3456u32; 8];
        assert_eq!(changed(&mut old, &new, 4), NOTHING);
        let mut new = new;
        new[5] = 0x8065_4321;
        let area = changed(&mut old, &new, 4);
        assert_eq!((area.x, area.y, area.width, area.height), (1, 1, 1, 1));
        assert!(old.iter().all(|pixel| pixel >> 24 == 0));
        assert_eq!(old[5], 0x0065_4321);
    }

    #[test]
    fn the_changed_area_bounds_every_changed_pixel_and_the_copy_catches_up() {
        let mut old = vec![0u32; 8 * 6];
        let mut new = old.clone();
        new[8 + 5] = 1;
        new[4 * 8 + 2] = 2;
        let area = changed(&mut old, &new, 8);
        assert_eq!(
            area,
            Rect {
                x: 2,
                y: 1,
                width: 4,
                height: 4
            }
        );
        assert_eq!(old, new);
        assert_eq!(changed(&mut old, &new, 8), NOTHING);
    }
}
