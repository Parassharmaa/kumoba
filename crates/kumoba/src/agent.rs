use crate::{Command, Error, ExecSummary, Result, Termination};
use base64::{Engine, engine::general_purpose::STANDARD};
use kumoba_protocol::{Entry, MAX_DATA, Request, Response, receive, send};
use std::{path::PathBuf, process::ExitStatus, time::Duration};
use tokio::{
    io::{AsyncWrite, AsyncWriteExt},
    net::UnixStream,
    sync::watch,
};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const REPLY_TIMEOUT: Duration = Duration::from_secs(60);
const EXEC_GRACE: Duration = Duration::from_secs(5);

type Exit = watch::Receiver<Option<std::result::Result<ExitStatus, String>>>;

#[derive(Clone)]
pub struct Agent {
    socket: PathBuf,
    exit: Exit,
}

pub struct ReadChunk {
    pub data: Vec<u8>,
    pub size: u64,
}

impl Agent {
    pub(crate) fn new(socket: PathBuf, exit: Exit) -> Self {
        Self { socket, exit }
    }

    fn exited(&self) -> bool {
        self.exit.borrow().is_some()
    }

    async fn open(&self) -> Result<UnixStream> {
        if self.exited() {
            return Err(Error::NotRunning);
        }
        tokio::time::timeout(CONNECT_TIMEOUT, UnixStream::connect(&self.socket))
            .await
            .map_err(|_| Error::Guest("guest agent connection timed out".into()))?
            .map_err(Error::Io)
    }

    async fn worker_exit(mut exit: Exit) {
        loop {
            if exit.borrow().is_some() {
                return;
            }
            if exit.changed().await.is_err() {
                return;
            }
        }
    }

    async fn call(&self, request: Request) -> Result<Response> {
        let mut stream = self.open().await?;
        let exit = self.exit.clone();
        let response = tokio::time::timeout(REPLY_TIMEOUT, async {
            tokio::select! {
                response = async {
                    send(&mut stream, &request).await?;
                    let response = receive::<_, Response>(&mut stream).await?;
                    stream.write_u8(1).await?;
                    Ok::<_, std::io::Error>(response)
                } => response.map_err(|e| Error::OutcomeUnknown(e.to_string())),
                () = Self::worker_exit(exit) => Err(Error::NotRunning),
            }
        })
        .await
        .map_err(|_| Error::OutcomeUnknown("guest reply deadline exceeded".into()))??;
        match response {
            Response::Error(message) => Err(Error::Guest(message)),
            response => Ok(response),
        }
    }

    pub async fn exec_streamed<O, E>(
        &self,
        command: Command,
        stdout: &mut O,
        stderr: &mut E,
    ) -> Result<ExecSummary>
    where
        O: AsyncWrite + Unpin + Send,
        E: AsyncWrite + Unpin + Send,
    {
        let deadline = command.timeout_duration() + EXEC_GRACE;
        let output_limit = command.output_limit;
        let request = Request::Exec(command.into_request());
        let mut stream = self.open().await?;
        let mut progress = crate::execution::Progress::default();
        let exit = self.exit.clone();
        let outcome = tokio::time::timeout(deadline, async {
            tokio::select! {
                result = crate::execution::drive(&mut stream, request, output_limit, stdout, stderr, &mut progress) => result,
                () = Self::worker_exit(exit) => Err(Error::OutcomeUnknown("worker exited during execution".into())),
            }
        })
        .await;
        let (termination, truncated) = match outcome {
            Ok(result) => result?,
            Err(_) => (Termination::TimedOut, true),
        };
        Ok(ExecSummary {
            termination,
            stdout_bytes: progress.stdout,
            stderr_bytes: progress.stderr,
            truncated,
        })
    }

    pub async fn read_at(
        &self,
        path: &str,
        offset: u64,
        len: usize,
        user: Option<&str>,
    ) -> Result<ReadChunk> {
        match self
            .call(Request::ReadAt {
                path: path.into(),
                offset,
                len: len.min(MAX_DATA),
                user: user.map(str::to_string),
            })
            .await?
        {
            Response::Chunk { data, size } => Ok(ReadChunk {
                data: STANDARD
                    .decode(data)
                    .map_err(|e| Error::Protocol(e.to_string()))?,
                size,
            }),
            _ => Err(Error::Protocol("unexpected read response".into())),
        }
    }

    pub async fn write_at(
        &self,
        path: &str,
        offset: u64,
        data: &[u8],
        truncate: bool,
        user: Option<&str>,
    ) -> Result<()> {
        if data.len() > MAX_DATA {
            return Err(Error::InvalidInput("file chunk exceeds 1 MiB".into()));
        }
        match self
            .call(Request::WriteAt {
                path: path.into(),
                offset,
                data: STANDARD.encode(data),
                truncate,
                user: user.map(str::to_string),
            })
            .await?
        {
            Response::Ok => Ok(()),
            _ => Err(Error::Protocol("unexpected write response".into())),
        }
    }

    pub async fn write_all(&self, path: &str, data: &[u8], user: Option<&str>) -> Result<()> {
        if data.is_empty() {
            return self.write_at(path, 0, &[], true, user).await;
        }
        let mut offset = 0;
        for chunk in data.chunks(MAX_DATA) {
            self.write_at(path, offset as u64, chunk, offset == 0, user)
                .await?;
            offset += chunk.len();
        }
        Ok(())
    }

    pub async fn read_all(
        &self,
        path: &str,
        max_bytes: u64,
        user: Option<&str>,
    ) -> Result<Vec<u8>> {
        let mut data = Vec::new();
        loop {
            let chunk = self
                .read_at(path, data.len() as u64, MAX_DATA, user)
                .await?;
            if chunk.size > max_bytes {
                return Err(Error::InvalidInput("file exceeds transfer limit".into()));
            }
            let done =
                chunk.data.is_empty() || data.len() + chunk.data.len() >= chunk.size as usize;
            data.extend(chunk.data);
            if done {
                return Ok(data);
            }
        }
    }

    pub async fn connect(&self, port: u16) -> Result<UnixStream> {
        if port == 0 {
            return Err(Error::InvalidInput("invalid guest port".into()));
        }
        let mut stream = self.open().await?;
        tokio::time::timeout(CONNECT_TIMEOUT, async {
            send(&mut stream, &Request::Connect { port }).await?;
            let response = receive::<_, Response>(&mut stream).await?;
            stream.write_u8(1).await?;
            Ok::<_, std::io::Error>(response)
        })
        .await
        .map_err(|_| Error::Guest("guest port connection timed out".into()))?
        .map_err(|e| Error::OutcomeUnknown(e.to_string()))
        .and_then(|response| match response {
            Response::Connected => Ok(stream),
            Response::Error(message) => Err(Error::Guest(message)),
            _ => Err(Error::Protocol("unexpected tunnel response".into())),
        })
    }

    pub async fn list(&self, path: &str, user: Option<&str>) -> Result<Vec<Entry>> {
        match self
            .call(Request::List {
                path: path.into(),
                user: user.map(str::to_string),
            })
            .await?
        {
            Response::Entries(entries) => Ok(entries),
            _ => Err(Error::Protocol("unexpected list response".into())),
        }
    }
}
