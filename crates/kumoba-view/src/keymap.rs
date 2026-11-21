use std::collections::HashMap;

pub const SHIFT_LEFT: u32 = 0xffe1;
pub const SHIFT_RIGHT: u32 = 0xffe2;
const SPARES: usize = 8;

pub struct Keymap {
    first: u8,
    width: usize,
    found: HashMap<u32, (u8, bool)>,
    spare: Vec<u8>,
    bound: Vec<u32>,
    next: usize,
}

pub enum Place {
    Key(u8, bool),
    Bind(u8),
}

impl Keymap {
    pub fn new(first: u8, width: usize, symbols: &[u32]) -> Self {
        let mut found = HashMap::new();
        let mut spare = Vec::new();
        if width > 0 {
            for level in 0..width.min(2) {
                for (index, row) in symbols.chunks(width).enumerate() {
                    let symbol = row.get(level).copied().unwrap_or(0);
                    let Ok(offset) = u8::try_from(index) else {
                        break;
                    };
                    let Some(code) = first.checked_add(offset) else {
                        break;
                    };
                    if symbol != 0 {
                        found.entry(symbol).or_insert((code, level == 1));
                    }
                }
            }
            for (index, row) in symbols.chunks(width).enumerate().rev() {
                if spare.len() == SPARES {
                    break;
                }
                if row.iter().all(|symbol| *symbol == 0)
                    && let Some(code) = u8::try_from(index)
                        .ok()
                        .and_then(|offset| first.checked_add(offset))
                {
                    spare.push(code);
                }
            }
        }
        let bound = vec![0; spare.len()];
        Self {
            first,
            width,
            found,
            spare,
            bound,
            next: 0,
        }
    }

    pub fn first(&self) -> u8 {
        self.first
    }

    pub fn width(&self) -> usize {
        self.width
    }

    pub fn code(&self, symbol: u32) -> Option<u8> {
        self.found.get(&symbol).map(|&(code, _)| code)
    }

    pub fn place(&mut self, symbol: u32) -> Option<Place> {
        if let Some(&(code, shifted)) = self.found.get(&symbol) {
            return Some(Place::Key(code, shifted));
        }
        if let Some(slot) = self.bound.iter().position(|bound| *bound == symbol) {
            return Some(Place::Key(self.spare[slot], false));
        }
        if self.spare.is_empty() {
            return None;
        }
        let slot = self.next % self.spare.len();
        self.next += 1;
        self.bound[slot] = symbol;
        Some(Place::Bind(self.spare[slot]))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn layout() -> Keymap {
        let mut symbols = vec![0u32; 2 * 12];
        symbols[0..2].copy_from_slice(&[0x61, 0x41]);
        symbols[2..4].copy_from_slice(&[0x37, 0x26]);
        symbols[4..6].copy_from_slice(&[SHIFT_LEFT, 0]);
        symbols[6..8].copy_from_slice(&[0x26, 0x26]);
        Keymap::new(8, 2, &symbols)
    }

    #[test]
    fn a_character_maps_to_its_key_and_shift_level() {
        let mut keys = layout();
        assert!(matches!(keys.place(0x61), Some(Place::Key(8, false))));
        assert!(matches!(keys.place(0x41), Some(Place::Key(8, true))));
        assert!(matches!(keys.place(0x37), Some(Place::Key(9, false))));
        assert_eq!(keys.code(SHIFT_LEFT), Some(10));
    }

    #[test]
    fn an_unshifted_key_wins_over_a_shifted_one() {
        let mut keys = layout();
        assert!(matches!(keys.place(0x26), Some(Place::Key(11, false))));
    }

    #[test]
    fn a_character_the_layout_lacks_gets_a_spare_key_and_keeps_it() {
        let mut keys = layout();
        let Some(Place::Bind(first)) = keys.place(0x0100_3042) else {
            panic!("expected a new binding");
        };
        assert!(first >= 12);
        assert!(matches!(keys.place(0x0100_3042), Some(Place::Key(code, false)) if code == first));
        let Some(Place::Bind(second)) = keys.place(0x0100_3044) else {
            panic!("expected a second binding");
        };
        assert_ne!(first, second);
    }

    #[test]
    fn spare_keys_are_reused_when_they_run_out() {
        let mut keys = layout();
        let mut codes = Vec::new();
        for symbol in 0..20u32 {
            if let Some(Place::Bind(code)) = keys.place(0x0100_4e00 + symbol) {
                codes.push(code);
            }
        }
        assert_eq!(codes.len(), 20);
        assert_eq!(codes[0], codes[8]);
    }

    #[test]
    fn a_layout_without_spare_keys_ignores_unknown_characters() {
        let mut keys = Keymap::new(8, 1, &[0x61, 0x62]);
        assert!(keys.place(0x0100_3042).is_none());
    }
}
