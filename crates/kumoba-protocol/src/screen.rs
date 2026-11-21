pub const GRAB: u8 = 1;
pub const KEY: u8 = 2;
pub const POINTER: u8 = 3;
pub const WHEEL: u8 = 4;

pub const HELLO: u8 = 1;
pub const DAMAGE: u8 = 2;
pub const GRABBED: u8 = 3;
pub const REFUSED: u8 = 4;

pub const LEFT: u8 = 1;
pub const RIGHT: u8 = 2;
pub const MIDDLE: u8 = 4;

pub const MAX_SIDE: u16 = 8192;

pub fn frame(kind: u8, payload: &[u8]) -> Vec<u8> {
    let payload = &payload[..payload.len().min(usize::from(u8::MAX))];
    let length = u8::try_from(payload.len()).unwrap_or(u8::MAX);
    let mut message = Vec::with_capacity(2 + payload.len());
    message.push(kind);
    message.push(length);
    message.extend_from_slice(payload);
    message
}

pub fn size(width: u16, height: u16) -> [u8; 4] {
    let (width, height) = (width.to_le_bytes(), height.to_le_bytes());
    [width[0], width[1], height[0], height[1]]
}

pub fn pair(payload: &[u8]) -> Option<(u16, u16)> {
    let bytes = payload.get(..4)?;
    Some((
        u16::from_le_bytes([bytes[0], bytes[1]]),
        u16::from_le_bytes([bytes[2], bytes[3]]),
    ))
}

#[derive(Default)]
pub struct Parser {
    buffer: Vec<u8>,
}

impl Parser {
    pub fn push(&mut self, bytes: &[u8]) {
        self.buffer.extend_from_slice(bytes);
    }

    pub fn take(&mut self) -> Option<(u8, Vec<u8>)> {
        let length = usize::from(*self.buffer.get(1)?);
        if self.buffer.len() < 2 + length {
            return None;
        }
        let kind = self.buffer[0];
        let payload = self.buffer[2..2 + length].to_vec();
        self.buffer.drain(..2 + length);
        Some((kind, payload))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn messages_survive_arriving_in_pieces() {
        let mut bytes = frame(POINTER, &[1, 2, 3, 4, LEFT]);
        bytes.extend(frame(GRAB, &[]));
        bytes.extend(frame(GRABBED, &size(1280, 800)));
        let mut parser = Parser::default();
        let mut seen = Vec::new();
        for byte in bytes {
            parser.push(&[byte]);
            while let Some(message) = parser.take() {
                seen.push(message);
            }
        }
        assert_eq!(seen.len(), 3);
        assert_eq!(seen[0], (POINTER, vec![1, 2, 3, 4, LEFT]));
        assert_eq!(seen[1], (GRAB, Vec::new()));
        assert_eq!(pair(&seen[2].1), Some((1280, 800)));
    }

    #[test]
    fn a_long_payload_is_cut_to_what_one_message_holds() {
        let mut parser = Parser::default();
        parser.push(&frame(REFUSED, &[b'x'; 400]));
        assert_eq!(parser.take(), Some((REFUSED, vec![b'x'; 255])));
        assert_eq!(parser.take(), None);
    }

    #[test]
    fn a_short_size_is_not_read() {
        assert_eq!(pair(&[1, 2, 3]), None);
    }
}
