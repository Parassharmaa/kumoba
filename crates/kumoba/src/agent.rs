use crate::{Command, Error, ExecSummary, Result, Termination};
use base64::{Engine, engine::general_purpose::STANDARD};
use kumoba_protocol::{
    Entry, MAX_DATA, Request, Response, ShellInput, ShellOutput, ShellRequest, UploadInput,
    receive, send,
};
use serde::Serialize;
use std::{path::PathBuf, process::ExitStatus, time::Duration};
use tokio::{
    io::{AsyncWrite, AsyncWriteExt},
    net::UnixStream,
    sync::watch,
};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const REPLY_TIMEOUT: Duration = Duration::from_secs(60);
const COMMIT_TIMEOUT: Duration = Duration::from_secs(600);
const EXEC_GRACE: Duration = Duration::from_secs(5);

type Exit = watch::Receiver<Option<std::result::Result<ExitStatus, String>>>;

#[derive(Clone)]
pub struct Agent {
    socket: PathBuf,
    exit: Exit,
    staged_uploads: bool,
}

pub struct ShellSender(tokio::net::unix::OwnedWriteHalf);

pub struct ShellReceiver(tokio::net::unix::OwnedReadHalf);

impl ShellSender {
    pub async fn send(&mut self, input: &ShellInput) -> Result<()> {
        send(&mut self.0, input).await.map_err(Error::Io)
    }
}

impl ShellReceiver {
    pub async fn next(&mut self) -> Result<Option<ShellOutput>> {
        match receive::<_, ShellOutput>(&mut self.0).await {
            Ok(output) => Ok(Some(output)),
            Err(error) if error.kind() == std::io::ErrorKind::UnexpectedEof => Ok(None),
            Err(error) => Err(Error::Io(error)),
        }
    }
}

pub struct Upload(Option<Mode>);

enum Mode {
    Staged {
        stream: UnixStream,
        exit: Exit,
    },
    InPlace {
        agent: Agent,
        path: String,
        user: Option<String>,
        offset: u64,
        started: bool,
    },
}

impl Mode {
    async fn write(&mut self, data: &[u8]) -> Result<()> {
        if data.len() > MAX_DATA {
            return Err(Error::InvalidInput("file chunk exceeds 1 MiB".into()));
        }
        match self {
            Self::Staged { stream, exit } => {
                let input = UploadInput::Data(STANDARD.encode(data));
                expect_ok(
                    exchange(stream, exit.clone(), &input, REPLY_TIMEOUT).await?,
                    "write",
                )
            }
            Self::InPlace {
                agent,
                path,
                user,
                offset,
                started,
            } => {
                agent
                    .write_at(path, *offset, data, !*started, user.as_deref())
                    .await?;
                *started = true;
                *offset += data.len() as u64;
                Ok(())
            }
        }
    }

    async fn commit(mut self) -> Result<()> {
        match &mut self {
            Self::Staged { stream, exit } => expect_ok(
                exchange(stream, exit.clone(), &UploadInput::Commit, COMMIT_TIMEOUT).await?,
                "commit",
            ),
            Self::InPlace { started: true, .. } => Ok(()),
            Self::InPlace { .. } => self.write(&[]).await,
        }
    }
}

impl Upload {
    pub async fn write(&mut self, data: &[u8]) -> Result<()> {
        let mut mode = self.0.take().ok_or_else(failed_upload)?;
        mode.write(data).await?;
        self.0 = Some(mode);
        Ok(())
    }

    pub async fn commit(self) -> Result<()> {
        self.0.ok_or_else(failed_upload)?.commit().await
    }
}

fn failed_upload() -> Error {
    Error::InvalidInput("upload cannot continue after an error".into())
}

fn expect_ok(response: Response, operation: &str) -> Result<()> {
    match response {
        Response::Ok => Ok(()),
        Response::Error(message) => Err(Error::Guest(message)),
        _ => Err(Error::Protocol(format!(
            "unexpected upload {operation} response"
        ))),
    }
}

async fn exchange<T: Serialize>(
    stream: &mut UnixStream,
    exit: Exit,
    request: &T,
    deadline: Duration,
) -> Result<Response> {
    tokio::time::timeout(deadline, async {
        tokio::select! {
            response = async {
                send(stream, request).await?;
                let response = receive::<_, Response>(stream).await?;
                stream.write_u8(1).await?;
                Ok::<_, std::io::Error>(response)
            } => response.map_err(|e| Error::OutcomeUnknown(e.to_string())),
            () = Agent::worker_exit(exit) => Err(Error::NotRunning),
        }
    })
    .await
    .map_err(|_| Error::OutcomeUnknown("guest reply deadline exceeded".into()))?
}

pub struct ReadChunk {
    pub data: Vec<u8>,
    pub size: u64,
}

impl Agent {
    pub(crate) fn new(socket: PathBuf, exit: Exit, staged_uploads: bool) -> Self {
        Self {
            socket,
            exit,
            staged_uploads,
        }
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
        match exchange(&mut stream, self.exit.clone(), &request, REPLY_TIMEOUT).await? {
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

    pub async fn shell(
        &self,
        command: Command,
        rows: u16,
        cols: u16,
        login: bool,
    ) -> Result<(ShellSender, ShellReceiver)> {
        let request = Request::Shell(ShellRequest {
            exec: command.into_request(),
            rows,
            cols,
            login,
        });
        let mut stream = self.open().await?;
        let reply = tokio::time::timeout(REPLY_TIMEOUT, async {
            send(&mut stream, &request).await?;
            receive::<_, Response>(&mut stream).await
        })
        .await
        .map_err(|_| Error::OutcomeUnknown("guest shell reply deadline exceeded".into()))?;
        match reply {
            Ok(Response::Ok) => {
                let (reader, writer) = stream.into_split();
                Ok((ShellSender(writer), ShellReceiver(reader)))
            }
            Ok(Response::Error(message)) => Err(Error::Guest(message)),
            Ok(_) => Err(Error::Protocol("unexpected shell response".into())),
            Err(error) => Err(Error::Guest(format!(
                "the guest agent closed the shell: {error}"
            ))),
        }
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

    pub async fn upload(&self, path: &str, user: Option<&str>) -> Result<Upload> {
        if !self.staged_uploads {
            return Ok(Upload(Some(Mode::InPlace {
                agent: self.clone(),
                path: path.into(),
                user: user.map(str::to_string),
                offset: 0,
                started: false,
            })));
        }
        let mut stream = self.open().await?;
        let request = Request::Upload {
            path: path.into(),
            user: user.map(str::to_string),
        };
        expect_ok(
            exchange(&mut stream, self.exit.clone(), &request, REPLY_TIMEOUT).await?,
            "start",
        )?;
        Ok(Upload(Some(Mode::Staged {
            stream,
            exit: self.exit.clone(),
        })))
    }

    pub async fn write_all(&self, path: &str, data: &[u8], user: Option<&str>) -> Result<()> {
        let mut upload = self.upload(path, user).await?;
        for chunk in data.chunks(MAX_DATA) {
            upload.write(chunk).await?;
        }
        upload.commit().await
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
            let done = chunk.data.is_empty()
                || data.len() + chunk.data.len()
                    >= usize::try_from(chunk.size).unwrap_or(usize::MAX);
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;
    use tokio::{io::AsyncReadExt, net::UnixListener, sync::mpsc};

    struct FakeGuest {
        agent: Agent,
        frames: mpsc::UnboundedReceiver<(usize, Option<Value>)>,
        _dir: tempfile::TempDir,
        _exit: watch::Sender<Option<std::result::Result<ExitStatus, String>>>,
    }

    fn fake_guest(staged_uploads: bool) -> FakeGuest {
        fake_guest_answering(staged_uploads, |_| Response::Ok)
    }

    fn fake_guest_answering(staged_uploads: bool, answer: fn(&Value) -> Response) -> FakeGuest {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("agent.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let (sender, frames) = mpsc::unbounded_channel();
        tokio::spawn(async move {
            let mut connections = 0..;
            loop {
                let connection = connections.next().unwrap();
                let (mut stream, _) = listener.accept().await.unwrap();
                let sender = sender.clone();
                tokio::spawn(async move {
                    while let Ok(frame) = receive::<_, Value>(&mut stream).await {
                        let response = answer(&frame);
                        sender.send((connection, Some(frame))).unwrap();
                        if send(&mut stream, &response).await.is_err()
                            || stream.read_u8().await.is_err()
                        {
                            break;
                        }
                    }
                    sender.send((connection, None)).unwrap();
                });
            }
        });
        let (exit, receiver) = watch::channel(None);
        FakeGuest {
            agent: Agent::new(socket, receiver, staged_uploads),
            frames,
            _dir: dir,
            _exit: exit,
        }
    }

    async fn next(guest: &mut FakeGuest) -> (usize, Option<Value>) {
        tokio::time::timeout(Duration::from_secs(5), guest.frames.recv())
            .await
            .unwrap()
            .unwrap()
    }

    fn write_at(frame: &Value) -> (u64, bool) {
        let write = &frame["WriteAt"];
        (
            write["offset"].as_u64().unwrap(),
            write["truncate"].as_bool().unwrap(),
        )
    }

    #[tokio::test]
    async fn older_guest_gets_in_place_writes_truncating_only_the_first() {
        let mut guest = fake_guest(false);
        let data = vec![7u8; MAX_DATA * 2 + 3];
        guest
            .agent
            .write_all("/workspace/file", &data, None)
            .await
            .unwrap();
        let mut writes = Vec::new();
        for _ in 0..6 {
            if let (_, Some(frame)) = next(&mut guest).await {
                writes.push(write_at(&frame));
            }
        }
        assert_eq!(
            writes,
            [
                (0, true),
                (MAX_DATA as u64, false),
                (2 * MAX_DATA as u64, false)
            ]
        );
        guest
            .agent
            .write_all("/workspace/empty", &[], None)
            .await
            .unwrap();
        let (_, Some(frame)) = next(&mut guest).await else {
            panic!("expected a write");
        };
        assert_eq!(write_at(&frame), (0, true));
        assert_eq!(frame["WriteAt"]["data"], "");
    }

    #[tokio::test]
    async fn current_guest_gets_one_connection_that_ends_in_commit() {
        let mut guest = fake_guest(true);
        let data = vec![7u8; MAX_DATA + 3];
        guest
            .agent
            .write_all("/workspace/file", &data, Some("kumoba"))
            .await
            .unwrap();
        let mut frames = Vec::new();
        loop {
            let (connection, frame) = next(&mut guest).await;
            assert_eq!(connection, 0);
            let Some(frame) = frame else { break };
            frames.push(frame);
        }
        assert_eq!(
            frames[0],
            serde_json::json!({"Upload": {"path": "/workspace/file", "user": "kumoba"}})
        );
        assert_eq!(
            frames[1]["Data"].as_str().unwrap(),
            STANDARD.encode(&data[..MAX_DATA])
        );
        assert_eq!(
            frames[2]["Data"].as_str().unwrap(),
            STANDARD.encode([7u8; 3])
        );
        assert_eq!(frames[3], serde_json::json!("Commit"));
        assert_eq!(frames.len(), 4);
    }

    #[tokio::test]
    async fn dropped_upload_closes_the_connection_without_commit() {
        let mut guest = fake_guest(true);
        let mut upload = guest.agent.upload("/workspace/file", None).await.unwrap();
        upload.write(b"partial").await.unwrap();
        drop(upload);
        let mut frames = Vec::new();
        while let (_, Some(frame)) = next(&mut guest).await {
            frames.push(frame);
        }
        assert_eq!(frames.len(), 2);
        assert!(frames.iter().all(|frame| frame != "Commit"));
    }

    fn refuse_first_chunk(frame: &Value) -> Response {
        let first = |data: &Value| data.as_str() == Some(STANDARD.encode(b"first").as_str());
        if first(&frame["Data"]) || first(&frame["WriteAt"]["data"]) {
            Response::Error("disk full".into())
        } else {
            Response::Ok
        }
    }

    #[tokio::test]
    async fn upload_refuses_further_use_after_an_error() {
        for staged in [true, false] {
            let mut guest = fake_guest_answering(staged, refuse_first_chunk);
            let mut upload = guest.agent.upload("/workspace/file", None).await.unwrap();
            assert!(matches!(
                upload.write(b"first").await,
                Err(Error::Guest(message)) if message == "disk full"
            ));
            assert!(upload.write(b"second").await.is_err());
            assert!(upload.commit().await.is_err());
            let mut frames = Vec::new();
            while let Ok(Some((_, frame))) =
                tokio::time::timeout(Duration::from_millis(300), guest.frames.recv()).await
            {
                frames.extend(frame);
            }
            assert_eq!(frames.len(), if staged { 2 } else { 1 }, "{frames:?}");
        }
    }

    #[tokio::test]
    async fn oversized_chunk_is_refused_before_sending() {
        let guest = fake_guest(true);
        let mut upload = guest.agent.upload("/workspace/file", None).await.unwrap();
        assert!(matches!(
            upload.write(&vec![0u8; MAX_DATA + 1]).await,
            Err(Error::InvalidInput(_))
        ));
    }
}
