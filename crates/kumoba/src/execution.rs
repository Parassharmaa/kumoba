use crate::{Error, Result, Termination};
use base64::{Engine, engine::general_purpose::STANDARD};
use kumoba_protocol::{
    ExecEvent, MAX_EXEC_FRAME, MAX_OUTPUT_CHUNK, OutputKind, Request, receive_limited, send,
};
use tokio::{
    io::{AsyncWrite, AsyncWriteExt},
    net::UnixStream,
};

#[derive(Default)]
pub(crate) struct Progress {
    pub stdout: u64,
    pub stderr: u64,
}

pub(crate) async fn drive<O: AsyncWrite + Unpin, E: AsyncWrite + Unpin>(
    stream: &mut UnixStream,
    request: Request,
    limit: usize,
    stdout: &mut O,
    stderr: &mut E,
    progress: &mut Progress,
) -> Result<(Termination, bool)> {
    send(stream, &request).await.map_err(unknown)?;
    loop {
        let event: ExecEvent = receive_limited(stream, MAX_EXEC_FRAME)
            .await
            .map_err(unknown)?;
        match event {
            ExecEvent::Output { stream: kind, data } => {
                if data.len() > MAX_OUTPUT_CHUNK.div_ceil(3) * 4 {
                    return Err(Error::Protocol("oversized output chunk".into()));
                }
                let data = STANDARD
                    .decode(data)
                    .map_err(|e| Error::Protocol(e.to_string()))?;
                if data.is_empty() || data.len() > MAX_OUTPUT_CHUNK {
                    return Err(Error::Protocol("invalid output chunk size".into()));
                }
                match kind {
                    OutputKind::Stdout => {
                        write_counted(stdout, &data, &mut progress.stdout, limit).await?;
                    }
                    OutputKind::Stderr => {
                        write_counted(stderr, &data, &mut progress.stderr, limit).await?;
                    }
                }
                stream.write_u8(1).await.map_err(unknown)?;
            }
            ExecEvent::Finished {
                code,
                signal,
                timed_out,
                truncated,
            } => {
                let termination = match (code, signal, timed_out) {
                    (Some(code @ 0..=255), None, false) => Termination::Exited(code),
                    (None, Some(signal @ 1..=64), false) => Termination::Signaled(signal),
                    (None, None, true) => Termination::TimedOut,
                    _ => return Err(Error::Protocol("invalid command termination".into())),
                };
                stdout.flush().await?;
                stderr.flush().await?;
                stream.write_u8(1).await.map_err(unknown)?;
                return Ok((termination, truncated));
            }
            ExecEvent::Failed { message } => {
                stream.write_u8(1).await.map_err(unknown)?;
                return Err(Error::Guest(message));
            }
        }
    }
}

async fn write_counted<W: AsyncWrite + Unpin>(
    writer: &mut W,
    data: &[u8],
    count: &mut u64,
    limit: usize,
) -> Result<()> {
    if data.len() as u64 > (limit as u64).saturating_sub(*count) {
        return Err(Error::Protocol("guest exceeded output limit".into()));
    }
    let mut remaining = data;
    while !remaining.is_empty() {
        let written = writer.write(remaining).await?;
        if written == 0 {
            return Err(std::io::Error::from(std::io::ErrorKind::WriteZero).into());
        }
        *count += written as u64;
        remaining = &remaining[written..];
    }
    Ok(())
}
fn unknown(error: std::io::Error) -> Error {
    Error::OutcomeUnknown(error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn hostile_output_cannot_exceed_the_requested_limit() {
        let (mut host, mut guest) = UnixStream::pair().unwrap();
        let peer = tokio::spawn(async move {
            let _: Request = kumoba_protocol::receive(&mut guest).await.unwrap();
            send(
                &mut guest,
                &ExecEvent::Output {
                    stream: OutputKind::Stdout,
                    data: STANDARD.encode([1u8; 32]),
                },
            )
            .await
            .unwrap();
        });
        let result = drive(
            &mut host,
            Request::Hello,
            8,
            &mut Vec::new(),
            &mut Vec::new(),
            &mut Progress::default(),
        )
        .await;
        assert!(matches!(result, Err(Error::Protocol(_))));
        peer.await.unwrap();
    }
    #[tokio::test]
    async fn inconsistent_exit_metadata_is_rejected() {
        let (mut host, mut guest) = UnixStream::pair().unwrap();
        let peer = tokio::spawn(async move {
            let _: Request = kumoba_protocol::receive(&mut guest).await.unwrap();
            send(
                &mut guest,
                &ExecEvent::Finished {
                    code: Some(0),
                    signal: Some(9),
                    timed_out: false,
                    truncated: false,
                },
            )
            .await
            .unwrap();
        });
        let result = drive(
            &mut host,
            Request::Hello,
            8,
            &mut Vec::new(),
            &mut Vec::new(),
            &mut Progress::default(),
        )
        .await;
        assert!(matches!(result, Err(Error::Protocol(_))));
        peer.await.unwrap();
    }
}
