use crate::agent::{VsockAddress, reply, serve};
use kumoba_protocol::{AGENT_PORT, Response};
use std::{
    io,
    os::fd::{AsRawFd, FromRawFd, OwnedFd},
};
use tokio::{io::unix::AsyncFd, net::UnixStream};

fn listener() -> io::Result<AsyncFd<OwnedFd>> {
    let fd = unsafe {
        libc::socket(
            libc::AF_VSOCK,
            libc::SOCK_STREAM | libc::SOCK_CLOEXEC | libc::SOCK_NONBLOCK,
            0,
        )
    };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    let owned = unsafe { OwnedFd::from_raw_fd(fd) };
    let address = VsockAddress::new(libc::VMADDR_CID_ANY, AGENT_PORT);
    if unsafe { libc::bind(fd, address.as_ptr(), address.size()) } < 0
        || unsafe { libc::listen(fd, 32) } < 0
    {
        return Err(io::Error::last_os_error());
    }
    Ok(unsafe { AsyncFd::register(owned) }?)
}

async fn accept(listener: &AsyncFd<OwnedFd>) -> io::Result<UnixStream> {
    loop {
        let mut ready = listener.readable().await?;
        let result = ready.try_io(|inner| {
            let fd = unsafe {
                libc::accept4(
                    inner.as_raw_fd(),
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    libc::SOCK_NONBLOCK | libc::SOCK_CLOEXEC,
                )
            };
            if fd < 0 {
                Err(io::Error::last_os_error())
            } else {
                Ok(unsafe { std::os::unix::net::UnixStream::from_raw_fd(fd) })
            }
        });
        match result {
            Ok(Ok(stream)) => return UnixStream::from_std(stream),
            Ok(Err(error)) if error.kind() == io::ErrorKind::Interrupted => continue,
            Ok(Err(error)) => return Err(error),
            Err(_) => continue,
        }
    }
}

pub async fn run() -> io::Result<()> {
    crate::files::umask();
    let scope = crate::shutdown::Scope::capture()?;
    std::fs::create_dir_all("/workspace")?;
    ensure_hosts()?;
    crate::desktop::reset()?;
    let desktop = std::sync::Arc::new(tokio::sync::Mutex::new(crate::desktop::Desktop::default()));
    let listener = listener()?;
    let (shutdown_tx, mut shutdown_rx) = tokio::sync::mpsc::channel(1);
    let mut tasks = tokio::task::JoinSet::new();
    if std::env::var("KUMOBA_EGRESS").as_deref() == Ok("1") {
        let proxy = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 3128)).await?;
        tasks.spawn(crate::egress::run(proxy));
    }
    let mut shutdown_stream = loop {
        tokio::select! {
            Some(stream) = shutdown_rx.recv() => break stream,
            connection = accept(&listener), if tasks.len() < 64 => {
                let mut stream = connection?;
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
    tasks.abort_all();
    while tasks.join_next().await.is_some() {}
    desktop
        .lock()
        .await
        .request(kumoba_protocol::desktop::DesktopRequest::Stop)
        .await?;
    scope.quiesce().await?;
    unsafe {
        libc::sync();
    }
    reply(&mut shutdown_stream, &Response::Ok).await?;
    Ok(())
}

fn ensure_hosts() -> io::Result<()> {
    let current = std::fs::read_to_string("/etc/hosts").unwrap_or_default();
    if current.lines().any(|line| {
        line.split_whitespace()
            .skip(1)
            .any(|name| name == "localhost")
    }) {
        return Ok(());
    }
    std::fs::write("/etc/hosts", "127.0.0.1 localhost sandbox\n::1 localhost\n")
}
