use std::{
    io::{Read, Write},
    net::TcpListener,
    time::Duration,
};

fn main() -> std::io::Result<()> {
    let listener = TcpListener::bind("127.0.0.1:8080")?;
    std::fs::write("/workspace/http-ready", b"ready")?;
    for stream in listener.incoming() {
        let mut stream = stream?;
        stream.set_read_timeout(Some(Duration::from_secs(2)))?;
        let mut request = [0u8; 2048];
        if stream.read(&mut request).is_err() {
            continue;
        }
        let body = std::fs::read("/workspace/index.html")?;
        write!(
            stream,
            "HTTP/1.0 200 OK\r\nContent-Length: {}\r\n\r\n",
            body.len()
        )?;
        stream.write_all(&body)?;
    }
    Ok(())
}
