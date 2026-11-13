use std::io;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

pub const PROTOCOL: &str = "kumoba-shell.v1";
const MAX_FRAME: usize = 1024 * 1024;

const DATA: u8 = 1;
const RESIZE: u8 = 2;
const EOF: u8 = 3;
const EXIT: u8 = 4;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Frame {
    Data(Vec<u8>),
    Resize { rows: u16, cols: u16 },
    Eof,
    Exit(i32),
}

pub async fn read_frame<R: AsyncRead + Unpin>(reader: &mut R) -> io::Result<Option<Frame>> {
    let kind = match reader.read_u8().await {
        Ok(kind) => kind,
        Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(error) => return Err(error),
    };
    let length = usize::try_from(reader.read_u32().await?).map_err(io::Error::other)?;
    if length > MAX_FRAME {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "terminal frame too large",
        ));
    }
    let mut payload = vec![0; length];
    reader.read_exact(&mut payload).await?;
    let invalid = || io::Error::new(io::ErrorKind::InvalidData, "invalid terminal frame");
    Ok(Some(match kind {
        DATA => Frame::Data(payload),
        RESIZE => {
            let bytes: [u8; 4] = payload.try_into().map_err(|_| invalid())?;
            Frame::Resize {
                rows: u16::from_be_bytes([bytes[0], bytes[1]]),
                cols: u16::from_be_bytes([bytes[2], bytes[3]]),
            }
        }
        EOF => Frame::Eof,
        EXIT => {
            let bytes: [u8; 4] = payload.try_into().map_err(|_| invalid())?;
            Frame::Exit(i32::from_be_bytes(bytes))
        }
        _ => return Err(invalid()),
    }))
}

pub async fn write_frame<W: AsyncWrite + Unpin>(writer: &mut W, frame: &Frame) -> io::Result<()> {
    let (kind, payload) = match frame {
        Frame::Data(data) => (DATA, data.clone()),
        Frame::Resize { rows, cols } => {
            let mut bytes = rows.to_be_bytes().to_vec();
            bytes.extend(cols.to_be_bytes());
            (RESIZE, bytes)
        }
        Frame::Eof => (EOF, Vec::new()),
        Frame::Exit(code) => (EXIT, code.to_be_bytes().to_vec()),
    };
    let length = u32::try_from(payload.len()).map_err(io::Error::other)?;
    let mut buffer = Vec::with_capacity(payload.len() + 5);
    buffer.push(kind);
    buffer.extend(length.to_be_bytes());
    buffer.extend(payload);
    writer.write_all(&buffer).await?;
    writer.flush().await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn frames_round_trip() {
        let frames = [
            Frame::Data(b"ls -la\r".to_vec()),
            Frame::Resize {
                rows: 40,
                cols: 120,
            },
            Frame::Eof,
            Frame::Exit(-1),
            Frame::Exit(130),
        ];
        let mut wire = Vec::new();
        for frame in &frames {
            write_frame(&mut wire, frame).await.unwrap();
        }
        let mut reader = wire.as_slice();
        for frame in &frames {
            assert_eq!(read_frame(&mut reader).await.unwrap().as_ref(), Some(frame));
        }
        assert_eq!(read_frame(&mut reader).await.unwrap(), None);
    }

    #[tokio::test]
    async fn rejects_oversized_and_unknown_frames() {
        let mut oversized = vec![DATA];
        oversized.extend(u32::MAX.to_be_bytes());
        assert!(read_frame(&mut oversized.as_slice()).await.is_err());
        let unknown = [9u8, 0, 0, 0, 0];
        assert!(read_frame(&mut unknown.as_slice()).await.is_err());
    }
}
