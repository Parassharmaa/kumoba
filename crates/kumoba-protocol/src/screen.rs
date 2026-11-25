use std::io;

pub const GRAB: u8 = 1;
pub const KEY: u8 = 2;
pub const POINTER: u8 = 3;
pub const WHEEL: u8 = 4;
pub const PASTE: u8 = 5;
pub const CLIPBOARD_SET: u8 = 6;
pub const COPY: u8 = 7;
pub const PULL: u8 = 8;

pub const HELLO: u8 = 1;
pub const DAMAGE: u8 = 2;
pub const GRABBED: u8 = 3;
pub const REFUSED: u8 = 4;
pub const CURSOR: u8 = 5;
pub const CLIPBOARD: u8 = 6;

pub const LEFT: u8 = 1;
pub const RIGHT: u8 = 2;
pub const MIDDLE: u8 = 4;

pub const MAX_SIDE: u16 = 8192;
pub const MAX_TEXT: usize = 800_000;
pub const MAX_CURSOR: u16 = 128;
pub const MAX_MESSAGE: usize = 1 << 20;
const HEAD: usize = 5;

pub fn frame(kind: u8, payload: &[u8]) -> Vec<u8> {
    let payload = &payload[..payload.len().min(MAX_MESSAGE)];
    let length = u32::try_from(payload.len()).unwrap_or(u32::MAX);
    let mut message = Vec::with_capacity(HEAD + payload.len());
    message.push(kind);
    message.extend_from_slice(&length.to_le_bytes());
    message.extend_from_slice(payload);
    message
}

pub fn text(value: &[u8]) -> &[u8] {
    if value.len() <= MAX_TEXT {
        return value;
    }
    let mut end = MAX_TEXT;
    while end > 0 && value[end] & 0xc0 == 0x80 {
        end -= 1;
    }
    &value[..end]
}

pub fn cursor(width: u16, height: u16, hot_x: u16, hot_y: u16, rgba: &[u8]) -> Option<Vec<u8>> {
    let pixels = usize::from(width) * usize::from(height) * 4;
    let fits = (1..=MAX_CURSOR).contains(&width) && (1..=MAX_CURSOR).contains(&height);
    if !fits || rgba.len() != pixels || hot_x >= width || hot_y >= height {
        return None;
    }
    let mut payload = Vec::with_capacity(8 + pixels);
    for value in [width, height, hot_x, hot_y] {
        payload.extend_from_slice(&value.to_le_bytes());
    }
    payload.extend_from_slice(rgba);
    Some(frame(CURSOR, &payload))
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

    pub fn take(&mut self) -> io::Result<Option<(u8, Vec<u8>)>> {
        let Some(head) = self.buffer.get(..HEAD) else {
            return Ok(None);
        };
        let length = usize::try_from(u32::from_le_bytes([head[1], head[2], head[3], head[4]]))
            .unwrap_or(usize::MAX);
        if length > MAX_MESSAGE {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "a screen message is larger than allowed",
            ));
        }
        if self.buffer.len() < HEAD + length {
            return Ok(None);
        }
        let kind = self.buffer[0];
        let payload = self.buffer[HEAD..HEAD + length].to_vec();
        self.buffer.drain(..HEAD + length);
        Ok(Some((kind, payload)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn all(parser: &mut Parser) -> Vec<(u8, Vec<u8>)> {
        let mut seen = Vec::new();
        while let Some(message) = parser.take().unwrap() {
            seen.push(message);
        }
        seen
    }

    #[test]
    fn messages_survive_arriving_in_pieces() {
        let mut bytes = frame(POINTER, &[1, 2, 3, 4, LEFT]);
        bytes.extend(frame(GRAB, &[]));
        bytes.extend(frame(GRABBED, &size(1280, 800)));
        let mut parser = Parser::default();
        let mut seen = Vec::new();
        for byte in bytes {
            parser.push(&[byte]);
            seen.extend(all(&mut parser));
        }
        assert_eq!(seen.len(), 3);
        assert_eq!(seen[0], (POINTER, vec![1, 2, 3, 4, LEFT]));
        assert_eq!(seen[1], (GRAB, Vec::new()));
        assert_eq!(pair(&seen[2].1), Some((1280, 800)));
    }

    #[test]
    fn long_messages_such_as_clipboard_text_arrive_whole() {
        let long = "あ".repeat(100_000);
        let mut parser = Parser::default();
        parser.push(&frame(CLIPBOARD, long.as_bytes()));
        assert_eq!(all(&mut parser), [(CLIPBOARD, long.into_bytes())]);
    }

    #[test]
    fn a_message_that_claims_to_be_too_large_is_refused_before_it_arrives() {
        let mut parser = Parser::default();
        let mut head = vec![CLIPBOARD];
        head.extend_from_slice(&u32::MAX.to_le_bytes());
        parser.push(&head);
        assert!(parser.take().is_err());
    }

    #[test]
    fn text_is_cut_at_a_character_boundary() {
        let long = "日".repeat(MAX_TEXT);
        let cut = text(long.as_bytes());
        assert!(cut.len() <= MAX_TEXT);
        assert!(cut.len() > MAX_TEXT - 3);
        assert!(std::str::from_utf8(cut).is_ok());
        assert_eq!(text(b"short"), b"short");
    }

    #[test]
    fn a_cursor_must_fit_and_hold_its_hot_spot() {
        let rgba = vec![255; 4 * 4 * 4];
        let message = cursor(4, 4, 1, 2, &rgba).unwrap();
        let mut parser = Parser::default();
        parser.push(&message);
        let (kind, payload) = parser.take().unwrap().unwrap();
        assert_eq!(kind, CURSOR);
        assert_eq!(pair(&payload), Some((4, 4)));
        assert_eq!(pair(&payload[4..]), Some((1, 2)));
        assert_eq!(&payload[8..], &rgba[..]);
        assert!(cursor(4, 4, 4, 0, &rgba).is_none());
        assert!(cursor(4, 4, 0, 0, &rgba[1..]).is_none());
        assert!(
            cursor(
                MAX_CURSOR + 1,
                1,
                0,
                0,
                &vec![0; usize::from(MAX_CURSOR + 1) * 4]
            )
            .is_none()
        );
    }

    #[test]
    fn a_short_size_is_not_read() {
        assert_eq!(pair(&[1, 2, 3]), None);
    }
}
