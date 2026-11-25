use base64::{Engine, engine::general_purpose::STANDARD};
use sha1::{Digest, Sha1};
use std::collections::VecDeque;
use std::io::{self, Read, Write};

const GUID: &str = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11";
pub const MAX_INCOMING: usize = 256 * 1024;
const MAX_BUFFERED: usize = 1024 * 1024;
const MAX_CONTROL: usize = 125;

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

pub fn close_frame(code: u16, reason: &str) -> Vec<u8> {
    const MAX_REASON: usize = 123;
    let mut end = reason.len().min(MAX_REASON);
    while !reason.is_char_boundary(end) {
        end -= 1;
    }
    let (header, used) = frame_header(0x8, 2 + end);
    let mut frame = header[..used].to_vec();
    frame.extend_from_slice(&code.to_be_bytes());
    frame.extend_from_slice(&reason.as_bytes()[..end]);
    frame
}

pub enum Message {
    Binary(Vec<u8>),
    Ping(Vec<u8>),
    Pong,
    Close,
}

#[derive(Default)]
pub struct Reader {
    buffer: Vec<u8>,
    partial: Vec<u8>,
}

impl Reader {
    pub fn fill(&mut self, stream: &mut impl Read) -> io::Result<bool> {
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
            let last = first & 0x80 != 0;
            if first & 0x08 != 0 && (length > MAX_CONTROL || !last) {
                return Err(io::Error::other(
                    "client control frame is too large or fragmented",
                ));
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
            match first & 0x0f {
                0x8 => return Ok(Some(Message::Close)),
                0x9 => return Ok(Some(Message::Ping(payload))),
                0xa => return Ok(Some(Message::Pong)),
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
        self.frame(0x2, payload);
    }

    pub fn ping(&mut self) {
        self.frame(0x9, &[]);
    }

    pub fn pong(&mut self, payload: &[u8]) {
        self.frame(0xa, &payload[..payload.len().min(MAX_CONTROL)]);
    }

    fn frame(&mut self, opcode: u8, payload: &[u8]) {
        let (header, used) = frame_header(opcode, payload.len());
        let mut frame = Vec::with_capacity(used + payload.len());
        frame.extend_from_slice(&header[..used]);
        frame.extend_from_slice(payload);
        self.queued += frame.len();
        self.queue.push_back(frame);
    }

    pub fn queued(&self) -> usize {
        self.queued
    }

    pub fn flush(&mut self, stream: &mut impl Write) -> io::Result<()> {
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
    fn a_close_frame_carries_its_code_and_as_much_of_the_reason_as_fits() {
        assert_eq!(close_frame(1011, "no"), [0x88, 4, 0x03, 0xf3, b'n', b'o']);
        let long = "é".repeat(100);
        let frame = close_frame(1011, &long);
        assert_eq!(frame[0], 0x88);
        assert_eq!(usize::from(frame[1]), frame.len() - 2);
        assert!(frame.len() - 2 <= 125);
        let reason = std::str::from_utf8(&frame[4..]).unwrap();
        assert_eq!(reason, "é".repeat(61));
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

    fn control(first: u8, body: &[u8]) -> Reader {
        let mask = [5u8, 6, 7, 8];
        let mut buffer = vec![first, 0x80 | 126];
        buffer.extend_from_slice(&(body.len() as u16).to_be_bytes());
        buffer.extend_from_slice(&mask);
        buffer.extend(body.iter().enumerate().map(|(i, b)| b ^ mask[i & 3]));
        Reader {
            buffer,
            ..Reader::default()
        }
    }

    #[test]
    fn hands_over_a_ping_with_its_payload_and_notes_a_pong() {
        match control(0x89, b"are you there").next().unwrap() {
            Some(Message::Ping(payload)) => assert_eq!(payload, b"are you there"),
            _ => panic!("expected a ping"),
        }
        assert!(matches!(
            control(0x8a, b"").next().unwrap(),
            Some(Message::Pong)
        ));
    }

    #[test]
    fn a_ping_between_the_pieces_of_a_message_leaves_the_message_whole() {
        let mut reader = control(0x02, b"hel");
        reader.buffer.extend(control(0x89, b"x").buffer);
        reader.buffer.extend(control(0x80, b"lo").buffer);
        assert!(matches!(reader.next().unwrap(), Some(Message::Ping(_))));
        match reader.next().unwrap() {
            Some(Message::Binary(payload)) => assert_eq!(payload, b"hello"),
            _ => panic!("expected the message"),
        }
    }

    #[test]
    fn refuses_control_frames_that_are_too_long_or_split() {
        for opcode in [0x8, 0x9, 0xa] {
            assert!(control(0x80 | opcode, &[0u8; 126]).next().is_err());
            assert!(control(opcode, b"part").next().is_err());
            assert!(control(0x80 | opcode, &[0u8; 125]).next().is_ok());
        }
    }

    #[test]
    fn writes_pings_and_pongs_as_short_control_frames() {
        let mut writer = Writer::default();
        writer.ping();
        writer.pong(b"abc");
        writer.pong(&[7u8; 300]);
        let mut sent = Vec::new();
        writer.flush(&mut sent).unwrap();
        let mut expected = vec![0x89, 0x00, 0x8a, 0x03, b'a', b'b', b'c', 0x8a, 125];
        expected.extend_from_slice(&[7u8; 125]);
        assert_eq!(sent, expected);
    }
}
