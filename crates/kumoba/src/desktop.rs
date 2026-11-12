use crate::{Error, Result, Sandbox};
use base64::{Engine, engine::general_purpose::STANDARD};
use kumoba_protocol::{
    Request, Response,
    desktop::{self, Action, DesktopRequest},
};
use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DesktopConfig {
    pub width: u16,
    pub height: u16,
    pub startup_timeout: Duration,
}
impl Default for DesktopConfig {
    fn default() -> Self {
        Self {
            width: 1280,
            height: 720,
            startup_timeout: Duration::from_secs(20),
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DesktopInfo {
    pub width: u16,
    pub height: u16,
    pub browser_port: u16,
    pub vnc_port: Option<u16>,
}
#[derive(Debug)]
pub struct DesktopScreenshot {
    pub width: u16,
    pub height: u16,
    pub png: Vec<u8>,
}
#[derive(Debug, Clone, Copy)]
pub enum MouseButton {
    Left,
    Middle,
    Right,
}
impl MouseButton {
    fn number(self) -> u8 {
        match self {
            Self::Left => 1,
            Self::Middle => 2,
            Self::Right => 3,
        }
    }
}
#[derive(Debug, Clone)]
pub enum DesktopAction {
    Move {
        x: u16,
        y: u16,
    },
    Click {
        x: u16,
        y: u16,
        button: MouseButton,
        count: u8,
    },
    Press(MouseButton),
    Release(MouseButton),
    Scroll {
        x: u16,
        y: u16,
        rows: i16,
    },
    Type(String),
    Key(String),
}
impl DesktopAction {
    fn wire(self) -> Action {
        match self {
            Self::Move { x, y } => Action::Move { x, y },
            Self::Click {
                x,
                y,
                button,
                count,
            } => Action::Click {
                x,
                y,
                button: button.number(),
                count,
            },
            Self::Press(button) => Action::Button {
                button: button.number(),
                down: true,
            },
            Self::Release(button) => Action::Button {
                button: button.number(),
                down: false,
            },
            Self::Scroll { x, y, rows } => Action::Scroll { x, y, rows },
            Self::Type(text) => Action::Type { text },
            Self::Key(chord) => Action::Key { chord },
        }
    }
}
impl Sandbox {
    pub async fn desktop_start(&mut self, config: DesktopConfig) -> Result<DesktopInfo> {
        if !desktop::valid_size(config.width, config.height) {
            return Err(Error::InvalidInput(
                "desktop must be 640..1920 by 480..1080 and at most 1,048,576 pixels".into(),
            ));
        }
        if self.config().memory_mib < 1024 {
            return Err(Error::InvalidInput(
                "desktop requires at least 1024 MiB; 2048 MiB is recommended".into(),
            ));
        }
        let timeout_ms = u64::try_from(config.startup_timeout.as_millis()).unwrap_or(u64::MAX);
        if !desktop::valid_startup_timeout_ms(timeout_ms) {
            return Err(Error::InvalidInput(
                "desktop startup timeout must be 1..180 seconds".into(),
            ));
        }
        self.require_desktop_capability()?;
        if timeout_ms != desktop::default_startup_timeout_ms()
            && !self.supports_desktop_startup_timeout()
        {
            return Err(Error::InvalidInput(
                "rebuild the guest image to configure desktop startup timeout".into(),
            ));
        }
        match self
            .rpc(
                Request::Desktop(DesktopRequest::Start {
                    width: config.width,
                    height: config.height,
                    startup_timeout_ms: timeout_ms,
                }),
                config.startup_timeout + Duration::from_secs(40),
            )
            .await?
        {
            Response::Desktop {
                width,
                height,
                cdp_port,
                vnc_port,
            } if width == config.width
                && height == config.height
                && cdp_port == desktop::CDP_PORT
                && vnc_port.is_none_or(|port| port == desktop::VNC_PORT) =>
            {
                let info = DesktopInfo {
                    width,
                    height,
                    browser_port: cdp_port,
                    vnc_port,
                };
                self.set_desktop_info(Some(info))?;
                Ok(info)
            }
            Response::Error(error) => Err(Error::Guest(error)),
            _ => Err(Error::Protocol("invalid desktop startup response".into())),
        }
    }
    pub async fn desktop_stop(&mut self) -> Result<()> {
        self.require_desktop_capability()?;
        match self
            .rpc(
                Request::Desktop(DesktopRequest::Stop),
                Duration::from_secs(10),
            )
            .await?
        {
            Response::Ok => {
                self.set_desktop_info(None)?;
                Ok(())
            }
            Response::Error(error) => Err(Error::Guest(error)),
            _ => Err(Error::Protocol("invalid desktop stop response".into())),
        }
    }
    pub async fn desktop_action(&mut self, action: DesktopAction) -> Result<()> {
        let info = self.desktop_info()?;
        let action = action.wire();
        if !desktop::valid_action(&action, info.width, info.height) {
            return Err(Error::InvalidInput("invalid desktop action".into()));
        }
        match self
            .rpc(
                Request::Desktop(DesktopRequest::Action(action)),
                Duration::from_secs(10),
            )
            .await?
        {
            Response::Ok => Ok(()),
            Response::Error(error) => Err(Error::Guest(error)),
            _ => Err(Error::Protocol("invalid desktop action response".into())),
        }
    }
    pub async fn desktop_screenshot(&mut self) -> Result<DesktopScreenshot> {
        let info = self.desktop_info()?;
        match self
            .rpc(
                Request::Desktop(DesktopRequest::Screenshot),
                Duration::from_secs(15),
            )
            .await?
        {
            Response::Screenshot { width, height, png }
                if width == info.width && height == info.height =>
            {
                if png.len() > desktop::MAX_SCREENSHOT.div_ceil(3) * 4 {
                    return Err(Error::Protocol("screenshot exceeds limit".into()));
                }
                let png = STANDARD
                    .decode(png)
                    .map_err(|_| Error::Protocol("invalid screenshot encoding".into()))?;
                if png.len() > desktop::MAX_SCREENSHOT
                    || png.len() < 33
                    || &png[..8] != b"\x89PNG\r\n\x1a\n"
                    || &png[12..16] != b"IHDR"
                    || u32::from_be_bytes(png[16..20].try_into().unwrap()) != u32::from(width)
                    || u32::from_be_bytes(png[20..24].try_into().unwrap()) != u32::from(height)
                {
                    return Err(Error::Protocol(
                        "invalid screenshot dimensions or PNG header".into(),
                    ));
                }
                Ok(DesktopScreenshot { width, height, png })
            }
            Response::Error(error) => Err(Error::Guest(error)),
            _ => Err(Error::Protocol("invalid screenshot response".into())),
        }
    }
}
