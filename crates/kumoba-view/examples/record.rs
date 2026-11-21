#![allow(clippy::cast_possible_truncation, clippy::cast_possible_wrap)]

#[cfg(target_os = "linux")]
fn record(path: &str, frames: u32, fps: u32) -> Result<(), Box<dyn std::error::Error>> {
    use std::io::Write;
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
    use std::time::{Duration, Instant};
    use x11rb::connection::Connection;
    use x11rb::protocol::{shm, xproto};

    let (conn, number) = x11rb::rust_connection::RustConnection::connect(None)?;
    let screen = &conn.setup().roots[number];
    let (width, height, root) = (screen.width_in_pixels, screen.height_in_pixels, screen.root);
    let size = usize::from(width) * usize::from(height) * 4;
    let fd = unsafe { libc::memfd_create(c"kumoba-view-record".as_ptr(), libc::MFD_CLOEXEC) };
    if fd < 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    let fd = unsafe { OwnedFd::from_raw_fd(fd) };
    if unsafe { libc::ftruncate(fd.as_raw_fd(), size as libc::off_t) } != 0 {
        return Err(std::io::Error::last_os_error().into());
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
        return Err(std::io::Error::last_os_error().into());
    }
    let pixels = unsafe { std::slice::from_raw_parts(map.cast::<u8>(), size) };
    let segment = conn.generate_id()?;
    shm::attach_fd(&conn, segment, fd, false)?.check()?;
    let mut out = std::io::BufWriter::new(std::fs::File::create(path)?);
    out.write_all(&u32::from(width).to_le_bytes())?;
    out.write_all(&u32::from(height).to_le_bytes())?;
    out.write_all(&frames.to_le_bytes())?;
    let interval = Duration::from_nanos(1_000_000_000 / u64::from(fps.max(1)));
    let started = Instant::now();
    for index in 0..frames {
        if let Some(wait) = (started + interval * index).checked_duration_since(Instant::now()) {
            std::thread::sleep(wait);
        }
        shm::get_image(
            &conn,
            root,
            0,
            0,
            width,
            height,
            !0,
            xproto::ImageFormat::Z_PIXMAP.into(),
            segment,
            0,
        )?
        .reply()?;
        out.write_all(pixels)?;
    }
    out.flush()?;
    println!("recorded {frames} frames of {width}x{height}");
    Ok(())
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    #[cfg(target_os = "linux")]
    if let Err(error) = record(
        args.get(1).map_or("/tmp/screen.raw", String::as_str),
        args.get(2)
            .and_then(|value| value.parse().ok())
            .unwrap_or(90),
        args.get(3)
            .and_then(|value| value.parse().ok())
            .unwrap_or(60),
    ) {
        eprintln!("record: {error}");
        std::process::exit(1);
    }
    #[cfg(not(target_os = "linux"))]
    eprintln!("record: Linux only ({} arguments ignored)", args.len());
}
