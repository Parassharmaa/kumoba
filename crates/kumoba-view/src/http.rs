use crate::remote::Remote;
use crate::session::{self, Settings};
use crate::ws;
#[cfg(target_os = "linux")]
use crate::x11::X11;
use std::io::{self, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::os::fd::AsRawFd;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

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
const MAX_CONNECTIONS: usize = 32;

struct Request {
    method: String,
    path: String,
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

fn read_request(stream: &mut impl Read) -> io::Result<Option<Request>> {
    let mut head = Vec::new();
    let mut chunk = [0u8; 2048];
    while !head.windows(4).any(|window| window == b"\r\n\r\n") {
        let count = stream.read(&mut chunk)?;
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
    let path = target.split('?').next().unwrap_or("/").to_string();
    let headers = lines
        .filter_map(|line| line.split_once(':'))
        .map(|(key, value)| (key.trim().to_ascii_lowercase(), value.trim().to_string()))
        .collect();
    Ok(Some(Request {
        method: method.to_string(),
        path,
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

fn handle(
    mut stream: impl Link,
    port: Option<u16>,
    settings: Settings,
    source: &Source,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    stream.limit(Some(Duration::from_secs(10)))?;
    let Some(request) = read_request(&mut stream)? else {
        return Ok(());
    };
    if port.is_some_and(|port| !local_request(&request, port)) {
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
        }
        _ => respond(&mut stream, "404 Not Found", "text/plain", b"not found\n")?,
    }
    Ok(())
}

fn accept<L: Link>(
    incoming: impl Iterator<Item = io::Result<L>>,
    port: Option<u16>,
    settings: Settings,
    source: &Source,
) {
    let open = Arc::new(AtomicUsize::new(0));
    for stream in incoming {
        let Ok(stream) = stream else { continue };
        if open.fetch_add(1, Ordering::SeqCst) >= MAX_CONNECTIONS {
            open.fetch_sub(1, Ordering::SeqCst);
            continue;
        }
        let open = Arc::clone(&open);
        let source = source.clone();
        std::thread::spawn(move || {
            if let Err(error) = handle(stream, port, settings, &source) {
                eprintln!("kumoba-view: connection ended: {error}");
            }
            open.fetch_sub(1, Ordering::SeqCst);
        });
    }
}

pub fn serve(listen: &str, settings: Settings, source: &Source) -> io::Result<()> {
    if let Some(path) = listen.strip_prefix("unix:") {
        let _ = std::fs::remove_file(path);
        let listener = UnixListener::bind(path)?;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
        println!("kumoba-view listening on {path}");
        accept(listener.incoming(), None, settings, source);
        return Ok(());
    }
    let listener = TcpListener::bind(listen)?;
    let address = listener.local_addr()?;
    if !address.ip().is_loopback() {
        return Err(io::Error::other("kumoba-view listens on loopback only"));
    }
    println!("kumoba-view listening on {address}");
    accept(listener.incoming(), Some(address.port()), settings, source);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn answer(port: Option<u16>, request: &str) -> String {
        let (mut browser, engine) = UnixStream::pair().unwrap();
        browser.write_all(request.as_bytes()).unwrap();
        let settings = Settings {
            fps: 30,
            max_mbps: 10,
        };
        let source = Source::Remote("/nonexistent/view.sock".into());
        handle(engine, port, settings, &source).unwrap();
        let mut reply = String::new();
        browser.read_to_string(&mut reply).unwrap();
        reply.lines().next().unwrap_or("").to_string()
    }

    #[test]
    fn a_private_socket_serves_any_host_and_a_port_serves_only_its_own() {
        let page = "GET / HTTP/1.1\r\nhost: sandbox.example\r\n\r\n";
        assert_eq!(answer(None, page), "HTTP/1.1 200 OK");
        assert_eq!(answer(Some(6090), page), "HTTP/1.1 403 Forbidden");
        let own = "GET / HTTP/1.1\r\nhost: 127.0.0.1:6090\r\n\r\n";
        assert_eq!(answer(Some(6090), own), "HTTP/1.1 200 OK");
        assert_eq!(
            answer(None, "POST / HTTP/1.1\r\n\r\n"),
            "HTTP/1.1 405 Method Not Allowed"
        );
    }

    fn request(headers: &[(&str, &str)]) -> Request {
        Request {
            method: "GET".into(),
            path: "/stream".into(),
            headers: headers
                .iter()
                .map(|(name, value)| ((*name).to_string(), (*value).to_string()))
                .collect(),
        }
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
