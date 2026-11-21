pub(crate) fn put_varint(out: &mut Vec<u8>, mut value: usize) {
    while value >= 0x80 {
        out.push((value as u8 & 0x7f) | 0x80);
        value >>= 7;
    }
    out.push(value as u8);
}

pub(crate) fn put_signed(out: &mut Vec<u8>, value: i32) {
    put_varint(out, ((value << 1) ^ (value >> 31)) as u32 as usize);
}

pub(crate) struct Reader<'a> {
    data: &'a [u8],
    at: usize,
}

impl<'a> Reader<'a> {
    pub(crate) fn new(data: &'a [u8]) -> Self {
        Self { data, at: 0 }
    }

    #[inline]
    pub(crate) fn byte(&mut self) -> Option<u8> {
        let value = *self.data.get(self.at)?;
        self.at += 1;
        Some(value)
    }

    pub(crate) fn varint(&mut self) -> Option<usize> {
        let mut value = 0u32;
        let mut shift = 0;
        loop {
            let byte = self.byte()?;
            if shift > 21 {
                return None;
            }
            value |= u32::from(byte & 0x7f) << shift;
            if byte & 0x80 == 0 {
                return Some(value as usize);
            }
            shift += 7;
        }
    }

    pub(crate) fn signed(&mut self) -> Option<i32> {
        let raw = self.varint()? as u32;
        Some(((raw >> 1) as i32) ^ -((raw & 1) as i32))
    }

    pub(crate) fn take(&mut self, count: usize) -> Option<&'a [u8]> {
        let end = self.at.checked_add(count)?;
        let slice = self.data.get(self.at..end)?;
        self.at = end;
        Some(slice)
    }

    pub(crate) fn done(&self) -> bool {
        self.at == self.data.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers_round_trip() {
        let mut out = Vec::new();
        for value in [0usize, 1, 127, 128, 300, 70_000, 4_000_000, (1 << 28) - 1] {
            put_varint(&mut out, value);
        }
        for value in [0i32, 1, -1, 63, -64, 4096, -4096] {
            put_signed(&mut out, value);
        }
        let mut reader = Reader::new(&out);
        for value in [0usize, 1, 127, 128, 300, 70_000, 4_000_000, (1 << 28) - 1] {
            assert_eq!(reader.varint(), Some(value));
        }
        for value in [0i32, 1, -1, 63, -64, 4096, -4096] {
            assert_eq!(reader.signed(), Some(value));
        }
        assert!(reader.done());
    }

    #[test]
    fn a_number_longer_than_four_bytes_is_refused() {
        assert_eq!(Reader::new(&[0xff, 0xff, 0xff, 0xff, 0x01]).varint(), None);
        assert_eq!(
            Reader::new(&[0xff, 0xff, 0xff, 0x7f]).varint(),
            Some((1 << 28) - 1)
        );
        assert_eq!(Reader::new(&[0x80]).varint(), None);
    }
}
