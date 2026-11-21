use kumoba_view::Config;

fn main() {
    let mut config = Config::default();
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
