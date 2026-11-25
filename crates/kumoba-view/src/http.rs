use crate::remote::Remote;
use crate::session::{self, Settings};
use crate::ws;
#[cfg(target_os = "linux")]
use crate::x11::X11;
use std::collections::HashMap;
use std::io::{self, IsTerminal, Read, Write};
use std::net::{IpAddr, Ipv6Addr, SocketAddr, TcpListener, TcpStream};
use std::os::fd::AsRawFd;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};
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
    fn peer(&self) -> Option<IpAddr>;
}

impl Link for TcpStream {
    fn limit(&self, wait: Option<Duration>) -> io::Result<()> {
        self.set_read_timeout(wait)
    }

    fn streaming(&self) -> io::Result<()> {
        self.set_nonblocking(true)?;
        self.set_nodelay(true)
    }

    fn peer(&self) -> Option<IpAddr> {
        self.peer_addr()
            .ok()
            .map(|address| peer_group(address.ip()))
    }
}

impl Link for UnixStream {
    fn limit(&self, wait: Option<Duration>) -> io::Result<()> {
        self.set_read_timeout(wait)
    }

    fn streaming(&self) -> io::Result<()> {
        self.set_nonblocking(true)
    }

    fn peer(&self) -> Option<IpAddr> {
        None
    }
}

fn peer_group(address: IpAddr) -> IpAddr {
    match address.to_canonical() {
        IpAddr::V6(address) => IpAddr::V6(Ipv6Addr::from(
            u128::from(address) & !(u128::from(u64::MAX)),
        )),
        address @ IpAddr::V4(_) => address,
    }
}

const PAGE: &[u8] = include_bytes!("../viewer/index.html");
const SCRIPT: &[u8] = include_bytes!("../viewer/viewer.js");
const CLIENT: &[u8] = include_bytes!("../viewer/view.js");
const MAX_HEAD: usize = 16 * 1024;
const MAX_WAITING: usize = 64;
const MAX_WAITING_PER_PEER: usize = 32;
const MAX_STREAMS: usize = 16;
const MAX_REFUSING: usize = 16;
const BUSY: &[u8] = b"the live view is busy, try again in a moment\n";

#[derive(Clone, Copy)]
struct Waits {
    first_byte: Duration,
    head: Duration,
    reply: Duration,
    refusal: Duration,
}

const WAITS: Waits = Waits {
    first_byte: Duration::from_secs(2),
    head: Duration::from_secs(5),
    reply: Duration::from_secs(10),
    refusal: Duration::from_secs(1),
};

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

fn read_request(
    stream: &mut impl Link,
    first_byte: Duration,
    whole: Duration,
) -> io::Result<Option<Request>> {
    let opened = Instant::now();
    let (first_byte, whole) = (opened + first_byte, opened + whole);
    let mut head = Vec::new();
    let mut chunk = [0u8; 2048];
    while !head.windows(4).any(|window| window == b"\r\n\r\n") {
        let deadline = if head.is_empty() {
            first_byte.min(whole)
        } else {
            whole
        };
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

fn head_of(status: &str, kind: &str, length: usize) -> String {
    format!(
        "HTTP/1.1 {status}\r\ncontent-type: {kind}\r\ncontent-length: {length}\r\ncache-control: no-store\r\nconnection: close\r\n\r\n"
    )
}

fn wait_writable(stream: &impl Link, left: Duration) -> io::Result<()> {
    let mut socket = libc::pollfd {
        fd: stream.as_raw_fd(),
        events: libc::POLLOUT,
        revents: 0,
    };
    let wait = i32::try_from(left.as_micros().div_ceil(1000)).unwrap_or(i32::MAX);
    if unsafe { libc::poll(&raw mut socket, 1, wait) } < 0 {
        let error = io::Error::last_os_error();
        if error.kind() != io::ErrorKind::Interrupted {
            return Err(error);
        }
    }
    Ok(())
}

fn send(stream: &mut impl Link, mut bytes: &[u8], deadline: Instant) -> io::Result<()> {
    while !bytes.is_empty() {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "the client took too long to read the reply",
            ));
        }
        match stream.write(bytes) {
            Ok(0) => return Err(io::ErrorKind::WriteZero.into()),
            Ok(count) => bytes = &bytes[count..],
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                wait_writable(stream, left)?;
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

fn respond(
    stream: &mut impl Link,
    deadline: Instant,
    status: &str,
    kind: &str,
    body: &[u8],
) -> io::Result<()> {
    send(
        stream,
        head_of(status, kind, body.len()).as_bytes(),
        deadline,
    )?;
    send(stream, body, deadline)
}

fn refuse(mut stream: impl Link, wait: Duration) -> io::Result<()> {
    read_request(&mut stream, wait, wait)?;
    stream.streaming()?;
    respond(
        &mut stream,
        Instant::now() + wait,
        "503 Service Unavailable",
        "text/plain",
        BUSY,
    )
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

#[derive(Default)]
struct Held {
    total: usize,
    by_peer: HashMap<IpAddr, usize>,
}

struct Gate {
    limit: usize,
    per_peer: usize,
    held: Mutex<Held>,
}

impl Gate {
    fn new(limit: usize, per_peer: usize) -> Arc<Self> {
        Arc::new(Self {
            limit,
            per_peer,
            held: Mutex::default(),
        })
    }

    fn take(self: &Arc<Self>, peer: Option<IpAddr>) -> Option<Slot> {
        let mut held = self.held.lock().unwrap_or_else(PoisonError::into_inner);
        if held.total >= self.limit {
            return None;
        }
        if let Some(peer) = peer {
            let mine = held.by_peer.entry(peer).or_default();
            if *mine >= self.per_peer {
                return None;
            }
            *mine += 1;
        }
        held.total += 1;
        Some(Slot {
            gate: Arc::clone(self),
            peer,
        })
    }
}

struct Slot {
    gate: Arc<Gate>,
    peer: Option<IpAddr>,
}

impl Drop for Slot {
    fn drop(&mut self) {
        let mut held = self
            .gate
            .held
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        held.total -= 1;
        if let Some(peer) = self.peer
            && let Some(mine) = held.by_peer.get_mut(&peer)
        {
            *mine -= 1;
            if *mine == 0 {
                held.by_peer.remove(&peer);
            }
        }
    }
}

struct Shared {
    streams: Arc<Gate>,
    guard: Guard,
    settings: Settings,
    source: Source,
    waits: Waits,
}

fn handle(
    mut stream: impl Link,
    waiting: Slot,
    shared: &Shared,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let Some(request) = read_request(&mut stream, shared.waits.first_byte, shared.waits.head)?
    else {
        return Ok(());
    };
    let deadline = Instant::now() + shared.waits.reply;
    stream.streaming()?;
    if !shared.guard.admits(&request) {
        return Ok(respond(
            &mut stream,
            deadline,
            "403 Forbidden",
            "text/plain",
            b"forbidden\n",
        )?);
    }
    if request.method != "GET" {
        return Ok(respond(
            &mut stream,
            deadline,
            "405 Method Not Allowed",
            "text/plain",
            b"method not allowed\n",
        )?);
    }
    match request.path.as_str() {
        "/" | "/index.html" => respond(
            &mut stream,
            deadline,
            "200 OK",
            "text/html; charset=utf-8",
            PAGE,
        )?,
        "/viewer.js" => respond(
            &mut stream,
            deadline,
            "200 OK",
            "text/javascript; charset=utf-8",
            SCRIPT,
        )?,
        "/view.js" => respond(
            &mut stream,
            deadline,
            "200 OK",
            "text/javascript; charset=utf-8",
            CLIENT,
        )?,
        "/stream" => {
            let upgrade = request.header("upgrade").unwrap_or("");
            let Some(key) = request.header("sec-websocket-key") else {
                return Ok(respond(
                    &mut stream,
                    deadline,
                    "400 Bad Request",
                    "text/plain",
                    b"expected a WebSocket\n",
                )?);
            };
            if !upgrade.eq_ignore_ascii_case("websocket") {
                return Ok(respond(
                    &mut stream,
                    deadline,
                    "400 Bad Request",
                    "text/plain",
                    b"expected a WebSocket\n",
                )?);
            }
            let Some(streaming) = shared.streams.take(None) else {
                return Ok(respond(
                    &mut stream,
                    deadline,
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
            send(&mut stream, head.as_bytes(), deadline)?;
            stream.limit(None)?;
            let settings = shared.settings;
            match &shared.source {
                #[cfg(target_os = "linux")]
                Source::X11 => session::run(stream, settings, X11::open)?,
                Source::Remote(path) => session::run(stream, settings, || Remote::open(path))?,
            }
            drop(streaming);
        }
        _ => respond(
            &mut stream,
            deadline,
            "404 Not Found",
            "text/plain",
            b"not found\n",
        )?,
    }
    Ok(())
}

fn accept<L: Link>(incoming: impl Iterator<Item = io::Result<L>>, shared: &Arc<Shared>) {
    let waiting = Gate::new(MAX_WAITING, MAX_WAITING_PER_PEER);
    let refusing = Gate::new(MAX_REFUSING, MAX_REFUSING);
    for stream in incoming {
        let Ok(stream) = stream else { continue };
        let Some(slot) = waiting.take(stream.peer()) else {
            if let Some(refusal) = refusing.take(None) {
                let wait = shared.waits.refusal;
                std::thread::spawn(move || {
                    let _ = refuse(stream, wait);
                    drop(refusal);
                });
            }
            continue;
        };
        let shared = Arc::clone(shared);
        std::thread::spawn(move || {
            if let Err(error) = handle(stream, slot, &shared) {
                eprintln!("kumoba-view: connection ended: {error}");
            }
        });
    }
}

fn shared(guard: Guard, settings: Settings, source: &Source, waits: Waits) -> Arc<Shared> {
    Arc::new(Shared {
        streams: Gate::new(MAX_STREAMS, MAX_STREAMS),
        guard,
        settings,
        source: source.clone(),
        waits,
    })
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
        accept(
            listener.incoming(),
            &shared(Guard::Private, settings, source, WAITS),
        );
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
    accept(listener.incoming(), &shared(guard, settings, source, WAITS));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const QUICK: Waits = Waits {
        first_byte: Duration::from_millis(200),
        head: Duration::from_millis(400),
        reply: Duration::from_millis(300),
        refusal: Duration::from_millis(200),
    };

    fn engine_for(guard: Guard, waits: Waits) -> Arc<Shared> {
        let settings = Settings {
            fps: 30,
            max_mbps: 10,
        };
        shared(
            guard,
            settings,
            &Source::Remote("/nonexistent/view.sock".into()),
            waits,
        )
    }

    fn answer(guard: Guard, request: &str) -> String {
        let (mut browser, engine) = UnixStream::pair().unwrap();
        browser.write_all(request.as_bytes()).unwrap();
        let shared = engine_for(guard, WAITS);
        let serving = std::thread::spawn(move || {
            let slot = Gate::new(MAX_WAITING, MAX_WAITING_PER_PEER)
                .take(None)
                .unwrap();
            handle(engine, slot, &shared).unwrap();
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
        assert_eq!(answer(guard(), &at("/view.js")), "HTTP/1.1 200 OK");
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
            read_request(&mut engine, QUICK.first_byte, QUICK.head)
                .unwrap()
                .is_none()
        });
        std::thread::sleep(QUICK.first_byte + Duration::from_millis(50));
        browser.write_all(b"host: example\r\n").unwrap();
        assert!(serving.join().unwrap());
        assert!(started.elapsed() >= QUICK.head);
    }

    #[test]
    fn a_client_that_sends_nothing_is_let_go_before_the_whole_head_wait() {
        let (_browser, mut engine) = UnixStream::pair().unwrap();
        let started = Instant::now();
        assert!(
            read_request(&mut engine, QUICK.first_byte, QUICK.head)
                .unwrap()
                .is_none()
        );
        let waited = started.elapsed();
        assert!(waited >= QUICK.first_byte, "{waited:?}");
        assert!(waited < QUICK.head, "{waited:?}");
    }

    fn serve_big_reply(engine: UnixStream) -> std::thread::JoinHandle<(io::Result<()>, Duration)> {
        std::thread::spawn(move || {
            let mut engine = engine;
            let body = vec![b'x'; 16 * 1024 * 1024];
            engine.streaming().unwrap();
            let started = Instant::now();
            let sent = respond(
                &mut engine,
                started + QUICK.reply,
                "200 OK",
                "text/plain",
                &body,
            );
            (sent, started.elapsed())
        })
    }

    #[test]
    fn a_reply_nobody_reads_gives_up_at_its_deadline() {
        let (_browser, engine) = UnixStream::pair().unwrap();
        let (sent, took) = serve_big_reply(engine).join().unwrap();
        assert_eq!(sent.unwrap_err().kind(), io::ErrorKind::TimedOut);
        assert!(took >= QUICK.reply, "{took:?}");
        assert!(took < QUICK.reply * 3, "{took:?}");
    }

    #[test]
    fn a_client_that_reads_a_little_at_a_time_cannot_stretch_the_deadline() {
        let (mut browser, engine) = UnixStream::pair().unwrap();
        let serving = serve_big_reply(engine);
        let mut chunk = [0u8; 512];
        while !serving.is_finished() {
            let _ = browser.read(&mut chunk);
            std::thread::sleep(Duration::from_millis(10));
        }
        let (sent, took) = serving.join().unwrap();
        assert_eq!(sent.unwrap_err().kind(), io::ErrorKind::TimedOut);
        assert!(took < QUICK.reply * 3, "{took:?}");
    }

    #[test]
    fn one_address_can_hold_only_part_of_the_waiting_slots() {
        let gate = Gate::new(4, 2);
        let (one, two): (IpAddr, IpAddr) =
            ("10.0.0.1".parse().unwrap(), "10.0.0.2".parse().unwrap());
        let first = [gate.take(Some(one)).unwrap(), gate.take(Some(one)).unwrap()];
        assert!(gate.take(Some(one)).is_none());
        let second = gate.take(Some(two)).unwrap();
        let unaddressed = gate.take(None).unwrap();
        assert!(gate.take(Some(two)).is_none());
        assert!(gate.take(None).is_none());
        drop(unaddressed);
        assert!(gate.take(Some(one)).is_none());
        let again = gate.take(Some(two)).unwrap();
        drop(first);
        assert!(gate.take(Some(one)).is_some());
        drop((second, again));
        assert_eq!(gate.held.lock().unwrap().by_peer.len(), 0);
    }

    #[test]
    fn addresses_in_one_ipv6_network_share_a_limit() {
        let near: IpAddr = "2001:db8:1:2:aaaa::1".parse().unwrap();
        let same: IpAddr = "2001:db8:1:2:bbbb::9".parse().unwrap();
        let other: IpAddr = "2001:db8:1:3::1".parse().unwrap();
        assert_eq!(peer_group(near), peer_group(same));
        assert_ne!(peer_group(near), peer_group(other));
        let mapped: IpAddr = "::ffff:192.0.2.7".parse().unwrap();
        assert_eq!(peer_group(mapped), "192.0.2.7".parse::<IpAddr>().unwrap());
    }

    fn fetch(port: u16) -> io::Result<String> {
        let mut browser = TcpStream::connect(("127.0.0.1", port))?;
        browser.set_read_timeout(Some(Duration::from_secs(5)))?;
        browser.write_all(b"GET / HTTP/1.1\r\nhost: example\r\n\r\n")?;
        let mut reply = Vec::new();
        browser.read_to_end(&mut reply)?;
        Ok(String::from_utf8_lossy(&reply)
            .lines()
            .next()
            .unwrap_or("")
            .to_string())
    }

    #[test]
    fn the_private_socket_shares_only_the_overall_limit() {
        let directory = std::env::temp_dir().join(format!("kumoba-view-{}", std::process::id()));
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join("view.sock");
        let _ = std::fs::remove_file(&path);
        let listener = UnixListener::bind(&path).unwrap();
        let shared = engine_for(Guard::Private, QUICK);
        std::thread::spawn(move || accept(listener.incoming(), &shared));
        let idle: Vec<_> = (0..=MAX_WAITING_PER_PEER)
            .map(|_| UnixStream::connect(&path).unwrap())
            .collect();
        let mut browser = UnixStream::connect(&path).unwrap();
        browser
            .write_all(b"GET / HTTP/1.1\r\nhost: sandbox\r\n\r\n")
            .unwrap();
        let mut reply = String::new();
        browser.read_to_string(&mut reply).unwrap();
        assert!(reply.starts_with("HTTP/1.1 200 OK\r\n"), "{reply}");
        drop(idle);
        let _ = std::fs::remove_dir_all(&directory);
    }

    #[test]
    fn a_client_over_the_limit_hears_busy_and_gets_in_once_the_idle_ones_are_let_go() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let shared = engine_for(Guard::Private, QUICK);
        std::thread::spawn(move || accept(listener.incoming(), &shared));
        let idle: Vec<_> = (0..MAX_WAITING_PER_PEER)
            .map(|_| TcpStream::connect(("127.0.0.1", port)).unwrap())
            .collect();
        let busy = fetch(port).unwrap();
        assert_eq!(busy, "HTTP/1.1 503 Service Unavailable");
        std::thread::sleep(QUICK.first_byte * 2);
        assert_eq!(fetch(port).unwrap(), "HTTP/1.1 200 OK");
        drop(idle);
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
