use serde::{Deserialize, Serialize};
pub const CDP_PORT: u16 = 9222;
pub const VNC_PORT: u16 = 5900;
pub const MAX_SCREENSHOT: usize = 5 * 1024 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum DesktopRequest {
    Start {
        width: u16,
        height: u16,
        #[serde(default = "default_startup_timeout_ms")]
        startup_timeout_ms: u64,
    },
    Stop,
    Screenshot,
    Action(Action),
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Action {
    Move {
        x: u16,
        y: u16,
    },
    Click {
        x: u16,
        y: u16,
        button: u8,
        count: u8,
    },
    Button {
        button: u8,
        down: bool,
    },
    Scroll {
        x: u16,
        y: u16,
        rows: i16,
    },
    Type {
        text: String,
    },
    Key {
        chord: String,
    },
}
pub fn valid_size(width: u16, height: u16) -> bool {
    (640..=1920).contains(&width)
        && (480..=1080).contains(&height)
        && u32::from(width) * u32::from(height) <= 1_048_576
}
pub fn valid_action(action: &Action, width: u16, height: u16) -> bool {
    match action {
        Action::Move { x, y } => *x < width && *y < height,
        Action::Click {
            x,
            y,
            button,
            count,
        } => *x < width && *y < height && (1..=3).contains(button) && (1..=2).contains(count),
        Action::Button { button, .. } => (1..=3).contains(button),
        Action::Scroll { x, y, rows } => *x < width && *y < height && (-20..=20).contains(rows),
        Action::Type { text } => text.len() <= 4096 && !text.contains('\0'),
        Action::Key { chord } => valid_key(chord),
    }
}
fn valid_key(chord: &str) -> bool {
    let parts: Vec<_> = chord.split('+').collect();
    if parts.is_empty() || parts.len() > 5 {
        return false;
    }
    if parts[..parts.len() - 1]
        .iter()
        .any(|p| !matches!(*p, "ctrl" | "alt" | "shift" | "super"))
    {
        return false;
    }
    let key = parts[parts.len() - 1];
    key.len() == 1 && key.as_bytes()[0].is_ascii_alphanumeric()
        || matches!(
            key,
            "Return"
                | "Tab"
                | "Escape"
                | "BackSpace"
                | "Delete"
                | "Home"
                | "End"
                | "Page_Up"
                | "Page_Down"
                | "Left"
                | "Right"
                | "Up"
                | "Down"
                | "space"
        )
        || key
            .strip_prefix('F')
            .and_then(|n| n.parse::<u8>().ok())
            .is_some_and(|n| (1..=12).contains(&n))
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn bounded_display_and_actions() {
        assert!(valid_size(1280, 720));
        assert!(!valid_size(1920, 1080));
        assert!(valid_key("ctrl+shift+a"));
        assert!(!valid_key("--window"));
        assert!(!valid_key("ctrl+"));
        assert!(!valid_action(
            &Action::Click {
                x: 1280,
                y: 0,
                button: 1,
                count: 1
            },
            1280,
            720
        ));
    }
}

pub fn default_startup_timeout_ms() -> u64 {
    20_000
}
pub fn valid_startup_timeout_ms(value: u64) -> bool {
    (1_000..=180_000).contains(&value)
}

#[cfg(test)]
mod compatibility_tests {
    use super::*;
    #[test]
    fn older_desktop_requests_keep_the_twenty_second_default() {
        let request: DesktopRequest =
            serde_json::from_str(r#"{"Start":{"width":1280,"height":720}}"#).unwrap();
        assert!(matches!(
            request,
            DesktopRequest::Start {
                startup_timeout_ms: 20_000,
                ..
            }
        ));
        let hello: crate::Response =
            serde_json::from_str(r#"{"Hello":{"version":2,"arch":"aarch64","desktop":true}}"#)
                .unwrap();
        assert!(matches!(
            hello,
            crate::Response::Hello {
                desktop_startup_timeout: false,
                ..
            }
        ));
    }
}
