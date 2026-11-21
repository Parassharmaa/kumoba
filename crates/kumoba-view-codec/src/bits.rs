#[derive(Default)]
pub(crate) struct BitWriter {
    out: Vec<u8>,
    hold: u64,
    held: u32,
}

impl BitWriter {
    pub(crate) fn clear(&mut self) {
        self.out.clear();
        self.hold = 0;
        self.held = 0;
    }

    #[inline]
    pub(crate) fn put(&mut self, value: u32, count: u32) {
        debug_assert!(count <= 32);
        if count == 0 {
            return;
        }
        let masked = u64::from(value) & ((1u64 << count) - 1);
        self.hold = (self.hold << count) | masked;
        self.held += count;
        while self.held >= 8 {
            self.held -= 8;
            self.out.push((self.hold >> self.held) as u8);
        }
    }

    #[inline]
    pub(crate) fn unsigned(&mut self, value: u32) {
        let code = u64::from(value) + 1;
        let bits = 64 - code.leading_zeros();
        self.put(0, bits - 1);
        self.put((code >> 16) as u32, bits.saturating_sub(16));
        self.put(code as u32 & 0xffff, bits.min(16));
    }

    #[inline]
    pub(crate) fn signed(&mut self, value: i32) {
        let magnitude = value.unsigned_abs();
        let mapped = if value > 0 {
            magnitude * 2 - 1
        } else {
            magnitude * 2
        };
        self.unsigned(mapped);
    }

    #[inline]
    pub(crate) fn rice(&mut self, value: i32, shift: u32) {
        let magnitude = value.unsigned_abs();
        let mapped = if value > 0 {
            magnitude * 2 - 1
        } else {
            magnitude * 2
        };
        self.unsigned(mapped >> shift);
        self.put(mapped, shift);
    }

    pub(crate) fn finish(&mut self) -> &[u8] {
        if self.held > 0 {
            let pad = 8 - self.held;
            self.put(0, pad);
        }
        &self.out
    }

    pub(crate) fn bytes(&self) -> usize {
        self.out.len()
    }
}

pub(crate) struct BitReader<'a> {
    data: &'a [u8],
    at: usize,
    hold: u64,
    held: u32,
}

impl<'a> BitReader<'a> {
    pub(crate) fn new(data: &'a [u8]) -> Self {
        Self {
            data,
            at: 0,
            hold: 0,
            held: 0,
        }
    }

    #[inline]
    pub(crate) fn get(&mut self, count: u32) -> Option<u32> {
        debug_assert!(count <= 32);
        if count == 0 {
            return Some(0);
        }
        while self.held < count {
            let byte = *self.data.get(self.at)?;
            self.at += 1;
            self.hold = (self.hold << 8) | u64::from(byte);
            self.held += 8;
        }
        self.held -= count;
        Some(((self.hold >> self.held) & ((1u64 << count) - 1)) as u32)
    }

    #[inline]
    pub(crate) fn unsigned(&mut self) -> Option<u32> {
        let mut zeros = 0;
        while self.get(1)? == 0 {
            zeros += 1;
            if zeros > 32 {
                return None;
            }
        }
        let rest = if zeros > 16 {
            (u64::from(self.get(zeros - 16)?) << 16) | u64::from(self.get(16)?)
        } else {
            u64::from(self.get(zeros)?)
        };
        u32::try_from(((1u64 << zeros) | rest) - 1).ok()
    }

    #[inline]
    pub(crate) fn signed(&mut self) -> Option<i32> {
        let mapped = self.unsigned()?;
        let magnitude = i32::try_from(mapped.div_ceil(2)).ok()?;
        Some(if mapped & 1 == 1 {
            magnitude
        } else {
            -magnitude
        })
    }

    #[inline]
    pub(crate) fn rice(&mut self, shift: u32) -> Option<i32> {
        let high = self.unsigned()?;
        let low = self.get(shift)?;
        let mapped = high.checked_shl(shift)? | low;
        if high.leading_zeros() < shift {
            return None;
        }
        let magnitude = i32::try_from(mapped.div_ceil(2)).ok()?;
        Some(if mapped & 1 == 1 {
            magnitude
        } else {
            -magnitude
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn values_round_trip() {
        let mut writer = BitWriter::default();
        let unsigned = [
            0u32,
            1,
            2,
            3,
            7,
            8,
            255,
            256,
            65_535,
            65_536,
            1 << 20,
            u32::MAX - 1,
        ];
        let signed = [0i32, 1, -1, 2, -2, 127, -128, 30_000, -30_000];
        for value in unsigned {
            writer.unsigned(value);
        }
        for value in signed {
            writer.signed(value);
        }
        for shift in 0..6 {
            for value in signed {
                writer.rice(value, shift);
            }
        }
        writer.put(0b101, 3);
        let bytes = writer.finish().to_vec();
        let mut reader = BitReader::new(&bytes);
        for value in unsigned {
            assert_eq!(reader.unsigned(), Some(value));
        }
        for value in signed {
            assert_eq!(reader.signed(), Some(value));
        }
        for shift in 0..6 {
            for value in signed {
                assert_eq!(reader.rice(shift), Some(value));
            }
        }
        assert_eq!(reader.get(3), Some(0b101));
    }

    #[test]
    fn a_short_stream_ends_cleanly() {
        let mut reader = BitReader::new(&[0, 0]);
        assert_eq!(reader.unsigned(), None);
        let mut reader = BitReader::new(&[]);
        assert_eq!(reader.get(1), None);
    }
}
