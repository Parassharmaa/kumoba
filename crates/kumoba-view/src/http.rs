use crate::remote::Remote;
use crate::session::{self, Settings};
use crate::ws;
#[cfg(target_os = "linux")]
use crate::x11::X11;
use std::io::{self, IsTerminal, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::os::fd::AsRawFd;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

#[derive(Clone)]
pub enum Source {
    #[cfg(target_os = "linux")]
    X11,
    Remote(PathBuf),
}

pub trait Link: Read + Write + AsRawFd + Send + 'static {
    fn limit(&self, wait: Option<Duration>) -> io::Result<()>;
    fn streaming(&self) -> io::Result<()>;
}

impl Link for TcpStream {
    fn limit(&self, wait: Option<Duration>) -> io::Result<()> {
        self.set_read_timeout(wait)
    }

    fn streaming(&self) -> io::Result<()> {
        self.set_nonblocking(true)?;
        self.set_nodelay(true)
    }
}

impl Link for UnixStream {
    fn limit(&self, wait: Option<Duration>) -> io::Result<()> {
        self.set_read_timeout(wait)
    }

    fn streaming(&self) -> io::Result<()> {
        self.set_nonblocking(true)
    }
}

const PAGE: &[u8] = include_bytes!("../viewer/index.html");
const SCRIPT: &[u8] = include_bytes!("../viewer/viewer.js");
const CODEC: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/codec.wasm"));
const MAX_HEAD: usize = 16 * 1024;
const MAX_WAITING: usize = 32;
const MAX_STREAMS: usize = 16;
const HEAD_WAIT: Duration = Duration::from_secs(5);

struct Request {
    method: String,
    path: String,
    query: String,
    headers: Vec<(String, String)>,
}

impl Request {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    }
}

fn read_request(stream: &mut impl Link) -> io::Result<Option<Request>> {
    let deadline = Instant::now() + HEAD_WAIT;
    let mut head = Vec::new();
    let mut chunk = [0u8; 2048];
    while !head.windows(4).any(|window| window == b"\r\n\r\n") {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Ok(None);
        }
        stream.limit(Some(left))?;
        let count = match stream.read(&mut chunk) {
            Ok(count) => count,
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                ) =>
            {
                return Ok(None);
            }
            Err(error) => return Err(error),
        };
        if count == 0 || head.len() + count > MAX_HEAD {
            return Ok(None);
        }
        head.extend_from_slice(&chunk[..count]);
    }
    let text = String::from_utf8_lossy(&head);
    let mut lines = text.split("\r\n");
    let mut first = lines.next().unwrap_or("").split_whitespace();
    let (Some(method), Some(target)) = (first.next(), first.next()) else {
        return Ok(None);
    };
    let (path, query) = target.split_once('?').unwrap_or((target, ""));
    let (path, query) = (path.to_string(), query.to_string());
    let headers = lines
        .filter_map(|line| line.split_once(':'))
        .map(|(key, value)| (key.trim().to_ascii_lowercase(), value.trim().to_string()))
        .collect();
    Ok(Some(Request {
        method: method.to_string(),
        path,
        query,
        headers,
    }))
}

fn respond(stream: &mut impl Write, status: &str, kind: &str, body: &[u8]) -> io::Result<()> {
    let head = format!(
        "HTTP/1.1 {status}\r\ncontent-type: {kind}\r\ncontent-length: {}\r\ncache-control: no-store\r\nconnection: close\r\n\r\n",
        body.len()
    );
    stream.write_all(head.as_bytes())?;
    stream.write_all(body)
}

fn local_request(request: &Request, port: u16) -> bool {
    let Some(host) = request.header("host") else {
        return false;
    };
    let allowed = [
        format!("127.0.0.1:{port}"),
        format!("localhost:{port}"),
        format!("[::1]:{port}"),
    ];
    if !allowed.iter().any(|name| name.eq_ignore_ascii_case(host)) {
        return false;
    }
    match request.header("origin") {
        None => true,
        Some(origin) => origin
            .strip_prefix("http://")
            .is_some_and(|rest| rest.eq_ignore_ascii_case(host)),
    }
}

#[derive(Clone)]
pub enum Guard {
    Private,
    Loopback(u16),
    Token(Arc<str>),
}

fn same(left: &[u8], right: &[u8]) -> bool {
    left.len() == right.len()
        && left
            .iter()
            .zip(right)
            .fold(0u8, |difference, (a, b)| difference | (a ^ b))
            == 0
}

fn token_of(query: &str) -> Option<&str> {
    query
        .split('&')
        .find_map(|pair| pair.strip_prefix("token="))
}

impl Guard {
    fn admits(&self, request: &Request) -> bool {
        match self {
            Self::Private => true,
            Self::Loopback(port) => local_request(request, *port),
            Self::Token(token) => {
                request.path != "/stream"
                    || token_of(&request.query)
                        .is_some_and(|given| same(given.as_bytes(), token.as_bytes()))
            }
        }
    }
}

fn save_token(token: &str, port: u16) -> io::Result<PathBuf> {
    use std::os::unix::fs::OpenOptionsExt;
    let home = std::env::var_os("HOME")
        .filter(|home| !home.is_empty())
        .ok_or_else(|| io::Error::other("set KUMOBA_VIEW_TOKEN, or HOME for a made-up token"))?;
    let path = PathBuf::from(home).join(format!(".kumoba-view-token-{port}"));
    let _ = std::fs::remove_file(&path);
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&path)?;
    writeln!(file, "{token}")?;
    Ok(path)
}

pub fn new_token() -> io::Result<String> {
    let mut bytes = [0u8; 16];
    std::fs::File::open("/dev/urandom")?.read_exact(&mut bytes)?;
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}

struct Slot(Arc<AtomicUsize>);

impl Slot {
    fn take(count: &Arc<AtomicUsize>, limit: usize) -> Option<Self> {
        if count.fetch_add(1, Ordering::SeqCst) >= limit {
            count.fetch_sub(1, Ordering::SeqCst);
            return None;
        }
        Some(Self(Arc::clone(count)))
    }
}

impl Drop for Slot {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

fn handle(
    mut stream: impl Link,
    waiting: Slot,
    streams: &Arc<AtomicUsize>,
    guard: &Guard,
    settings: Settings,
    source: &Source,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let Some(request) = read_request(&mut stream)? else {
        return Ok(());
    };
    if !guard.admits(&request) {
        return Ok(respond(
            &mut stream,
            "403 Forbidden",
            "text/plain",
            b"forbidden\n",
        )?);
    }
    if request.method != "GET" {
        return Ok(respond(
            &mut stream,
            "405 Method Not Allowed",
            "text/plain",
            b"method not allowed\n",
        )?);
    }
    match request.path.as_str() {
        "/" | "/index.html" => respond(&mut stream, "200 OK", "text/html; charset=utf-8", PAGE)?,
        "/viewer.js" => respond(
            &mut stream,
            "200 OK",
            "text/javascript; charset=utf-8",
            SCRIPT,
        )?,
        "/codec.wasm" => respond(&mut stream, "200 OK", "application/wasm", CODEC)?,
        "/stream" => {
            let upgrade = request.header("upgrade").unwrap_or("");
            let Some(key) = request.header("sec-websocket-key") else {
                return Ok(respond(
                    &mut stream,
                    "400 Bad Request",
                    "text/plain",
                    b"expected a WebSocket\n",
                )?);
            };
            if !upgrade.eq_ignore_ascii_case("websocket") {
                return Ok(respond(
                    &mut stream,
                    "400 Bad Request",
                    "text/plain",
                    b"expected a WebSocket\n",
                )?);
            }
            let Some(streaming) = Slot::take(streams, MAX_STREAMS) else {
                return Ok(respond(
                    &mut stream,
                    "503 Service Unavailable",
                    "text/plain",
                    b"too many live views are open\n",
                )?);
            };
            drop(waiting);
            let head = format!(
                "HTTP/1.1 101 Switching Protocols\r\nupgrade: websocket\r\nconnection: Upgrade\r\nsec-websocket-accept: {}\r\n\r\n",
                ws::accept_key(key)
            );
            stream.write_all(head.as_bytes())?;
            stream.limit(None)?;
            match source {
                #[cfg(target_os = "linux")]
                Source::X11 => session::run(stream, settings, X11::open)?,
                Source::Remote(path) => session::run(stream, settings, || Remote::open(path))?,
            }
            drop(streaming);
        }
        _ => respond(&mut stream, "404 Not Found", "text/plain", b"not found\n")?,
    }
    Ok(())
}

fn accept<L: Link>(
    incoming: impl Iterator<Item = io::Result<L>>,
    guard: &Guard,
    settings: Settings,
    source: &Source,
) {
    let (waiting, streams) = (Arc::new(AtomicUsize::new(0)), Arc::new(AtomicUsize::new(0)));
    for stream in incoming {
        let Ok(stream) = stream else { continue };
        let Some(slot) = Slot::take(&waiting, MAX_WAITING) else {
            continue;
        };
        let (guard, source, streams) = (guard.clone(), source.clone(), Arc::clone(&streams));
        std::thread::spawn(move || {
            if let Err(error) = handle(stream, slot, &streams, &guard, settings, &source) {
                eprintln!("kumoba-view: connection ended: {error}");
            }
        });
    }
}

pub fn valid_token(token: &str) -> bool {
    !token.is_empty()
        && token
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || "._~-".contains(character))
}

fn guard_for(address: SocketAddr, token: Option<&str>) -> io::Result<(Guard, bool)> {
    match token {
        Some(token) if valid_token(token) => Ok((Guard::Token(token.into()), false)),
        Some(_) => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "the token must be letters, digits, '.', '_', '~' or '-'",
        )),
        None if address.ip().is_loopback() => Ok((Guard::Loopback(address.port()), false)),
        None => Ok((Guard::Token(new_token()?.into()), true)),
    }
}

pub fn serve(
    listen: &str,
    token: Option<&str>,
    settings: Settings,
    source: &Source,
) -> io::Result<()> {
    if let Some(path) = listen.strip_prefix("unix:") {
        let _ = std::fs::remove_file(path);
        let listener = UnixListener::bind(path)?;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
        println!("kumoba-view listening on {path}");
        accept(listener.incoming(), &Guard::Private, settings, source);
        return Ok(());
    }
    let listener = TcpListener::bind(listen)?;
    let address = listener.local_addr()?;
    let port = address.port();
    let (guard, made_up) = guard_for(address, token)?;
    let opening =
        format!("kumoba-view listening on {address}; open http://<this host>:{port}/?token=");
    match &guard {
        Guard::Token(token) if made_up && io::stdout().is_terminal() => {
            println!("{opening}{token}");
        }
        Guard::Token(token) if made_up => {
            let saved = save_token(token, port)?;
            println!("{opening} followed by the token in {}", saved.display());
        }
        Guard::Token(_) => println!("{opening}<your token>"),
        _ => println!("kumoba-view listening on {address}"),
    }
    accept(listener.incoming(), &guard, settings, source);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn answer(guard: Guard, request: &str) -> String {
        let (mut browser, engine) = UnixStream::pair().unwrap();
        browser.write_all(request.as_bytes()).unwrap();
        let settings = Settings {
            fps: 30,
            max_mbps: 10,
        };
        let source = Source::Remote("/nonexistent/view.sock".into());
        let serving = std::thread::spawn(move || {
            let waiting = Arc::new(AtomicUsize::new(0));
            let slot = Slot::take(&waiting, MAX_WAITING).unwrap();
            let streams = Arc::new(AtomicUsize::new(0));
            handle(engine, slot, &streams, &guard, settings, &source).unwrap();
        });
        let mut reply = Vec::new();
        browser.read_to_end(&mut reply).unwrap();
        serving.join().unwrap();
        String::from_utf8_lossy(&reply)
            .lines()
            .next()
            .unwrap_or("")
            .to_string()
    }

    #[test]
    fn a_private_socket_serves_any_host_and_a_port_serves_only_its_own() {
        let page = "GET / HTTP/1.1\r\nhost: sandbox.example\r\n\r\n";
        assert_eq!(answer(Guard::Private, page), "HTTP/1.1 200 OK");
        assert_eq!(
            answer(Guard::Loopback(6090), page),
            "HTTP/1.1 403 Forbidden"
        );
        let own = "GET / HTTP/1.1\r\nhost: 127.0.0.1:6090\r\n\r\n";
        assert_eq!(answer(Guard::Loopback(6090), own), "HTTP/1.1 200 OK");
        assert_eq!(
            answer(Guard::Private, "POST / HTTP/1.1\r\n\r\n"),
            "HTTP/1.1 405 Method Not Allowed"
        );
    }

    fn request(headers: &[(&str, &str)]) -> Request {
        Request {
            method: "GET".into(),
            path: "/stream".into(),
            query: String::new(),
            headers: headers
                .iter()
                .map(|(name, value)| ((*name).to_string(), (*value).to_string()))
                .collect(),
        }
    }

    #[test]
    fn a_token_guards_the_stream_from_any_host_but_not_the_page() {
        let guard = || Guard::Token("s3cret".into());
        let at = |target: &str| format!("GET {target} HTTP/1.1\r\nhost: 172.17.0.2:16090\r\n\r\n");
        assert_eq!(answer(guard(), &at("/")), "HTTP/1.1 200 OK");
        assert_eq!(answer(guard(), &at("/viewer.js")), "HTTP/1.1 200 OK");
        assert_eq!(answer(guard(), &at("/stream")), "HTTP/1.1 403 Forbidden");
        assert_eq!(
            answer(guard(), &at("/stream?token=s3cre")),
            "HTTP/1.1 403 Forbidden"
        );
        assert_eq!(
            answer(guard(), &at("/stream?token=s3cret0")),
            "HTTP/1.1 403 Forbidden"
        );
        assert_eq!(
            answer(guard(), &at("/stream?view=1&token=s3cret")),
            "HTTP/1.1 400 Bad Request"
        );
    }

    #[test]
    fn only_the_whole_token_parameter_opens_the_stream() {
        let guard = || Guard::Token("s3cret".into());
        let at = |query: &str| format!("GET /stream{query} HTTP/1.1\r\nhost: example:1\r\n\r\n");
        for refused in [
            "?token=",
            "?mytoken=s3cret",
            "?token=bad&token=s3cret",
            "?TOKEN=s3cret",
        ] {
            assert_eq!(
                answer(guard(), &at(refused)),
                "HTTP/1.1 403 Forbidden",
                "{refused}"
            );
        }
        for elsewhere in ["/stream/", "//stream", "/stream%3F", "/Stream"] {
            let request =
                format!("GET {elsewhere}?token=s3cret HTTP/1.1\r\nhost: example:1\r\n\r\n");
            assert_eq!(
                answer(guard(), &request),
                "HTTP/1.1 404 Not Found",
                "{elsewhere}"
            );
        }
    }

    #[test]
    fn the_guard_follows_the_address_and_the_token() {
        let loopback: SocketAddr = "127.0.0.1:6090".parse().unwrap();
        let everywhere: SocketAddr = "0.0.0.0:6090".parse().unwrap();
        assert!(matches!(
            guard_for(loopback, None).unwrap(),
            (Guard::Loopback(6090), false)
        ));
        assert!(
            matches!(guard_for(everywhere, None).unwrap(), (Guard::Token(token), true) if token.len() == 32)
        );
        assert!(
            matches!(guard_for(loopback, Some("chosen")).unwrap(), (Guard::Token(token), false) if &*token == "chosen")
        );
        assert!(matches!(
            guard_for(everywhere, Some("a.b_c~d-9")).unwrap(),
            (Guard::Token(_), false)
        ));
        for unusable in ["", "with space", "per%cent", "a&b", "a#b", "ünï"] {
            assert!(
                guard_for(everywhere, Some(unusable)).is_err(),
                "{unusable:?}"
            );
        }
    }

    #[test]
    fn a_client_that_sends_its_request_too_slowly_is_let_go() {
        let (mut browser, engine) = UnixStream::pair().unwrap();
        browser.write_all(b"GET / HTTP/1.1\r\n").unwrap();
        let started = Instant::now();
        let serving = std::thread::spawn(move || {
            let mut engine = engine;
            read_request(&mut engine).unwrap().is_none()
        });
        std::thread::sleep(Duration::from_millis(100));
        browser.write_all(b"host: example\r\n").unwrap();
        assert!(serving.join().unwrap());
        assert!(started.elapsed() >= HEAD_WAIT);
    }

    #[test]
    fn a_new_token_is_long_and_different_each_time() {
        let (first, second) = (new_token().unwrap(), new_token().unwrap());
        assert_eq!(first.len(), 32);
        assert!(first.chars().all(|character| character.is_ascii_hexdigit()));
        assert_ne!(first, second);
    }

    #[test]
    fn accepts_loopback_hosts_without_an_origin() {
        assert!(local_request(&request(&[("host", "127.0.0.1:6090")]), 6090));
        assert!(local_request(&request(&[("host", "localhost:6090")]), 6090));
    }

    #[test]
    fn accepts_a_page_the_engine_served() {
        let headers = [
            ("host", "127.0.0.1:6090"),
            ("origin", "http://127.0.0.1:6090"),
        ];
        assert!(local_request(&request(&headers), 6090));
    }

    #[test]
    fn refuses_other_origins_and_hosts() {
        let foreign = [
            ("host", "127.0.0.1:6090"),
            ("origin", "https://example.com"),
        ];
        assert!(!local_request(&request(&foreign), 6090));
        let other_port = [
            ("host", "127.0.0.1:6090"),
            ("origin", "http://127.0.0.1:8000"),
        ];
        assert!(!local_request(&request(&other_port), 6090));
        let rebound = [
            ("host", "example.com:6090"),
            ("origin", "http://example.com:6090"),
        ];
        assert!(!local_request(&request(&rebound), 6090));
        assert!(!local_request(
            &request(&[("origin", "http://127.0.0.1:6090")]),
            6090
        ));
        assert!(!local_request(
            &request(&[("host", "127.0.0.1:6091")]),
            6090
        ));
    }
}
