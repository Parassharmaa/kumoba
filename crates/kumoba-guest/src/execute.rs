use base64::{Engine, engine::general_purpose::STANDARD};
use kumoba_protocol::{
    ExecEvent, ExecRequest, MAX_DATA, MAX_OUTPUT_CHUNK, MAX_OUTPUT_LIMIT, OutputKind, send,
};
use std::{
    io,
    os::{
        fd::{AsRawFd, RawFd},
        unix::process::ExitStatusExt,
    },
    process::Stdio,
    time::Duration,
};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWriteExt},
    net::UnixStream,
    sync::{mpsc, watch},
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

struct Forwarder {
    stream: OutputKind,
    limit: usize,
    forwarded: usize,
    truncated: bool,
    sender: mpsc::Sender<OutputChunk>,
}

impl Forwarder {
    async fn take(&mut self, bytes: &[u8]) -> io::Result<()> {
        let keep = bytes.len().min(self.limit.saturating_sub(self.forwarded));
        self.truncated |= keep != bytes.len();
        if keep != 0 {
            self.sender
                .send(OutputChunk {
                    stream: self.stream,
                    bytes: bytes[..keep].to_vec(),
                })
                .await
                .map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "output writer ended"))?;
            self.forwarded += keep;
        }
        Ok(())
    }
}

fn buffered(fd: RawFd) -> io::Result<usize> {
    let mut count: libc::c_int = 0;
    if unsafe { libc::ioctl(fd, libc::FIONREAD, &mut count) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(usize::try_from(count).unwrap_or(0))
}

async fn pump(
    mut reader: impl AsyncRead + AsRawFd + Unpin,
    stream: OutputKind,
    limit: usize,
    sender: mpsc::Sender<OutputChunk>,
    mut detach: watch::Receiver<bool>,
) -> io::Result<bool> {
    let mut buffer = [0; MAX_OUTPUT_CHUNK];
    let mut output = Forwarder {
        stream,
        limit,
        forwarded: 0,
        truncated: false,
        sender,
    };
    loop {
        let count = tokio::select! {
            biased;
            _ = detach.wait_for(|detached| *detached) => break,
            count = reader.read(&mut buffer) => count?,
        };
        if count == 0 {
            return Ok(output.truncated);
        }
        output.take(&buffer[..count]).await?;
    }
    let mut pending = buffered(reader.as_raw_fd())?;
    while pending != 0 {
        let wanted = pending.min(buffer.len());
        let count = reader.read(&mut buffer[..wanted]).await?;
        if count == 0 {
            break;
        }
        pending -= count;
        output.take(&buffer[..count]).await?;
    }
    Ok(output.truncated)
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
                if libc::initgroups(name.as_ptr(), crate::agent::initgroups_gid(account.gid)) != 0
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
    let pid = match crate::agent::pid_of(&child) {
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
            let (detach, detached) = watch::channel(false);
            let out = tokio::spawn(pump(
                stdout,
                OutputKind::Stdout,
                limit,
                sender,
                detached.clone(),
            ));
            let err = tokio::spawn(pump(
                stderr,
                OutputKind::Stderr,
                limit,
                stderr_sender,
                detached,
            ));
            let feed = tokio::spawn(async move {
                let _ = input.write_all(&input_bytes).await;
            });
            let status = child.wait().await?;
            feed.abort();
            let detach_later = tokio::spawn(async move {
                tokio::time::sleep(DETACH_DRAIN).await;
                let _ = detach.send(true);
            });
            let out_truncated = out.await.map_err(io::Error::other)??;
            let err_truncated = err.await.map_err(io::Error::other)??;
            detach_later.abort();
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

    struct Collected {
        stdout: Vec<u8>,
        stderr: Vec<u8>,
        code: Option<i32>,
        timed_out: bool,
        truncated: bool,
    }

    async fn collect(host: &mut UnixStream, first_ack_delay: Duration) -> Collected {
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let mut delay = Some(first_ack_delay);
        loop {
            let event: ExecEvent = receive_limited(host, MAX_EXEC_FRAME).await.unwrap();
            if let Some(delay) = delay.take() {
                tokio::time::sleep(delay).await;
            }
            host.write_u8(1).await.unwrap();
            match event {
                ExecEvent::Output { stream, data } => {
                    let bytes = STANDARD.decode(data).unwrap();
                    match stream {
                        OutputKind::Stdout => stdout.extend(bytes),
                        OutputKind::Stderr => stderr.extend(bytes),
                    }
                }
                ExecEvent::Finished {
                    code,
                    signal,
                    timed_out,
                    truncated,
                } => {
                    assert_eq!(signal, None);
                    return Collected {
                        stdout,
                        stderr,
                        code,
                        timed_out,
                        truncated,
                    };
                }
                ExecEvent::Failed { message } => panic!("unexpected failure: {message}"),
            }
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
        let result = collect(&mut host, Duration::ZERO).await;
        send(&mut host, &Request::Hello).await.unwrap();
        assert_eq!(result.code, Some(0));
        assert!(!result.timed_out);
        assert!(result.truncated);
        assert_eq!(result.stdout, b"abc");
        assert_eq!(result.stderr, b"ghi");
        runner.await.unwrap();
    }

    #[tokio::test]
    async fn complete_output_is_not_reported_truncated() {
        let (mut host, mut guest) = UnixStream::pair().unwrap();
        let runner = tokio::spawn(async move {
            exec(&mut guest, shell("printf abc; printf def >&2", 3, 3000))
                .await
                .unwrap();
        });
        let result = collect(&mut host, Duration::ZERO).await;
        assert_eq!(result.code, Some(0));
        assert!(!result.truncated);
        assert_eq!(result.stdout, b"abc");
        assert_eq!(result.stderr, b"def");
        runner.await.unwrap();
    }

    async fn run_bounded(
        script: &'static str,
        limit: usize,
        first_ack_delay: Duration,
    ) -> Collected {
        let (mut host, mut guest) = UnixStream::pair().unwrap();
        let runner = tokio::spawn(async move {
            exec(&mut guest, shell(script, limit, 10_000))
                .await
                .unwrap();
        });
        let result =
            tokio::time::timeout(Duration::from_secs(8), collect(&mut host, first_ack_delay))
                .await
                .expect("execution did not finish");
        runner.await.unwrap();
        assert_eq!(result.code, Some(0), "{script}");
        assert!(!result.timed_out, "{script}");
        result
    }

    #[tokio::test]
    async fn output_cut_while_a_background_process_holds_the_pipe_is_reported_truncated() {
        for script in ["seq 1 200000; sleep 2 &", "seq 1 200000 >&2; sleep 2 &"] {
            let result = run_bounded(script, 1024, Duration::ZERO).await;
            assert!(result.truncated, "{script}");
            assert!(result.stdout.len() <= 1024 && result.stderr.len() <= 1024);
        }
    }

    #[tokio::test]
    async fn complete_output_with_a_background_process_holding_the_pipe_is_not_truncated() {
        let result = run_bounded("printf abc; sleep 2 &", 1024, Duration::ZERO).await;
        assert!(!result.truncated);
        assert_eq!(result.stdout, b"abc");
        assert!(result.stderr.is_empty());
    }

    #[tokio::test]
    async fn output_waiting_in_the_pipe_when_the_command_ends_is_returned() {
        let result = run_bounded(
            "for i in $(seq 1 40); do printf '%s,' $i; sleep 0.01; done; printf end; sleep 3 &",
            MAX_DATA,
            Duration::from_millis(1500),
        )
        .await;
        let expected = (1..=40).map(|i| format!("{i},")).collect::<String>() + "end";
        assert!(!result.truncated);
        assert_eq!(String::from_utf8(result.stdout).unwrap(), expected);
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
