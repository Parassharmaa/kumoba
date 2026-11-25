use crate::clipboard::Clipboard;
use crate::hints::Hints;
use crate::keymap::{Keymap, Place, SHIFT_LEFT, SHIFT_RIGHT};
use crate::screen::{Cursor, Failure, Screen};
use kumoba_view_codec::Rect;
use std::collections::HashMap;
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::sync::{Mutex, MutexGuard, OnceLock, PoisonError};
use x11rb::connection::Connection;
use x11rb::errors::ConnectionError;
use x11rb::protocol::{Event, damage, shm, xfixes, xproto, xtest};
use x11rb::rust_connection::RustConnection;

static KEYS: OnceLock<Mutex<Keymap>> = OnceLock::new();

const CONTROL_LEFT: u32 = 0xffe3;
const TERMINALS: [&str; 7] = [
    "term",
    "konsole",
    "kitty",
    "alacritty",
    "tilix",
    "foot",
    "wezterm",
];

fn read_keys(
    conn: &RustConnection,
    root: xproto::Window,
    codes: (u8, u8),
) -> Result<Keymap, Failure> {
    let mapping = xproto::get_keyboard_mapping(conn, codes.0, codes.1 - codes.0 + 1)?.reply()?;
    let marker = xproto::intern_atom(conn, false, b"KUMOBA_VIEW_KEYS")?
        .reply()?
        .atom;
    let owned = xproto::get_property(conn, false, root, marker, xproto::AtomEnum::CARDINAL, 0, 64)?
        .reply()?
        .value;
    let layout = Keymap::new(
        codes.0,
        usize::from(mapping.keysyms_per_keycode),
        &mapping.keysyms,
        &owned,
    );
    if owned.is_empty() {
        xproto::change_property(
            conn,
            xproto::PropMode::REPLACE,
            root,
            marker,
            xproto::AtomEnum::CARDINAL,
            8,
            layout.spare().len() as u32,
            layout.spare(),
        )?
        .check()?;
    }
    Ok(layout)
}

pub struct X11 {
    conn: RustConnection,
    root: xproto::Window,
    width: u16,
    height: u16,
    segment: shm::Seg,
    map: *mut u32,
    damage: damage::Damage,
    region: xfixes::Region,
    dirty: bool,
    cursor_changed: bool,
    keys: &'static Mutex<Keymap>,
    codes: (u8, u8),
    shift: [u8; 2],
    shifted: [bool; 2],
    held: HashMap<u32, u8>,
    keys_down: Vec<u8>,
    buttons_down: Vec<u8>,
    hints: Hints,
    resized: bool,
    clipboard: Clipboard,
}

const MAX_CURSOR: u16 = 128;

unsafe impl Send for X11 {}

fn last_error() -> Failure {
    io::Error::last_os_error().into()
}

fn extension<T>(asked: Result<T, ConnectionError>, name: &str) -> Result<T, Failure> {
    asked.map_err(|error| match error {
        ConnectionError::UnsupportedExtension => {
            format!("the X display has no {name} extension, which the live view needs").into()
        }
        other => other.into(),
    })
}

impl X11 {
    pub fn open() -> Result<Self, Failure> {
        let (conn, number) = RustConnection::connect(None)
            .map_err(|error| format!("could not connect to the X display: {error}"))?;
        let setup = conn.setup();
        let screen = &setup.roots[number];
        let (width, height, root) = (screen.width_in_pixels, screen.height_in_pixels, screen.root);
        let format = setup
            .pixmap_formats
            .iter()
            .find(|format| format.depth == screen.root_depth)
            .ok_or("the display has no pixmap format for its depth")?;
        if format.bits_per_pixel != 32 {
            return Err(format!(
                "the X display has depth {}; the live view needs depth 24, as in Xvfb -screen 0 1920x1080x24",
                screen.root_depth
            )
            .into());
        }
        if setup.image_byte_order != xproto::ImageOrder::LSB_FIRST {
            return Err(
                "the X display sends big-endian pixels; the live view needs little-endian ones"
                    .into(),
            );
        }
        extension(shm::query_version(&conn), "MIT-SHM")?.reply()?;
        extension(xfixes::query_version(&conn, 5, 0), "XFIXES")?.reply()?;
        extension(damage::query_version(&conn, 1, 1), "DAMAGE")?.reply()?;
        extension(xtest::get_version(&conn, 2, 2), "XTEST")?.reply()?;
        let size = usize::from(width) * usize::from(height) * 4;
        let fd = unsafe { libc::memfd_create(c"kumoba-view".as_ptr(), libc::MFD_CLOEXEC) };
        if fd < 0 {
            return Err(last_error());
        }
        let fd = unsafe { OwnedFd::from_raw_fd(fd) };
        if unsafe { libc::ftruncate(fd.as_raw_fd(), size as libc::off_t) } != 0 {
            return Err(last_error());
        }
        let map = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                size,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_SHARED,
                fd.as_raw_fd(),
                0,
            )
        };
        if map == libc::MAP_FAILED {
            return Err(last_error());
        }
        let codes = (setup.min_keycode, setup.max_keycode);
        if KEYS.get().is_none() {
            let _ = KEYS.set(Mutex::new(read_keys(&conn, root, codes)?));
        }
        let keys = KEYS.get().ok_or("the keyboard layout could not be read")?;
        let shift = {
            let layout = keys.lock().unwrap_or_else(PoisonError::into_inner);
            [
                layout.code(SHIFT_LEFT).unwrap_or(0),
                layout.code(SHIFT_RIGHT).unwrap_or(0),
            ]
        };
        let clipboard = Clipboard::new(&conn, root)?;
        let segment = conn.generate_id()?;
        shm::attach_fd(&conn, segment, fd, false)?.check()?;
        let damage = conn.generate_id()?;
        damage::create(&conn, damage, root, damage::ReportLevel::NON_EMPTY)?.check()?;
        let region = conn.generate_id()?;
        xfixes::create_region(&conn, region, &[])?.check()?;
        xfixes::select_cursor_input(&conn, root, xfixes::CursorNotifyMask::DISPLAY_CURSOR)?
            .check()?;
        xproto::change_window_attributes(
            &conn,
            root,
            &xproto::ChangeWindowAttributesAux::new().event_mask(
                xproto::EventMask::SUBSTRUCTURE_NOTIFY | xproto::EventMask::STRUCTURE_NOTIFY,
            ),
        )?
        .check()?;
        conn.flush()?;
        Ok(Self {
            conn,
            root,
            width,
            height,
            segment,
            map: map.cast(),
            damage,
            region,
            dirty: true,
            cursor_changed: true,
            keys,
            codes,
            shift,
            shifted: [false; 2],
            held: HashMap::new(),
            keys_down: Vec::new(),
            buttons_down: Vec::new(),
            hints: Hints::default(),
            resized: false,
            clipboard,
        })
    }

    fn foreign(&self, change: &xproto::MappingNotifyEvent) -> bool {
        let layout = self.layout();
        (0..change.count).any(|offset| {
            !layout
                .spare()
                .contains(&change.first_keycode.saturating_add(offset))
        })
    }

    fn terminal_focused(&self) -> Result<bool, Failure> {
        let mut window = xproto::get_input_focus(&self.conn)?.reply()?.focus;
        for _ in 0..8 {
            if window <= 1 || window == self.root {
                break;
            }
            let Ok(class) = xproto::get_property(
                &self.conn,
                false,
                window,
                xproto::AtomEnum::WM_CLASS,
                xproto::AtomEnum::STRING,
                0,
                64,
            )?
            .reply() else {
                break;
            };
            if !class.value.is_empty() {
                let name = String::from_utf8_lossy(&class.value).to_ascii_lowercase();
                return Ok(TERMINALS.iter().any(|terminal| name.contains(terminal)));
            }
            let Ok(tree) = xproto::query_tree(&self.conn, window)?.reply() else {
                break;
            };
            window = tree.parent;
        }
        Ok(false)
    }

    fn layout(&self) -> MutexGuard<'static, Keymap> {
        self.keys.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn click(&self, button: u8, down: bool) -> Result<(), Failure> {
        let kind = if down {
            xproto::BUTTON_PRESS_EVENT
        } else {
            xproto::BUTTON_RELEASE_EVENT
        };
        xtest::fake_input(
            &self.conn,
            kind,
            button,
            x11rb::CURRENT_TIME,
            self.root,
            0,
            0,
            0,
        )?;
        Ok(())
    }

    fn press(&self, keycode: u8, down: bool) -> Result<(), Failure> {
        let kind = if down {
            xproto::KEY_PRESS_EVENT
        } else {
            xproto::KEY_RELEASE_EVENT
        };
        xtest::fake_input(
            &self.conn,
            kind,
            keycode,
            x11rb::CURRENT_TIME,
            self.root,
            0,
            0,
            0,
        )?;
        Ok(())
    }
}

impl Screen for X11 {
    fn size(&self) -> (usize, usize) {
        (usize::from(self.width), usize::from(self.height))
    }

    fn fd(&self) -> RawFd {
        self.conn.stream().as_raw_fd()
    }

    fn pixels(&self) -> &[u32] {
        unsafe {
            std::slice::from_raw_parts(self.map, usize::from(self.width) * usize::from(self.height))
        }
    }

    fn drain(&mut self) -> Result<bool, Failure> {
        while let Some(event) = self.conn.poll_for_event()? {
            self.clipboard.handle(&self.conn, &event)?;
            match event {
                Event::DamageNotify(_) => self.dirty = true,
                Event::XfixesCursorNotify(_) => self.cursor_changed = true,
                Event::MappingNotify(change)
                    if change.request == xproto::Mapping::KEYBOARD && self.foreign(&change) =>
                {
                    let fresh = read_keys(&self.conn, self.root, self.codes)?;
                    self.shift = [
                        fresh.code(SHIFT_LEFT).unwrap_or(0),
                        fresh.code(SHIFT_RIGHT).unwrap_or(0),
                    ];
                    *self.layout() = fresh;
                }
                Event::ConfigureNotify(event) if event.window == self.root => {
                    self.resized |= (event.width, event.height) != (self.width, self.height);
                }
                Event::ConfigureNotify(event) => {
                    self.hints
                        .window(event.window, i32::from(event.x), i32::from(event.y));
                }
                _ => {}
            }
        }
        Ok(self.dirty)
    }

    fn cursor(&mut self) -> Result<Option<Cursor>, Failure> {
        if !self.cursor_changed {
            return Ok(None);
        }
        self.cursor_changed = false;
        let image = xfixes::get_cursor_image(&self.conn)?.reply()?;
        let pixels = usize::from(image.width) * usize::from(image.height);
        if image.width == 0
            || image.height == 0
            || image.width > MAX_CURSOR
            || image.height > MAX_CURSOR
            || image.cursor_image.len() < pixels
        {
            return Ok(None);
        }
        let mut rgba = Vec::with_capacity(pixels * 4);
        for &pixel in &image.cursor_image[..pixels] {
            let alpha = pixel >> 24;
            for shift in [16, 8, 0] {
                let value = (pixel >> shift) & 0xff;
                rgba.push(
                    (value * 255)
                        .checked_div(alpha)
                        .map_or(0, |straight| straight.min(255) as u8),
                );
            }
            rgba.push(alpha as u8);
        }
        Ok(Some(Cursor {
            width: image.width,
            height: image.height,
            hot_x: image.xhot.min(image.width - 1),
            hot_y: image.yhot.min(image.height - 1),
            rgba,
        }))
    }

    fn position(&mut self) -> Result<(u16, u16), Failure> {
        let reply = xproto::query_pointer(&self.conn, self.root)?.reply()?;
        self.hints
            .pointer(i32::from(reply.root_x), i32::from(reply.root_y));
        Ok((reply.root_x.max(0) as u16, reply.root_y.max(0) as u16))
    }

    fn offer(&mut self, text: &[u8]) -> Result<(), Failure> {
        self.clipboard.offer(&self.conn, text)?;
        self.conn.flush()?;
        Ok(())
    }

    fn expect_copy(&mut self) {
        self.clipboard.expect_copy();
    }

    fn pull(&mut self) -> Result<(), Failure> {
        self.clipboard.pull(&self.conn)
    }

    fn copied(&mut self) -> Option<Vec<u8>> {
        self.clipboard.copied()
    }

    fn chord(&mut self, letter: u8, shift: bool) -> Result<(), Failure> {
        let terminal = self.terminal_focused()?;
        if letter != b'v' {
            self.clipboard.expect_copy();
        }
        let (control, key) = {
            let layout = self.layout();
            (layout.code(CONTROL_LEFT), layout.code(u32::from(letter)))
        };
        let (Some(control), Some(key)) = (control, key) else {
            return Ok(());
        };
        let shift = (shift || (terminal && letter != b'x')) && self.shift[0] != 0;
        self.press(control, true)?;
        if shift {
            self.press(self.shift[0], true)?;
        }
        self.press(key, true)?;
        self.press(key, false)?;
        if shift {
            self.press(self.shift[0], false)?;
        }
        self.press(control, false)?;
        self.conn.flush()?;
        Ok(())
    }

    fn needs_key(&self, symbol: u32) -> bool {
        self.layout().needs_key(symbol)
    }

    fn bind(&mut self, symbols: impl Iterator<Item = u32>) -> Result<bool, Failure> {
        let (binds, width) = {
            let mut layout = self.layout();
            (layout.plan(symbols), layout.width())
        };
        for &(code, symbol, _) in &binds {
            xproto::change_keyboard_mapping(
                &self.conn,
                1,
                code,
                width as u8,
                &vec![symbol; width],
            )?
            .check()?;
        }
        xproto::get_input_focus(&self.conn)?.reply()?;
        Ok(binds.iter().any(|&(_, _, reused)| reused))
    }

    fn resized(&self) -> bool {
        self.resized
    }

    fn moves(&mut self) -> Vec<(i32, i32)> {
        self.hints.take()
    }

    fn grab(&mut self, full: bool) -> Result<Rect, Failure> {
        let (width, height) = self.size();
        let mut area = Rect {
            x: 0,
            y: 0,
            width,
            height,
        };
        damage::subtract(&self.conn, self.damage, x11rb::NONE, self.region)?;
        let fetched = xfixes::fetch_region(&self.conn, self.region)?.reply()?;
        self.dirty = false;
        if !full {
            let (mut top, mut bottom) = (height, 0usize);
            let (mut left, mut right) = (width, 0usize);
            for rect in &fetched.rectangles {
                let x = usize::try_from(rect.x.max(0)).unwrap_or(0).min(width);
                let y = usize::try_from(rect.y.max(0)).unwrap_or(0).min(height);
                left = left.min(x);
                top = top.min(y);
                right = right.max((x + usize::from(rect.width)).min(width));
                bottom = bottom.max((y + usize::from(rect.height)).min(height));
            }
            if bottom <= top || right <= left {
                return Ok(Rect {
                    x: 0,
                    y: 0,
                    width: 0,
                    height: 0,
                });
            }
            area = Rect {
                x: left,
                y: top,
                width: right - left,
                height: bottom - top,
            };
        }
        shm::get_image(
            &self.conn,
            self.root,
            0,
            area.y as i16,
            self.width,
            area.height as u16,
            !0,
            xproto::ImageFormat::Z_PIXMAP.into(),
            self.segment,
            (area.y * width * 4) as u32,
        )?
        .reply()?;
        let rows = unsafe {
            std::slice::from_raw_parts_mut(self.map.add(area.y * width), area.height * width)
        };
        for pixel in rows {
            *pixel &= 0x00ff_ffff;
        }
        Ok(area)
    }

    fn pointer(&mut self, x: i16, y: i16) -> Result<(), Failure> {
        xtest::fake_input(
            &self.conn,
            xproto::MOTION_NOTIFY_EVENT,
            0,
            x11rb::CURRENT_TIME,
            self.root,
            x,
            y,
            0,
        )?;
        self.conn.flush()?;
        Ok(())
    }

    fn button(&mut self, button: u8, down: bool) -> Result<(), Failure> {
        self.buttons_down.retain(|held| *held != button);
        if down {
            self.buttons_down.push(button);
        }
        self.click(button, down)?;
        self.conn.flush()?;
        Ok(())
    }

    fn key(&mut self, keycode: u8, down: bool) -> Result<(), Failure> {
        self.keys_down.retain(|held| *held != keycode);
        if down {
            self.keys_down.push(keycode);
        }
        for (slot, code) in self.shift.into_iter().enumerate() {
            if code != 0 && code == keycode {
                self.shifted[slot] = down;
            }
        }
        self.press(keycode, down)?;
        self.conn.flush()?;
        Ok(())
    }

    fn symbol(&mut self, symbol: u32, down: bool) -> Result<(), Failure> {
        if !down {
            if let Some(code) = self.held.remove(&symbol) {
                self.press(code, false)?;
                self.conn.flush()?;
            }
            return Ok(());
        }
        let Some(Place::Key(code, wanted)) = self.layout().place(symbol) else {
            return Ok(());
        };
        let active = self.shifted.iter().any(|down| *down);
        let (left, shift) = (self.shift[0], self.shift);
        if wanted && !active && left != 0 {
            self.press(left, true)?;
        } else if !wanted && active {
            for (slot, code) in shift.into_iter().enumerate() {
                if self.shifted[slot] {
                    self.press(code, false)?;
                }
            }
        }
        self.press(code, true)?;
        if wanted && !active && left != 0 {
            self.press(left, false)?;
        } else if !wanted && active {
            for (slot, code) in shift.into_iter().enumerate() {
                if self.shifted[slot] {
                    self.press(code, true)?;
                }
            }
        }
        self.held.insert(symbol, code);
        self.conn.flush()?;
        Ok(())
    }
}

impl Drop for X11 {
    fn drop(&mut self) {
        for &code in self.held.values().chain(&self.keys_down) {
            let _ = self.press(code, false);
        }
        for &button in &self.buttons_down {
            let _ = self.click(button, false);
        }
        let _ = self.conn.flush();
        let size = usize::from(self.width) * usize::from(self.height) * 4;
        unsafe {
            libc::munmap(self.map.cast(), size);
        }
    }
}
