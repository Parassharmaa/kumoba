#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss
)]

#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
mod flow;
#[cfg(target_os = "linux")]
mod http;
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
mod keymap;
#[cfg(target_os = "linux")]
mod session;
#[cfg(target_os = "linux")]
mod ws;
#[cfg(target_os = "linux")]
mod x11;

use std::io;

#[derive(Clone, Debug)]
pub struct Config {
    pub listen: String,
    pub fps: u32,
    pub max_mbps: u32,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            listen: "127.0.0.1:6090".into(),
            fps: 60,
            max_mbps: 30,
        }
    }
}

#[cfg(target_os = "linux")]
pub fn serve(config: &Config) -> io::Result<()> {
    http::serve(
        &config.listen,
        session::Settings {
            fps: config.fps.clamp(1, 120),
            max_mbps: config.max_mbps.clamp(1, 1000),
        },
    )
}

#[cfg(not(target_os = "linux"))]
pub fn serve(_config: &Config) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "this platform has no capture backend yet",
    ))
}
