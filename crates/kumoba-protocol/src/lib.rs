pub mod desktop;
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use std::{collections::BTreeMap, io, path::PathBuf};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

pub const VERSION: u32 = 3;
pub const AGENT_PORT: u32 = 1024;
pub const EGRESS_PORT: u32 = 1025;
pub const MAX_FRAME: usize = 8 * 1024 * 1024;
pub const MAX_DATA: usize = 1024 * 1024;
pub const MAX_OUTPUT_CHUNK: usize = 16 * 1024;
pub const MAX_EXEC_FRAME: usize = 64 * 1024;
pub const MAX_OUTPUT_LIMIT: usize = 256 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HostMount {
    pub source: PathBuf,
    pub target: String,
    pub read_only: bool,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct WorkerConfig {
    pub version: u32,
    pub library: PathBuf,
    pub firmware: PathBuf,
    pub disk: PathBuf,
    pub socket: PathBuf,
    #[serde(default)]
    pub egress_socket: Option<PathBuf>,
    pub console: PathBuf,
    pub vcpus: u8,
    pub memory_mib: u32,
    pub lock_fd: i32,
    #[serde(default)]
    pub mounts: Vec<HostMount>,
}

#[derive(Debug, Serialize, Deserialize)]
pub enum Request {
    Hello,
    Exec(ExecRequest),
    Read {
        path: String,
        max_bytes: usize,
    },
    Write {
        path: String,
        data: String,
    },
    Connect {
        port: u16,
    },
    Shutdown,
    Desktop(desktop::DesktopRequest),
    ReadAt {
        path: String,
        offset: u64,
        len: usize,
        #[serde(default)]
        user: Option<String>,
    },
    WriteAt {
        path: String,
        offset: u64,
        data: String,
        truncate: bool,
        #[serde(default)]
        user: Option<String>,
    },
    List {
        path: String,
        #[serde(default)]
        user: Option<String>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EntryKind {
    File,
    Directory,
    Other,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    pub name: String,
    pub kind: EntryKind,
    pub size: u64,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ExecRequest {
    pub program: String,
    pub args: Vec<String>,
    pub cwd: String,
    pub env: BTreeMap<String, String>,
    pub stdin: String,
    pub timeout_ms: u64,
    pub output_limit: usize,
    #[serde(default)]
    pub user: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub enum Response {
    Hello {
        version: u32,
        arch: String,
        #[serde(default)]
        desktop: bool,
        #[serde(default)]
        desktop_startup_timeout: bool,
    },
    Desktop {
        width: u16,
        height: u16,
        cdp_port: u16,
        #[serde(default)]
        vnc_port: Option<u16>,
    },
    Screenshot {
        width: u16,
        height: u16,
        png: String,
    },
    Data(String),
    Chunk {
        data: String,
        size: u64,
    },
    Entries(Vec<Entry>),
    Ok,
    Connected,
    Error(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum OutputKind {
    Stdout,
    Stderr,
}

#[derive(Debug, Serialize, Deserialize)]
pub enum ExecEvent {
    Output {
        stream: OutputKind,
        data: String,
    },
    Finished {
        code: Option<i32>,
        signal: Option<i32>,
        timed_out: bool,
        truncated: bool,
    },
    Failed {
        message: String,
    },
}

pub async fn send<W: AsyncWrite + Unpin, T: Serialize>(
    writer: &mut W,
    value: &T,
) -> io::Result<()> {
    let bytes = serde_json::to_vec(value).map_err(io::Error::other)?;
    let length = u32::try_from(bytes.len())
        .ok()
        .filter(|_| bytes.len() <= MAX_FRAME)
        .ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidInput, "protocol frame exceeds limit")
        })?;
    writer.write_u32(length).await?;
    writer.write_all(&bytes).await?;
    writer.flush().await
}

pub async fn receive<R: AsyncRead + Unpin, T: DeserializeOwned>(reader: &mut R) -> io::Result<T> {
    receive_limited(reader, MAX_FRAME).await
}

pub async fn receive_limited<R: AsyncRead + Unpin, T: DeserializeOwned>(
    reader: &mut R,
    limit: usize,
) -> io::Result<T> {
    let size = reader.read_u32().await? as usize;
    if size > limit.min(MAX_FRAME) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "protocol frame exceeds limit",
        ));
    }
    let mut bytes = vec![0; size];
    reader.read_exact(&mut bytes).await?;
    serde_json::from_slice(&bytes).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn reject_oversized_length_without_allocating_body() {
        let (mut a, mut b) = tokio::io::duplex(16);
        a.write_u32(u32::try_from(MAX_FRAME + 1).unwrap())
            .await
            .unwrap();
        assert_eq!(
            receive::<_, Request>(&mut b).await.unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
    }

    #[tokio::test]
    async fn streaming_limit_rejects_length_before_waiting_for_body() {
        let (mut writer, mut reader) = tokio::io::duplex(4);
        writer
            .write_u32(u32::try_from(MAX_EXEC_FRAME + 1).unwrap())
            .await
            .unwrap();
        assert_eq!(
            receive_limited::<_, ExecEvent>(&mut reader, MAX_EXEC_FRAME)
                .await
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidData
        );
    }

    #[tokio::test]
    async fn streaming_frames_leave_following_request_intact() {
        let (mut writer, mut reader) = tokio::io::duplex(MAX_EXEC_FRAME);
        let sender = tokio::spawn(async move {
            send(
                &mut writer,
                &ExecEvent::Output {
                    stream: OutputKind::Stderr,
                    data: "AAEC/w==".into(),
                },
            )
            .await
            .unwrap();
            send(
                &mut writer,
                &ExecEvent::Finished {
                    code: Some(0),
                    signal: None,
                    timed_out: false,
                    truncated: false,
                },
            )
            .await
            .unwrap();
            send(&mut writer, &Request::Hello).await.unwrap();
        });
        assert!(matches!(
            receive_limited::<_, ExecEvent>(&mut reader, MAX_EXEC_FRAME)
                .await
                .unwrap(),
            ExecEvent::Output { stream: OutputKind::Stderr, data } if data == "AAEC/w=="
        ));
        assert!(matches!(
            receive_limited::<_, ExecEvent>(&mut reader, MAX_EXEC_FRAME)
                .await
                .unwrap(),
            ExecEvent::Finished { code: Some(0), .. }
        ));
        assert!(matches!(
            receive::<_, Request>(&mut reader).await.unwrap(),
            Request::Hello
        ));
        sender.await.unwrap();
    }
}
