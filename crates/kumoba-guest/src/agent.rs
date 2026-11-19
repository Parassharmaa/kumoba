use base64::{Engine, engine::general_purpose::STANDARD};
use kumoba_protocol::{MAX_DATA, Request, Response, VERSION, receive, send};
use std::{io, os::unix::fs::OpenOptionsExt, time::Duration};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::UnixStream,
};

pub(crate) struct VsockAddress(libc::sockaddr_vm);

impl VsockAddress {
    pub(crate) fn new(cid: u32, port: u32) -> Self {
        let mut address: libc::sockaddr_vm = unsafe { std::mem::zeroed() };
        #[cfg(target_os = "macos")]
        {
            address.svm_len =
                u8::try_from(size_of::<libc::sockaddr_vm>()).expect("sockaddr_vm fits u8");
        }
        address.svm_family =
            libc::sa_family_t::try_from(libc::AF_VSOCK).expect("AF_VSOCK fits sa_family_t");
        address.svm_cid = cid;
        address.svm_port = port;
        Self(address)
    }

    pub(crate) fn as_ptr(&self) -> *const libc::sockaddr {
        std::ptr::from_ref(&self.0).cast()
    }

    pub(crate) fn size(&self) -> libc::socklen_t {
        libc::socklen_t::try_from(size_of_val(&self.0)).expect("sockaddr_vm fits socklen_t")
    }
}

pub(crate) fn pid_of(child: &tokio::process::Child) -> io::Result<libc::pid_t> {
    let id = child
        .id()
        .ok_or_else(|| io::Error::other("child exited before its process id was read"))?;
    libc::pid_t::try_from(id).map_err(io::Error::other)
}

pub(crate) async fn serve(
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
                        shell: true,
                        os: std::env::consts::OS.into(),
                        clock: true,
                    },
                )
                .await?;
            }
            Request::Exec(mut request) => {
                desktop.lock().await.inherit_environment(&mut request.env);
                crate::execute::exec(stream, request).await?;
            }
            Request::Shell(mut request) => {
                desktop
                    .lock()
                    .await
                    .inherit_environment(&mut request.exec.env);
                crate::shell::run(stream, request).await?;
                return Ok(false);
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
            Request::SetClock { unix_micros } => {
                let response = match set_clock(unix_micros) {
                    Ok(()) => Response::Ok,
                    Err(e) => Response::Error(e.to_string()),
                };
                reply(stream, &response).await?;
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

fn set_clock(unix_micros: u64) -> io::Result<()> {
    let time = libc::timeval {
        tv_sec: (unix_micros / 1_000_000)
            .try_into()
            .map_err(io::Error::other)?,
        tv_usec: (unix_micros % 1_000_000)
            .try_into()
            .map_err(io::Error::other)?,
    };
    if unsafe { libc::settimeofday(&raw const time, std::ptr::null()) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

pub(crate) async fn reply(stream: &mut UnixStream, response: &Response) -> io::Result<()> {
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

#[cfg(target_os = "macos")]
pub(crate) fn initgroups_gid(gid: u32) -> libc::c_int {
    gid.cast_signed()
}

#[cfg(target_os = "linux")]
pub(crate) fn initgroups_gid(gid: u32) -> libc::gid_t {
    gid
}
