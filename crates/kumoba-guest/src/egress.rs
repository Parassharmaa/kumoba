use std::{
    io,
    os::fd::{AsRawFd, FromRawFd, IntoRawFd, OwnedFd},
    time::Duration,
};
use tokio::net::{TcpListener, UnixStream};

async fn connect() -> io::Result<UnixStream> {
    let fd = unsafe {
        libc::socket(
            libc::AF_VSOCK,
            libc::SOCK_STREAM | libc::SOCK_NONBLOCK | libc::SOCK_CLOEXEC,
            0,
        )
    };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    let fd = unsafe { OwnedFd::from_raw_fd(fd) };
    let address =
        crate::linux::VsockAddress::new(libc::VMADDR_CID_HOST, kumoba_protocol::EGRESS_PORT);
    let result = unsafe { libc::connect(fd.as_raw_fd(), address.as_ptr(), address.size()) };
    if result < 0 {
        let error = io::Error::last_os_error();
        if !matches!(error.raw_os_error(), Some(libc::EINPROGRESS | libc::EINTR)) {
            return Err(error);
        }
    }
    let stream = UnixStream::from_std(unsafe {
        std::os::unix::net::UnixStream::from_raw_fd(fd.into_raw_fd())
    })?;
    stream.writable().await?;
    if let Some(error) = stream.take_error()? {
        return Err(error);
    }
    Ok(stream)
}

pub(crate) async fn run(listener: TcpListener) {
    let mut connections = tokio::task::JoinSet::new();
    loop {
        tokio::select! {
            result = listener.accept(), if connections.len() < 32 => {
                match result {
                    Ok((mut tcp, _)) => { connections.spawn(async move {
                        if let Ok(Ok(mut vsock)) = tokio::time::timeout(Duration::from_secs(5), connect()).await {
                            let _ = tokio::io::copy_bidirectional(&mut tcp, &mut vsock).await;
                        }
                    }); }
                    Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                    Err(_) => return,
                }
            }
            _ = connections.join_next(), if !connections.is_empty() => {}
        }
    }
}
