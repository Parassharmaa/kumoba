use kumoba_view::Config;

const USAGE: &str =
    "usage: kumoba-view [--listen ADDRESS] [--token-file PATH] [--watch-token-file PATH]
                   [--fps N] [--max-mbps N]

Streams the X display named by DISPLAY to a browser.

  --listen ADDRESS   where to serve the viewer (default 127.0.0.1:6090). On any address other
                     than loopback, the stream needs ?token=TOKEN. The token comes from
                     KUMOBA_VIEW_TOKEN or --token-file, and is then needed on loopback too;
                     without either, one is made up and printed on a terminal, or written
                     to ~/.kumoba-view-token-PORT. A token is letters, digits and . _ ~ -
  --token-file PATH  read the token from PATH
  --watch-token-file PATH
                     read a second token from PATH, or set KUMOBA_VIEW_WATCH_TOKEN. A stream
                     opened with it shows the screen and ignores keyboard, mouse and
                     clipboard. It makes a token needed on loopback too, made up as above
                     when none is given
  --fps N            frames a second at most (default 60)
  --max-mbps N       megabits a second at most (default 30)";

fn from_env(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|token| !token.is_empty())
}

fn read_token(flag: &str, path: Option<String>) -> String {
    let Some(path) = path else {
        eprintln!("kumoba-view: {flag} needs a path");
        std::process::exit(2);
    };
    match std::fs::read_to_string(&path) {
        Ok(token) if token.trim().is_empty() => {
            eprintln!("kumoba-view: {path} is empty");
            std::process::exit(2);
        }
        Ok(token) => token.trim().to_string(),
        Err(error) => {
            eprintln!("kumoba-view: cannot read {path}: {error}");
            std::process::exit(2);
        }
    }
}

fn main() {
    let mut config = Config {
        token: from_env("KUMOBA_VIEW_TOKEN"),
        watch_token: from_env("KUMOBA_VIEW_WATCH_TOKEN"),
        ..Config::default()
    };
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--listen" => {
                if let Some(value) = args.next() {
                    config.listen = value;
                }
            }
            "--fps" => {
                if let Some(value) = args.next().and_then(|value| value.parse().ok()) {
                    config.fps = value;
                }
            }
            "--max-mbps" => {
                if let Some(value) = args.next().and_then(|value| value.parse().ok()) {
                    config.max_mbps = value;
                }
            }
            "--screen" => {
                if let Some(value) = args.next() {
                    config.screen = Some(value.into());
                }
            }
            "--owned" => config.owned = true,
            "--token-file" => config.token = Some(read_token(&arg, args.next())),
            "--watch-token-file" => config.watch_token = Some(read_token(&arg, args.next())),
            "--help" | "-h" => {
                println!("{USAGE}");
                return;
            }
            "--version" => {
                println!("kumoba-view {}", env!("CARGO_PKG_VERSION"));
                return;
            }
            other => {
                eprintln!("kumoba-view: unknown argument {other}");
                std::process::exit(2);
            }
        }
    }
    if let Err(error) = kumoba_view::serve(&config) {
        eprintln!("kumoba-view: {error}");
        std::process::exit(1);
    }
}
