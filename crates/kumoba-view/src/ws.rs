use base64::{Engine, engine::general_purpose::STANDARD};
use sha1::{Digest, Sha1};
use std::collections::VecDeque;
use std::io::{self, Read, Write};
use std::net::TcpStream;

const GUID: &str = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11";
pub const MAX_INCOMING: usize = 256 * 1024;
const MAX_BUFFERED: usize = 1024 * 1024;

pub fn accept_key(key: &str) -> String {
    let mut hash = Sha1::new();
    hash.update(key.trim().as_bytes());
    hash.update(GUID.as_bytes());
    STANDARD.encode(hash.finalize())
}

pub fn frame_header(opcode: u8, length: usize) -> ([u8; 10], usize) {
    let mut header = [0u8; 10];
    header[0] = 0x80 | opcode;
    if length < 126 {
        header[1] = u8::try_from(length).unwrap_or(0);
        (header, 2)
    } else if let Ok(short) = u16::try_from(length) {
        header[1] = 126;
        header[2..4].copy_from_slice(&short.to_be_bytes());
        (header, 4)
    } else {
        header[1] = 127;
        header[2..10].copy_from_slice(&(length as u64).to_be_bytes());
        (header, 10)
    }
}

pub enum Message {
    Binary(Vec<u8>),
    Close,
}

#[derive(Default)]
pub struct Reader {
    buffer: Vec<u8>,
    partial: Vec<u8>,
}

impl Reader {
    pub fn fill(&mut self, stream: &mut TcpStream) -> io::Result<bool> {
        let mut chunk = [0u8; 16 * 1024];
        loop {
            match stream.read(&mut chunk) {
                Ok(0) => return Ok(false),
                Ok(count) => {
                    if self.buffer.len() + count > MAX_BUFFERED {
                        return Err(io::Error::other("the client sent too much at once"));
                    }
                    self.buffer.extend_from_slice(&chunk[..count]);
                }
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => return Ok(true),
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                Err(error) => return Err(error),
            }
        }
    }

    pub fn next(&mut self) -> io::Result<Option<Message>> {
        loop {
            if self.buffer.len() < 2 {
                return Ok(None);
            }
            let (first, second) = (self.buffer[0], self.buffer[1]);
            if second & 0x80 == 0 {
                return Err(io::Error::other("client frame is not masked"));
            }
            let (length, offset) = match second & 0x7f {
                126 => {
                    if self.buffer.len() < 4 {
                        return Ok(None);
                    }
                    (
                        usize::from(u16::from_be_bytes([self.buffer[2], self.buffer[3]])),
                        4,
                    )
                }
                127 => {
                    if self.buffer.len() < 10 {
                        return Ok(None);
                    }
                    let mut wide = [0u8; 8];
                    wide.copy_from_slice(&self.buffer[2..10]);
                    let length = usize::try_from(u64::from_be_bytes(wide))
                        .map_err(|_| io::Error::other("client frame is too large"))?;
                    (length, 10)
                }
                short => (usize::from(short), 2),
            };
            if length > MAX_INCOMING {
                return Err(io::Error::other("client frame is too large"));
            }
            let total = offset + 4 + length;
            if self.buffer.len() < total {
                return Ok(None);
            }
            let mask = [
                self.buffer[offset],
                self.buffer[offset + 1],
                self.buffer[offset + 2],
                self.buffer[offset + 3],
            ];
            let mut payload: Vec<u8> = self.buffer[offset + 4..total].to_vec();
            for (index, byte) in payload.iter_mut().enumerate() {
                *byte ^= mask[index & 3];
            }
            self.buffer.drain(..total);
            let last = first & 0x80 != 0;
            match first & 0x0f {
                0x8 => return Ok(Some(Message::Close)),
                0x9 | 0xa => {}
                0x0..=0x2 => {
                    if self.partial.len() + payload.len() > MAX_INCOMING {
                        return Err(io::Error::other("client message is too large"));
                    }
                    self.partial.append(&mut payload);
                    if last {
                        return Ok(Some(Message::Binary(std::mem::take(&mut self.partial))));
                    }
                }
                _ => return Err(io::Error::other("unknown client frame")),
            }
        }
    }
}

#[derive(Default)]
pub struct Writer {
    queue: VecDeque<Vec<u8>>,
    offset: usize,
    queued: usize,
}

impl Writer {
    pub fn push(&mut self, payload: &[u8]) {
        let (header, used) = frame_header(0x2, payload.len());
        let mut frame = Vec::with_capacity(used + payload.len());
        frame.extend_from_slice(&header[..used]);
        frame.extend_from_slice(payload);
        self.queued += frame.len();
        self.queue.push_back(frame);
    }

    pub fn queued(&self) -> usize {
        self.queued
    }

    pub fn flush(&mut self, stream: &mut TcpStream) -> io::Result<()> {
        while let Some(front) = self.queue.front() {
            match stream.write(&front[self.offset..]) {
                Ok(0) => return Err(io::ErrorKind::WriteZero.into()),
                Ok(count) => {
                    self.offset += count;
                    self.queued -= count;
                    if self.offset == front.len() {
                        self.queue.pop_front();
                        self.offset = 0;
                    }
                }
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => return Ok(()),
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                Err(error) => return Err(error),
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn computes_the_handshake_key_from_the_rfc() {
        assert_eq!(
            accept_key("dGhlIHNhbXBsZSBub25jZQ=="),
            "s3pPLMBiTxaQ9kYGzzhZRbK+xOo="
        );
    }

    #[test]
    fn frames_carry_their_length() {
        assert_eq!(frame_header(2, 5).1, 2);
        assert_eq!(frame_header(2, 300).1, 4);
        assert_eq!(frame_header(2, 70_000).1, 10);
    }

    #[test]
    fn reads_a_masked_message() {
        let mut reader = Reader::default();
        let mask = [1u8, 2, 3, 4];
        let body = b"hello";
        reader.buffer.extend_from_slice(&[0x82, 0x80 | 5]);
        reader.buffer.extend_from_slice(&mask);
        reader
            .buffer
            .extend(body.iter().enumerate().map(|(i, b)| b ^ mask[i & 3]));
        match reader.next().unwrap() {
            Some(Message::Binary(payload)) => assert_eq!(payload, body),
            _ => panic!("expected a message"),
        }
        assert!(reader.next().unwrap().is_none());
    }

    fn masked(body: &[u8]) -> Vec<u8> {
        let mask = [9u8, 8, 7, 6];
        let mut frame = vec![0x82, 0x80 | 127];
        frame.extend_from_slice(&(body.len() as u64).to_be_bytes());
        frame.extend_from_slice(&mask);
        frame.extend(body.iter().enumerate().map(|(i, b)| b ^ mask[i & 3]));
        frame
    }

    #[test]
    fn reads_a_message_longer_than_a_short_frame_allows() {
        let body: Vec<u8> = (0..70_000u32).map(|value| value as u8).collect();
        let mut reader = Reader {
            buffer: masked(&body),
            ..Reader::default()
        };
        match reader.next().unwrap() {
            Some(Message::Binary(payload)) => assert_eq!(payload, body),
            _ => panic!("expected a message"),
        }
    }

    #[test]
    fn refuses_a_message_over_the_limit() {
        let mut reader = Reader {
            buffer: masked(&vec![0u8; MAX_INCOMING + 1])[..14].to_vec(),
            ..Reader::default()
        };
        assert!(reader.next().is_err());
    }
}
