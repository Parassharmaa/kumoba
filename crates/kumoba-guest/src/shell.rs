use base64::{Engine, engine::general_purpose::STANDARD};
use kumoba_protocol::{Response, ShellInput, ShellOutput, ShellRequest, receive, send};
use std::{
    io,
    os::{
        fd::{AsRawFd, FromRawFd, OwnedFd},
        unix::process::ExitStatusExt,
    },
    process::Stdio,
    time::Duration,
};
use tokio::{io::unix::AsyncFd, net::UnixStream, process::Child};

const DRAIN_AFTER_EXIT: Duration = Duration::from_millis(150);
const DEFAULT_TERM: &str = "xterm-256color";

pub(crate) async fn run(stream: &mut UnixStream, request: ShellRequest) -> io::Result<()> {
    match start(request) {
        Ok((master, child)) => {
            send(stream, &Response::Ok).await?;
            relay(stream, master, child).await
        }
        Err(error) => send(stream, &Response::Error(error.to_string())).await,
    }
}

fn login_shell(user: Option<&crate::files::User>) -> String {
    let name = user.map_or("root", |user| user.name.as_str());
    std::fs::read_to_string("/etc/passwd")
        .ok()
        .and_then(|passwd| {
            passwd.lines().find_map(|line| {
                let fields: Vec<&str> = line.split(':').collect();
                (fields.len() >= 7 && fields[0] == name).then(|| fields[6].to_string())
            })
        })
        .filter(|shell| shell.starts_with('/') && std::path::Path::new(shell).is_file())
        .unwrap_or_else(|| "/bin/sh".to_string())
}

fn window(rows: u16, cols: u16) -> libc::winsize {
    libc::winsize {
        ws_row: rows.max(1),
        ws_col: cols.max(1),
        ws_xpixel: 0,
        ws_ypixel: 0,
    }
}

fn start(request: ShellRequest) -> io::Result<(AsyncFd<OwnedFd>, Child)> {
    let exec = request.exec;
    let user = exec
        .user
        .as_deref()
        .map(crate::files::resolve_user)
        .transpose()?;
    let (program, args) = if request.login {
        (login_shell(user.as_ref()), vec!["-l".to_string()])
    } else {
        (exec.program, exec.args)
    };
    let size = window(request.rows, request.cols);
    let (mut master, mut slave) = (-1, -1);
    if unsafe {
        libc::openpty(
            &raw mut master,
            &raw mut slave,
            std::ptr::null_mut(),
            std::ptr::null(),
            &raw const size,
        )
    } != 0
    {
        return Err(io::Error::last_os_error());
    }
    let master = unsafe { OwnedFd::from_raw_fd(master) };
    let slave = unsafe { OwnedFd::from_raw_fd(slave) };
    let mut command = tokio::process::Command::new(&program);
    command.env_clear().envs(exec.env);
    if let Some(user) = &user {
        if unsafe { libc::fchown(slave.as_raw_fd(), user.account.uid, user.account.gid) } != 0 {
            return Err(io::Error::last_os_error());
        }
        command
            .env("HOME", &user.home)
            .env("USER", &user.name)
            .env("LOGNAME", &user.name);
    }
    command
        .env("SHELL", &program)
        .env_remove("TERM")
        .env(
            "TERM",
            std::env::var("KUMOBA_TERM").unwrap_or_else(|_| DEFAULT_TERM.into()),
        )
        .args(args)
        .current_dir(&exec.cwd)
        .stdin(Stdio::from(slave.try_clone()?))
        .stdout(Stdio::from(slave.try_clone()?))
        .stderr(Stdio::from(slave))
        .kill_on_drop(true);
    let account = user
        .as_ref()
        .map(|user| {
            std::ffi::CString::new(user.name.clone())
                .map(|name| (name, user.account))
                .map_err(io::Error::other)
        })
        .transpose()?;
    unsafe {
        command.pre_exec(move || {
            if libc::setsid() < 0 || libc::ioctl(0, libc::TIOCSCTTY as _, 0) < 0 {
                return Err(io::Error::last_os_error());
            }
            if let Some((name, account)) = &account
                && (libc::initgroups(name.as_ptr(), account.gid as _) != 0
                    || libc::setgid(account.gid) != 0
                    || libc::setuid(account.uid) != 0)
            {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let child = command.spawn()?;
    drop(command);
    let flags = unsafe { libc::fcntl(master.as_raw_fd(), libc::F_GETFL) };
    if flags < 0
        || unsafe { libc::fcntl(master.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0
    {
        return Err(io::Error::last_os_error());
    }
    let master = unsafe { AsyncFd::register(master) }?;
    Ok((master, child))
}

async fn read_chunk(master: &AsyncFd<OwnedFd>, buffer: &mut [u8]) -> io::Result<Option<usize>> {
    loop {
        let mut guard = master.readable().await?;
        let result = guard.try_io(|fd| {
            let read =
                unsafe { libc::read(fd.as_raw_fd(), buffer.as_mut_ptr().cast(), buffer.len()) };
            usize::try_from(read).map_err(|_| io::Error::last_os_error())
        });
        match result {
            Ok(Ok(0)) => return Ok(None),
            Ok(Ok(read)) => return Ok(Some(read)),
            Ok(Err(error)) if error.raw_os_error() == Some(libc::EIO) => return Ok(None),
            Ok(Err(error)) => return Err(error),
            Err(_) => {}
        }
    }
}

async fn write_all(master: &AsyncFd<OwnedFd>, mut data: &[u8]) -> io::Result<()> {
    while !data.is_empty() {
        let mut guard = master.writable().await?;
        if let Ok(result) = guard.try_io(|fd| {
            let written = unsafe { libc::write(fd.as_raw_fd(), data.as_ptr().cast(), data.len()) };
            usize::try_from(written).map_err(|_| io::Error::last_os_error())
        }) {
            data = &data[result?..];
        }
    }
    Ok(())
}

async fn relay(
    stream: &mut UnixStream,
    master: AsyncFd<OwnedFd>,
    mut child: Child,
) -> io::Result<()> {
    let group = crate::linux::pid_of(&child)?;
    let (mut reader, mut writer) = stream.split();
    let master = &master;
    let input = async move {
        while let Ok(input) = receive::<_, ShellInput>(&mut reader).await {
            match input {
                ShellInput::Data(data) => {
                    let bytes = STANDARD.decode(data).map_err(io::Error::other)?;
                    write_all(master, &bytes).await?;
                }
                ShellInput::Resize { rows, cols } => {
                    let size = window(rows, cols);
                    unsafe { libc::ioctl(master.as_raw_fd(), libc::TIOCSWINSZ, &raw const size) };
                }
                ShellInput::Eof => write_all(master, b"\x04").await?,
            }
        }
        unsafe { libc::kill(-group, libc::SIGHUP) };
        Ok::<(), io::Error>(())
    };
    let output = async move {
        let mut buffer = vec![0u8; 16 * 1024];
        let status = loop {
            tokio::select! {
                status = child.wait() => break status?,
                read = read_chunk(master, &mut buffer) => match read? {
                    Some(read) => {
                        send(&mut writer, &ShellOutput::Data(STANDARD.encode(&buffer[..read]))).await?;
                    }
                    None => break child.wait().await?,
                },
            }
        };
        while let Ok(Ok(Some(read))) =
            tokio::time::timeout(DRAIN_AFTER_EXIT, read_chunk(master, &mut buffer)).await
        {
            send(
                &mut writer,
                &ShellOutput::Data(STANDARD.encode(&buffer[..read])),
            )
            .await?;
        }
        send(
            &mut writer,
            &ShellOutput::Exit {
                code: status.code(),
                signal: status.signal(),
            },
        )
        .await
    };
    let (input, output) = tokio::join!(input, output);
    unsafe { libc::kill(-group, libc::SIGHUP) };
    output.and(input)
}
