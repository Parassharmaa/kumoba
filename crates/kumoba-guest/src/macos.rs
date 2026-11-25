use crate::agent::{VsockAddress, reply, serve};
use kumoba_protocol::{AGENT_PORT, Response};
use std::{
    fs::File,
    io,
    os::{
        fd::{AsRawFd, FromRawFd, OwnedFd},
        unix::net::UnixStream as StdUnixStream,
    },
    time::Duration,
};
use tokio::{net::UnixStream, sync::mpsc};

fn socket() -> io::Result<OwnedFd> {
    let fd = unsafe { libc::socket(libc::AF_VSOCK, libc::SOCK_STREAM, 0) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    let fd = unsafe { OwnedFd::from_raw_fd(fd) };
    if unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_SETFD, libc::FD_CLOEXEC) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(fd)
}

fn pump(from: OwnedFd, to: OwnedFd) {
    let mut reader = File::from(from);
    let mut writer = File::from(to);
    let _ = io::copy(&mut reader, &mut writer);
    unsafe { libc::shutdown(writer.as_raw_fd(), libc::SHUT_WR) };
}

fn bridged(vsock: OwnedFd) -> io::Result<StdUnixStream> {
    let (inside, outside) = StdUnixStream::pair()?;
    let vsock_writer = vsock.try_clone()?;
    let inside_writer = OwnedFd::from(inside.try_clone()?);
    let inside = OwnedFd::from(inside);
    std::thread::spawn(move || pump(vsock, inside_writer));
    std::thread::spawn(move || pump(inside, vsock_writer));
    Ok(outside)
}

fn into_tokio(stream: StdUnixStream) -> io::Result<UnixStream> {
    stream.set_nonblocking(true)?;
    UnixStream::from_std(stream)
}

pub(crate) async fn connect_host(port: u32) -> io::Result<UnixStream> {
    let stream = tokio::task::spawn_blocking(move || {
        let fd = socket()?;
        let address = VsockAddress::new(libc::VMADDR_CID_HOST, port);
        if unsafe { libc::connect(fd.as_raw_fd(), address.as_ptr(), address.size()) } < 0 {
            return Err(io::Error::last_os_error());
        }
        bridged(fd)
    })
    .await
    .map_err(io::Error::other)??;
    into_tokio(stream)
}

fn listen(connections: &mpsc::Sender<StdUnixStream>) -> io::Result<()> {
    let listener = socket()?;
    let address = VsockAddress::new(libc::VMADDR_CID_ANY, AGENT_PORT);
    if unsafe { libc::bind(listener.as_raw_fd(), address.as_ptr(), address.size()) } < 0
        || unsafe { libc::listen(listener.as_raw_fd(), 32) } < 0
    {
        return Err(io::Error::last_os_error());
    }
    loop {
        let fd = unsafe {
            libc::accept(
                listener.as_raw_fd(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        };
        if fd < 0 {
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(error);
        }
        let fd = unsafe { OwnedFd::from_raw_fd(fd) };
        if unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_SETFD, libc::FD_CLOEXEC) } < 0 {
            continue;
        }
        if connections.blocking_send(bridged(fd)?).is_err() {
            return Ok(());
        }
    }
}

pub async fn run() -> io::Result<()> {
    crate::files::umask();
    let desktop = std::sync::Arc::new(tokio::sync::Mutex::new(crate::desktop::Desktop));
    let mut tasks = tokio::task::JoinSet::new();
    let proxy = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 3128)).await?;
    tasks.spawn(crate::egress::run(proxy));
    let (connections, mut incoming) = mpsc::channel(64);
    std::thread::spawn(move || {
        loop {
            if let Err(error) = listen(&connections) {
                eprintln!("agent: vsock listener: {error}");
            }
            if connections.is_closed() {
                return;
            }
            std::thread::sleep(Duration::from_secs(1));
        }
    });
    let (shutdown_tx, mut shutdown_rx) = mpsc::channel(1);
    let mut shutdown_stream = loop {
        tokio::select! {
            Some(stream) = shutdown_rx.recv() => break stream,
            Some(connection) = incoming.recv(), if tasks.len() < 64 => {
                let mut stream = into_tokio(connection)?;
                let shutdown = shutdown_tx.clone();
                let desktop = desktop.clone();
                tasks.spawn(async move {
                    match serve(&mut stream, desktop).await {
                        Ok(true) => { let _ = shutdown.send(stream).await; }
                        Ok(false) => {}
                        Err(error) => {
                            eprintln!("agent: {error:?}");
                            let _ = reply(&mut stream, &Response::Error(format!("agent error: {error:?}"))).await;
                        }
                    }
                });
            }
            result = tasks.join_next(), if !tasks.is_empty() => {
                if let Some(Err(error)) = result { eprintln!("agent task failed: {error:?}"); }
            }
        }
    };
    unsafe { libc::sync() };
    reply(&mut shutdown_stream, &Response::Ok).await?;
    tokio::process::Command::new("/sbin/shutdown")
        .args(["-h", "now"])
        .status()
        .await?;
    std::future::pending().await
}
