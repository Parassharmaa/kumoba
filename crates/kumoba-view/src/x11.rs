use crate::keymap::{Keymap, Place, SHIFT_LEFT, SHIFT_RIGHT};
use kumoba_view_codec::Rect;
use std::collections::HashMap;
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use x11rb::connection::Connection;
use x11rb::protocol::{Event, damage, shm, xfixes, xproto, xtest};
use x11rb::rust_connection::RustConnection;

type Failure = Box<dyn std::error::Error + Send + Sync>;

pub struct Screen {
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
    keys: Keymap,
    shift: [u8; 2],
    shifted: [bool; 2],
    held: HashMap<u32, u8>,
    keys_down: Vec<u8>,
    buttons_down: Vec<u8>,
}

pub struct Cursor {
    pub width: u16,
    pub height: u16,
    pub hot_x: u16,
    pub hot_y: u16,
    pub rgba: Vec<u8>,
}

const MAX_CURSOR: u16 = 128;

unsafe impl Send for Screen {}

fn last_error() -> Failure {
    io::Error::last_os_error().into()
}

impl Screen {
    pub fn open() -> Result<Self, Failure> {
        let (conn, number) = RustConnection::connect(None)?;
        let setup = conn.setup();
        let screen = &setup.roots[number];
        let (width, height, root) = (screen.width_in_pixels, screen.height_in_pixels, screen.root);
        let format = setup
            .pixmap_formats
            .iter()
            .find(|format| format.depth == screen.root_depth)
            .ok_or("the display has no pixmap format for its depth")?;
        if format.bits_per_pixel != 32 || setup.image_byte_order != xproto::ImageOrder::LSB_FIRST {
            return Err("the display must use 32-bit little-endian pixels".into());
        }
        shm::query_version(&conn)?.reply()?;
        xfixes::query_version(&conn, 5, 0)?.reply()?;
        damage::query_version(&conn, 1, 1)?.reply()?;
        xtest::get_version(&conn, 2, 2)?.reply()?;
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
        let (first, last) = (setup.min_keycode, setup.max_keycode);
        let mapping = xproto::get_keyboard_mapping(&conn, first, last - first + 1)?.reply()?;
        let keys = Keymap::new(
            first,
            usize::from(mapping.keysyms_per_keycode),
            &mapping.keysyms,
        );
        let shift = [
            keys.code(SHIFT_LEFT).unwrap_or(0),
            keys.code(SHIFT_RIGHT).unwrap_or(0),
        ];
        let segment = conn.generate_id()?;
        shm::attach_fd(&conn, segment, fd, false)?.check()?;
        let damage = conn.generate_id()?;
        damage::create(&conn, damage, root, damage::ReportLevel::NON_EMPTY)?.check()?;
        let region = conn.generate_id()?;
        xfixes::create_region(&conn, region, &[])?.check()?;
        xfixes::select_cursor_input(&conn, root, xfixes::CursorNotifyMask::DISPLAY_CURSOR)?
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
            shift,
            shifted: [false; 2],
            held: HashMap::new(),
            keys_down: Vec::new(),
            buttons_down: Vec::new(),
        })
    }

    pub fn size(&self) -> (usize, usize) {
        (usize::from(self.width), usize::from(self.height))
    }

    pub fn fd(&self) -> RawFd {
        self.conn.stream().as_raw_fd()
    }

    pub fn pixels(&self) -> &[u32] {
        unsafe {
            std::slice::from_raw_parts(self.map, usize::from(self.width) * usize::from(self.height))
        }
    }

    pub fn drain(&mut self) -> Result<bool, Failure> {
        while let Some(event) = self.conn.poll_for_event()? {
            match event {
                Event::DamageNotify(_) => self.dirty = true,
                Event::XfixesCursorNotify(_) => self.cursor_changed = true,
                _ => {}
            }
        }
        Ok(self.dirty)
    }

    pub fn cursor(&mut self) -> Result<Option<Cursor>, Failure> {
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

    pub fn position(&self) -> Result<(u16, u16), Failure> {
        let reply = xproto::query_pointer(&self.conn, self.root)?.reply()?;
        Ok((reply.root_x.max(0) as u16, reply.root_y.max(0) as u16))
    }

    pub fn grab(&mut self, full: bool) -> Result<Rect, Failure> {
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

    pub fn pointer(&self, x: i16, y: i16) -> Result<(), Failure> {
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

    pub fn button(&mut self, button: u8, down: bool) -> Result<(), Failure> {
        self.buttons_down.retain(|held| *held != button);
        if down {
            self.buttons_down.push(button);
        }
        self.click(button, down)?;
        self.conn.flush()?;
        Ok(())
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

    pub fn key(&mut self, keycode: u8, down: bool) -> Result<(), Failure> {
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

    pub fn symbol(&mut self, symbol: u32, down: bool) -> Result<(), Failure> {
        if !down {
            if let Some(code) = self.held.remove(&symbol) {
                self.press(code, false)?;
                self.conn.flush()?;
            }
            return Ok(());
        }
        let (code, wanted) = match self.keys.place(symbol) {
            None => return Ok(()),
            Some(Place::Key(code, shifted)) => (code, shifted),
            Some(Place::Bind(code)) => {
                let symbols = vec![symbol; self.keys.width()];
                xproto::change_keyboard_mapping(
                    &self.conn,
                    1,
                    code,
                    self.keys.width() as u8,
                    &symbols,
                )?
                .check()?;
                xproto::get_input_focus(&self.conn)?.reply()?;
                (code, false)
            }
        };
        debug_assert!(code >= self.keys.first());
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

impl Drop for Screen {
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
