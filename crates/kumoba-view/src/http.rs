use crate::session::{self, Settings};
use crate::ws;
use std::io::{self, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

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

fn read_request(stream: &mut TcpStream) -> io::Result<Option<Request>> {
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

fn respond(stream: &mut TcpStream, status: &str, kind: &str, body: &[u8]) -> io::Result<()> {
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
    mut stream: TcpStream,
    port: u16,
    settings: Settings,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    stream.set_read_timeout(Some(Duration::from_secs(10)))?;
    let Some(request) = read_request(&mut stream)? else {
        return Ok(());
    };
    if !local_request(&request, port) {
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
            stream.set_read_timeout(None)?;
            session::run(stream, settings)?;
        }
        _ => respond(&mut stream, "404 Not Found", "text/plain", b"not found\n")?,
    }
    Ok(())
}

pub fn serve(listen: &str, settings: Settings) -> io::Result<()> {
    let listener = TcpListener::bind(listen)?;
    let address = listener.local_addr()?;
    if !address.ip().is_loopback() {
        return Err(io::Error::other("kumoba-view listens on loopback only"));
    }
    let port = address.port();
    println!("kumoba-view listening on {address}");
    let open = Arc::new(AtomicUsize::new(0));
    for stream in listener.incoming() {
        let Ok(stream) = stream else { continue };
        if open.fetch_add(1, Ordering::SeqCst) >= MAX_CONNECTIONS {
            open.fetch_sub(1, Ordering::SeqCst);
            continue;
        }
        let open = Arc::clone(&open);
        std::thread::spawn(move || {
            if let Err(error) = handle(stream, port, settings) {
                eprintln!("kumoba-view: connection ended: {error}");
            }
            open.fetch_sub(1, Ordering::SeqCst);
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

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
