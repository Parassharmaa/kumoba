#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss
)]

#[cfg(target_os = "linux")]
mod clipboard;
mod flow;
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
mod hints;
mod http;
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
mod keymap;
mod mackeys;
mod remote;
mod screen;
mod session;
mod ws;
#[cfg(target_os = "linux")]
mod x11;

use std::io::{self, Read};
use std::path::PathBuf;
use std::time::Duration;

const GONE_CHECK: Duration = Duration::from_secs(2);

#[derive(Clone, Debug)]
pub struct Config {
    pub listen: String,
    pub fps: u32,
    pub max_mbps: u32,
    pub screen: Option<PathBuf>,
    pub owned: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            listen: "127.0.0.1:6090".into(),
            fps: 60,
            max_mbps: 30,
            screen: None,
            owned: false,
        }
    }
}

#[cfg(target_os = "linux")]
fn local() -> io::Result<http::Source> {
    Ok(http::Source::X11)
}

#[cfg(not(target_os = "linux"))]
fn local() -> io::Result<http::Source> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "this platform captures no screen of its own; pass --screen SOCKET",
    ))
}

pub fn serve(config: &Config) -> io::Result<()> {
    if config.owned {
        std::thread::spawn(|| {
            let mut byte = [0u8; 1];
            loop {
                match io::stdin().read(&mut byte) {
                    Ok(1) => {}
                    Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                    _ => break,
                }
            }
            std::process::exit(0);
        });
    }
    let source = match &config.screen {
        Some(path) => {
            let screen = path.clone();
            std::thread::spawn(move || {
                while screen.exists() {
                    std::thread::sleep(GONE_CHECK);
                }
                std::process::exit(0);
            });
            http::Source::Remote(path.clone())
        }
        None => local()?,
    };
    http::serve(
        &config.listen,
        session::Settings {
            fps: config.fps.clamp(1, 120),
            max_mbps: config.max_mbps.clamp(1, 1000),
        },
        &source,
    )
}
