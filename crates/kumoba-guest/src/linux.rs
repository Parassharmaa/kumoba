use base64::{Engine, engine::general_purpose::STANDARD};
use kumoba_protocol::{AGENT_PORT, MAX_DATA, Request, Response, VERSION, receive, send};
use std::{
    io,
    os::{
        fd::{AsRawFd, FromRawFd, OwnedFd},
        unix::fs::OpenOptionsExt,
    },
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt, unix::AsyncFd},
    net::UnixStream,
};

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
    let mut address: libc::sockaddr_vm = unsafe { std::mem::zeroed() };
    address.svm_family = libc::AF_VSOCK as _;
    address.svm_port = AGENT_PORT;
    address.svm_cid = libc::VMADDR_CID_ANY;
    if unsafe {
        libc::bind(
            fd,
            (&address as *const libc::sockaddr_vm).cast(),
            std::mem::size_of_val(&address) as _,
        )
    } < 0
        || unsafe { libc::listen(fd, 32) } < 0
    {
        return Err(io::Error::last_os_error());
    }
    AsyncFd::new(owned)
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

async fn serve(
    stream: &mut UnixStream,
    desktop: std::sync::Arc<tokio::sync::Mutex<crate::desktop::Desktop>>,
) -> io::Result<bool> {
    loop {
        let request: Request = receive(stream).await?;
        match request {
            Request::Hello => {
                reply(
                    stream,
                    &Response::Hello {
                        version: VERSION,
                        arch: std::env::consts::ARCH.into(),
                        desktop: crate::desktop::available(),
                        desktop_startup_timeout: true,
                    },
                )
                .await?
            }
            Request::Exec(mut request) => {
                desktop.lock().await.inherit_environment(&mut request.env);
                crate::execute::exec(stream, request).await?;
            }
            Request::Read { path, max_bytes } => {
                let response = match read_file(&path, max_bytes).await {
                    Ok(data) => Response::Data(STANDARD.encode(data)),
                    Err(e) => Response::Error(e.to_string()),
                };
                reply(stream, &response).await?;
            }
            Request::Write { path, data } => {
                let response = match write_file(&path, &data).await {
                    Ok(()) => Response::Ok,
                    Err(e) => Response::Error(e.to_string()),
                };
                reply(stream, &response).await?;
            }
            Request::Connect { port } => {
                if port == 0 {
                    reply(stream, &Response::Error("invalid port".into())).await?;
                    return Ok(false);
                }
                match tokio::time::timeout(
                    Duration::from_secs(3),
                    tokio::net::TcpStream::connect((std::net::Ipv4Addr::LOCALHOST, port)),
                )
                .await
                {
                    Ok(Ok(mut tcp)) => {
                        reply(stream, &Response::Connected).await?;
                        tokio::io::copy_bidirectional(stream, &mut tcp).await?;
                    }
                    other => {
                        reply(
                            stream,
                            &Response::Error(format!("guest port connection failed: {other:?}")),
                        )
                        .await?;
                    }
                }
                return Ok(false);
            }
            Request::Desktop(request) => {
                let response = match desktop.lock().await.request(request).await {
                    Ok(response) => response,
                    Err(error) => Response::Error(error.to_string()),
                };
                reply(stream, &response).await?;
            }
            Request::Shutdown => {
                return Ok(true);
            }
            Request::ReadAt {
                path,
                offset,
                len,
                user,
            } => {
                let response = match owner_of(user.as_deref()) {
                    Err(e) => Response::Error(e.to_string()),
                    Ok(owner) => match crate::files::read_at(&path, offset, len, owner).await {
                        Ok((data, size)) => Response::Chunk {
                            data: STANDARD.encode(data),
                            size,
                        },
                        Err(e) => Response::Error(e.to_string()),
                    },
                };
                reply(stream, &response).await?;
            }
            Request::WriteAt {
                path,
                offset,
                data,
                truncate,
                user,
            } => {
                let response = match owner_of(user.as_deref()) {
                    Ok(owner) => {
                        match crate::files::write_at(&path, offset, &data, truncate, owner).await {
                            Ok(()) => Response::Ok,
                            Err(e) => Response::Error(e.to_string()),
                        }
                    }
                    Err(e) => Response::Error(e.to_string()),
                };
                reply(stream, &response).await?;
            }
            Request::List { path, user } => {
                let response = match owner_of(user.as_deref())
                    .and_then(|owner| crate::files::list(&path, owner))
                {
                    Ok(entries) => Response::Entries(entries),
                    Err(e) => Response::Error(e.to_string()),
                };
                reply(stream, &response).await?;
            }
        }
    }
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

fn owner_of(user: Option<&str>) -> io::Result<Option<crate::files::Account>> {
    user.map(crate::files::resolve_user)
        .transpose()
        .map(|user| user.map(|user| user.account))
}

fn path_valid(path: &str) -> bool {
    path.starts_with('/')
        && path.len() <= 4096
        && !path.contains('\0')
        && !path.split('/').any(|s| s == "..")
}

async fn read_file(path: &str, max: usize) -> io::Result<Vec<u8>> {
    if !path_valid(path) || max > MAX_DATA {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid file request",
        ));
    }
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)?;
    if !file.metadata()?.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "only regular files are supported",
        ));
    }
    let mut data = Vec::new();
    tokio::fs::File::from_std(file)
        .take((max + 1) as u64)
        .read_to_end(&mut data)
        .await?;
    if data.len() > max {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "file exceeds transfer limit",
        ));
    }
    Ok(data)
}

async fn write_file(path: &str, encoded: &str) -> io::Result<()> {
    if !path_valid(path) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid guest path",
        ));
    }
    let data = STANDARD.decode(encoded).map_err(io::Error::other)?;
    if data.len() > MAX_DATA {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "file exceeds transfer limit",
        ));
    }
    let path = std::path::Path::new(path);
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::other("file has no parent"))?;
    let tmp = tempfile::Builder::new()
        .prefix(".sandbox-write-")
        .tempfile_in(parent)?;
    let mut file = tokio::fs::File::from_std(tmp.as_file().try_clone()?);
    file.write_all(&data).await?;
    file.sync_all().await?;
    std::fs::set_permissions(
        tmp.path(),
        std::os::unix::fs::PermissionsExt::from_mode(0o644),
    )?;
    tokio::fs::rename(tmp.path(), path).await?;
    std::fs::File::open(parent)?.sync_all()
}

async fn reply(stream: &mut UnixStream, response: &Response) -> io::Result<()> {
    send(stream, response).await?;
    let ack = tokio::time::timeout(Duration::from_secs(3), stream.read_u8()).await??;
    if ack != 1 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid response acknowledgement",
        ));
    }
    Ok(())
}
