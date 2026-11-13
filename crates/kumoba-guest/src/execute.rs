use base64::{Engine, engine::general_purpose::STANDARD};
use kumoba_protocol::{
    ExecEvent, ExecRequest, MAX_DATA, MAX_OUTPUT_CHUNK, MAX_OUTPUT_LIMIT, OutputKind, send,
};
use std::{io, os::unix::process::ExitStatusExt, process::Stdio, time::Duration};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWriteExt},
    net::UnixStream,
    sync::mpsc,
};

const OUTPUT_QUEUE: usize = 16;
const TERMINAL_ACK_TIMEOUT: Duration = Duration::from_secs(3);
const DETACH_DRAIN: Duration = Duration::from_millis(250);

struct OutputChunk {
    stream: OutputKind,
    bytes: Vec<u8>,
}

struct ProcessGroup {
    pid: i32,
    armed: bool,
}

impl Drop for ProcessGroup {
    fn drop(&mut self) {
        if self.armed {
            unsafe { libc::kill(-self.pid, libc::SIGKILL) };
        }
    }
}

fn path_valid(path: &str) -> bool {
    path.starts_with('/')
        && path.len() <= 4096
        && !path.contains('\0')
        && !path.split('/').any(|part| part == "..")
}

fn validate(request: &ExecRequest) -> io::Result<Vec<u8>> {
    if !path_valid(&request.program)
        || !path_valid(&request.cwd)
        || request.timeout_ms == 0
        || request.timeout_ms > 3_600_000
        || request.output_limit == 0
        || request.output_limit > MAX_OUTPUT_LIMIT
        || request.stdin.len() > MAX_DATA.div_ceil(3) * 4
        || request.args.len() > 1024
        || request.args.iter().any(|arg| arg.contains('\0'))
        || request.env.iter().any(|(name, value)| {
            name.is_empty() || name.contains(['=', '\0']) || value.contains('\0')
        })
        || request.args.iter().map(String::len).sum::<usize>()
            + request
                .env
                .iter()
                .map(|(name, value)| name.len() + value.len())
                .sum::<usize>()
            > 128 * 1024
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid command limits, arguments, environment or paths",
        ));
    }
    let input = STANDARD.decode(&request.stdin).map_err(io::Error::other)?;
    if input.len() > MAX_DATA {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "command input exceeds limit",
        ));
    }
    Ok(input)
}

async fn pump(
    mut reader: impl AsyncRead + Unpin,
    stream: OutputKind,
    limit: usize,
    sender: mpsc::Sender<OutputChunk>,
) -> io::Result<bool> {
    let mut buffer = [0; MAX_OUTPUT_CHUNK];
    let mut forwarded = 0;
    let mut truncated = false;
    loop {
        let count = reader.read(&mut buffer).await?;
        if count == 0 {
            return Ok(truncated);
        }
        let keep = count.min(limit.saturating_sub(forwarded));
        truncated |= keep != count;
        if keep != 0 {
            sender
                .send(OutputChunk {
                    stream,
                    bytes: buffer[..keep].to_vec(),
                })
                .await
                .map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "output writer ended"))?;
            forwarded += keep;
        }
    }
}

async fn event(stream: &mut UnixStream, event: &ExecEvent, wire_idle: &mut bool) -> io::Result<()> {
    *wire_idle = false;
    send(stream, event).await?;
    if stream.read_u8().await? != 1 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid execution event acknowledgement",
        ));
    }
    *wire_idle = true;
    Ok(())
}

async fn terminal(
    stream: &mut UnixStream,
    terminal: &ExecEvent,
    wire_idle: &mut bool,
) -> io::Result<()> {
    tokio::time::timeout(TERMINAL_ACK_TIMEOUT, event(stream, terminal, wire_idle))
        .await
        .map_err(|_| {
            io::Error::new(
                io::ErrorKind::TimedOut,
                "terminal acknowledgement timed out",
            )
        })?
}

async fn write_events(
    stream: &mut UnixStream,
    mut receiver: mpsc::Receiver<OutputChunk>,
    wire_idle: &mut bool,
) -> io::Result<()> {
    loop {
        let chunk = tokio::select! {
            chunk = receiver.recv() => chunk,
            received = stream.read_u8() => {
                return Err(match received {
                    Ok(_) => io::Error::new(io::ErrorKind::InvalidData, "unsolicited execution input"),
                    Err(error) => error,
                });
            }
        };
        let Some(chunk) = chunk else {
            return Ok(());
        };
        event(
            stream,
            &ExecEvent::Output {
                stream: chunk.stream,
                data: STANDARD.encode(chunk.bytes),
            },
            wire_idle,
        )
        .await?;
    }
}

pub(crate) async fn exec(stream: &mut UnixStream, request: ExecRequest) -> io::Result<()> {
    let mut wire_idle = true;
    let input_bytes = match validate(&request) {
        Ok(input) => input,
        Err(error) => {
            return terminal(
                stream,
                &ExecEvent::Failed {
                    message: error.to_string(),
                },
                &mut wire_idle,
            )
            .await;
        }
    };
    let user = match request
        .user
        .as_deref()
        .map(crate::files::resolve_user)
        .transpose()
    {
        Ok(user) => user,
        Err(error) => {
            return terminal(
                stream,
                &ExecEvent::Failed {
                    message: error.to_string(),
                },
                &mut wire_idle,
            )
            .await;
        }
    };
    let mut command = tokio::process::Command::new(request.program);
    command.env_clear();
    if let Some(user) = &user {
        command
            .env("HOME", &user.home)
            .env("USER", &user.name)
            .env("LOGNAME", &user.name);
        let name = std::ffi::CString::new(user.name.clone()).map_err(io::Error::other)?;
        let account = user.account;
        unsafe {
            command.pre_exec(move || {
                if libc::initgroups(name.as_ptr(), account.gid as _) != 0
                    || libc::setgid(account.gid) != 0
                    || libc::setuid(account.uid) != 0
                {
                    return Err(io::Error::last_os_error());
                }
                Ok(())
            });
        }
    }
    command
        .args(request.args)
        .envs(request.env)
        .current_dir(request.cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .process_group(0);
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) => {
            return terminal(
                stream,
                &ExecEvent::Failed {
                    message: error.to_string(),
                },
                &mut wire_idle,
            )
            .await;
        }
    };
    let pid = match crate::linux::pid_of(&child) {
        Ok(pid) => pid,
        Err(error) => {
            return terminal(
                stream,
                &ExecEvent::Failed {
                    message: error.to_string(),
                },
                &mut wire_idle,
            )
            .await;
        }
    };
    let mut group = ProcessGroup { pid, armed: true };
    let mut input = child.stdin.take().unwrap();
    let stdout = child.stdout.take().unwrap();
    let stderr = child.stderr.take().unwrap();
    let (sender, receiver) = mpsc::channel(OUTPUT_QUEUE);
    let stderr_sender = sender.clone();
    let completed = async {
        let limit = request.output_limit;
        let work = async {
            let out = tokio::spawn(pump(stdout, OutputKind::Stdout, limit, sender));
            let err = tokio::spawn(pump(stderr, OutputKind::Stderr, limit, stderr_sender));
            let feed = tokio::spawn(async move {
                let _ = input.write_all(&input_bytes).await;
            });
            let status = child.wait().await?;
            feed.abort();
            let (out_abort, err_abort) = (out.abort_handle(), err.abort_handle());
            let drained = tokio::time::timeout(DETACH_DRAIN, async {
                let out = out.await.map_err(io::Error::other)??;
                let err = err.await.map_err(io::Error::other)??;
                Ok::<_, io::Error>((out, err))
            })
            .await;
            let (out_truncated, err_truncated) = match drained {
                Ok(result) => result?,
                Err(_) => {
                    out_abort.abort();
                    err_abort.abort();
                    (false, false)
                }
            };
            Ok::<_, io::Error>((out_truncated, err_truncated, (), status))
        };
        let ((out_truncated, err_truncated, (), status), ()) =
            tokio::try_join!(work, write_events(stream, receiver, &mut wire_idle))?;
        event(
            stream,
            &ExecEvent::Finished {
                code: status.code(),
                signal: status.signal(),
                timed_out: false,
                truncated: out_truncated || err_truncated,
            },
            &mut wire_idle,
        )
        .await
    };
    let outcome = tokio::select! {
        outcome = completed => Some(outcome),
        _ = tokio::time::sleep(Duration::from_millis(request.timeout_ms)) => None,
    };
    match outcome {
        Some(Ok(())) => {
            group.armed = false;
            Ok(())
        }
        outcome => {
            unsafe { libc::kill(-group.pid, libc::SIGKILL) };
            let _ = tokio::time::timeout(Duration::from_secs(1), child.wait()).await;
            if outcome.is_none() && wire_idle {
                let _ = terminal(
                    stream,
                    &ExecEvent::Finished {
                        code: None,
                        signal: None,
                        timed_out: true,
                        truncated: true,
                    },
                    &mut wire_idle,
                )
                .await;
            }
            let _ = stream.shutdown().await;
            match outcome {
                Some(Err(error)) => Err(error),
                None => Ok(()),
                Some(Ok(())) => unreachable!(),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kumoba_protocol::{MAX_EXEC_FRAME, Request, receive, receive_limited};
    use std::collections::BTreeMap;

    fn shell(script: &str, output_limit: usize, timeout_ms: u64) -> ExecRequest {
        ExecRequest {
            program: "/bin/sh".into(),
            args: vec!["-c".into(), script.into()],
            cwd: "/tmp".into(),
            env: BTreeMap::new(),
            stdin: String::new(),
            timeout_ms,
            output_limit,
            user: None,
        }
    }

    #[tokio::test]
    async fn caps_each_stream_and_hands_back_terminal_ownership() {
        let (mut host, mut guest) = UnixStream::pair().unwrap();
        let runner = tokio::spawn(async move {
            exec(
                &mut guest,
                shell("printf abcdef; printf ghijkl >&2", 3, 3000),
            )
            .await
            .unwrap();
            assert!(matches!(
                receive::<_, Request>(&mut guest).await.unwrap(),
                Request::Hello
            ));
        });
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        loop {
            let event: ExecEvent = receive_limited(&mut host, MAX_EXEC_FRAME).await.unwrap();
            let finished = match event {
                ExecEvent::Output { stream, data } => {
                    let bytes = STANDARD.decode(data).unwrap();
                    match stream {
                        OutputKind::Stdout => stdout.extend(bytes),
                        OutputKind::Stderr => stderr.extend(bytes),
                    }
                    false
                }
                ExecEvent::Finished {
                    code,
                    signal,
                    timed_out,
                    truncated,
                } => {
                    assert_eq!(code, Some(0));
                    assert_eq!(signal, None);
                    assert!(!timed_out);
                    assert!(truncated);
                    true
                }
                ExecEvent::Failed { message } => panic!("unexpected failure: {message}"),
            };
            host.write_u8(1).await.unwrap();
            if finished {
                send(&mut host, &Request::Hello).await.unwrap();
                break;
            }
        }
        assert_eq!(stdout, b"abc");
        assert_eq!(stderr, b"ghi");
        runner.await.unwrap();
    }

    #[tokio::test]
    async fn timeout_closes_stream_while_output_ack_is_pending() {
        let (mut host, mut guest) = UnixStream::pair().unwrap();
        let runner = tokio::spawn(async move {
            exec(
                &mut guest,
                shell("while :; do printf output; done", MAX_DATA, 100),
            )
            .await
            .unwrap();
        });
        assert!(matches!(
            receive_limited::<_, ExecEvent>(&mut host, MAX_EXEC_FRAME)
                .await
                .unwrap(),
            ExecEvent::Output { .. }
        ));
        tokio::time::timeout(Duration::from_secs(2), runner)
            .await
            .unwrap()
            .unwrap();
        let mut byte = [0];
        assert_eq!(host.read(&mut byte).await.unwrap(), 0);
    }
}
