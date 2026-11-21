use std::collections::HashMap;

pub const SHIFT_LEFT: u32 = 0xffe1;
pub const SHIFT_RIGHT: u32 = 0xffe2;
const SPARES: usize = 32;
const LEFT_FREE: usize = 4;

pub struct Keymap {
    width: usize,
    found: HashMap<u32, (u8, bool)>,
    spare: Vec<u8>,
    bound: Vec<u32>,
    used: Vec<u64>,
    clock: u64,
}

pub enum Place {
    Key(u8, bool),
    Bind(u8, bool),
}

impl Keymap {
    pub fn new(first: u8, width: usize, symbols: &[u32], owned: &[u8]) -> Self {
        let mut found = HashMap::new();
        let mut spare = Vec::new();
        let mut bound = Vec::new();
        if width > 0 {
            let rows: Vec<(u8, &[u32])> = symbols
                .chunks(width)
                .enumerate()
                .filter_map(|(index, row)| {
                    let offset = u8::try_from(index).ok()?;
                    Some((first.checked_add(offset)?, row))
                })
                .collect();
            let empty: Vec<u8> = rows
                .iter()
                .filter(|(_, row)| row.iter().all(|symbol| *symbol == 0))
                .map(|&(code, _)| code)
                .collect();
            let usable = &empty[LEFT_FREE.min(empty.len())..];
            for &(code, row) in rows.iter().rev() {
                let ours = if owned.is_empty() {
                    usable.contains(&code)
                } else {
                    owned.contains(&code)
                };
                if ours && spare.len() < SPARES {
                    spare.push(code);
                    bound.push(row[0]);
                }
            }
            for level in 0..width.min(2) {
                for &(code, row) in &rows {
                    let symbol = row.get(level).copied().unwrap_or(0);
                    if symbol != 0 && !spare.contains(&code) {
                        found.entry(symbol).or_insert((code, level == 1));
                    }
                }
            }
        }
        let used = vec![0; spare.len()];
        Self {
            width,
            found,
            spare,
            bound,
            used,
            clock: 0,
        }
    }

    pub fn spare(&self) -> &[u8] {
        &self.spare
    }

    pub fn width(&self) -> usize {
        self.width
    }

    pub fn code(&self, symbol: u32) -> Option<u8> {
        self.found.get(&symbol).map(|&(code, _)| code)
    }

    pub fn needs_key(&self, symbol: u32) -> bool {
        !self.spare.is_empty() && !self.found.contains_key(&symbol) && !self.bound.contains(&symbol)
    }

    pub fn plan(&mut self, symbols: impl Iterator<Item = u32>) -> Vec<(u8, u32, bool)> {
        let mut binds = Vec::new();
        let mut held: Vec<u32> = Vec::new();
        for symbol in symbols {
            if self.found.contains_key(&symbol) || held.contains(&symbol) {
                continue;
            }
            if held.len() == self.spare.len() {
                break;
            }
            held.push(symbol);
            if let Some(Place::Bind(code, reused)) = self.place(symbol) {
                binds.push((code, symbol, reused));
            }
        }
        binds
    }

    pub fn place(&mut self, symbol: u32) -> Option<Place> {
        if let Some(&(code, shifted)) = self.found.get(&symbol) {
            return Some(Place::Key(code, shifted));
        }
        self.clock += 1;
        if let Some(slot) = self.bound.iter().position(|bound| *bound == symbol) {
            self.used[slot] = self.clock;
            return Some(Place::Key(self.spare[slot], false));
        }
        let slot = (0..self.spare.len()).min_by_key(|slot| self.used[*slot])?;
        let reused = self.bound[slot] != 0;
        self.bound[slot] = symbol;
        self.used[slot] = self.clock;
        Some(Place::Bind(self.spare[slot], reused))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn layout() -> Keymap {
        let mut symbols = vec![0u32; 2 * 16];
        symbols[0..2].copy_from_slice(&[0x61, 0x41]);
        symbols[2..4].copy_from_slice(&[0x37, 0x26]);
        symbols[4..6].copy_from_slice(&[SHIFT_LEFT, 0]);
        symbols[6..8].copy_from_slice(&[0x26, 0x26]);
        Keymap::new(8, 2, &symbols, &[])
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
        let Some(Place::Bind(first, false)) = keys.place(0x0100_3042) else {
            panic!("expected a new binding");
        };
        assert!(first >= 16);
        assert!(matches!(keys.place(0x0100_3042), Some(Place::Key(code, false)) if code == first));
        let Some(Place::Bind(second, false)) = keys.place(0x0100_3044) else {
            panic!("expected a second binding");
        };
        assert_ne!(first, second);
    }

    #[test]
    fn spare_keys_are_reused_when_they_run_out() {
        let mut keys = layout();
        let mut codes = Vec::new();
        for symbol in 0..20u32 {
            if let Some(Place::Bind(code, _)) = keys.place(0x0100_4e00 + symbol) {
                codes.push(code);
            }
        }
        assert_eq!(codes.len(), 20);
        assert!(codes.iter().all(|code| *code >= 16));
        assert_eq!(codes[0], codes[8]);
    }

    #[test]
    fn keys_bound_by_an_earlier_run_are_found_and_reused() {
        let mut symbols = vec![0u32; 4 * 6];
        symbols[0..2].copy_from_slice(&[0x61, 0x41]);
        symbols[4..8].copy_from_slice(&[0x0100_3042, 0x0100_3042, 0, 0]);
        symbols[8..12].copy_from_slice(&[0xe9; 4]);
        symbols[12..16].copy_from_slice(&[0xff08; 4]);
        let mut keys = Keymap::new(8, 4, &symbols, &[9, 10, 12, 13]);
        assert_eq!(keys.spare().len(), 4);
        assert!(matches!(keys.place(0xe9), Some(Place::Key(10, false))));
        assert!(matches!(
            keys.place(0x0100_3042),
            Some(Place::Key(9, false))
        ));
        assert!(matches!(keys.place(0xff08), Some(Place::Key(11, false))));
        let mut fresh = Vec::new();
        for symbol in 0..4u32 {
            if let Some(Place::Bind(code, _)) = keys.place(0x0100_5000 + symbol) {
                fresh.push(code);
            }
        }
        assert_eq!(fresh.len(), 4);
        assert!(fresh.iter().all(|code| [9, 10, 12, 13].contains(code)));
        assert!(matches!(keys.place(0x61), Some(Place::Key(8, false))));
        assert!(matches!(keys.place(0xff08), Some(Place::Key(11, false))));
    }

    #[test]
    fn the_key_used_longest_ago_is_the_one_reused() {
        let mut keys = layout();
        let mut codes = Vec::new();
        for symbol in 0..8u32 {
            if let Some(Place::Bind(code, _)) = keys.place(0x0100_4e00 + symbol) {
                codes.push(code);
            }
        }
        assert!(
            matches!(keys.place(0x0100_4e00), Some(Place::Key(code, false)) if code == codes[0])
        );
        let Some(Place::Bind(next, true)) = keys.place(0x0100_5000) else {
            panic!("expected a new binding");
        };
        assert_eq!(next, codes[1]);
        assert!(
            matches!(keys.place(0x0100_4e00), Some(Place::Key(code, false)) if code == codes[0])
        );
    }

    #[test]
    fn only_empty_keys_become_spare_keys_and_the_lowest_stay_free() {
        let mut symbols = vec![0u32; 3 * 9];
        symbols[0..3].copy_from_slice(&[0x61, 0x41, 0]);
        symbols[3..6].copy_from_slice(&[0x1008_ff13, 0, 0x1008_ff13]);
        symbols[6..9].copy_from_slice(&[0xff08, 0xff08, 0xff08]);
        let keys = Keymap::new(8, 3, &symbols, &[]);
        assert_eq!(keys.spare(), &[16, 15]);
        let few = Keymap::new(8, 3, &symbols[..3 * 6], &[]);
        assert!(few.spare().is_empty());
    }

    fn typed(keys: &mut Keymap, text: &[u32]) -> Vec<u32> {
        let mut server: HashMap<u8, u32> = HashMap::new();
        let mut queue: std::collections::VecDeque<u32> = text.iter().copied().collect();
        let mut seen = Vec::new();
        let mut rounds = 0;
        while let Some(&head) = queue.front() {
            rounds += 1;
            assert!(rounds < 10_000, "typing never finished");
            if keys.needs_key(head) {
                for (code, symbol, _) in keys.plan(queue.iter().copied()) {
                    server.insert(code, symbol);
                }
                continue;
            }
            let Some(Place::Key(code, _)) = keys.place(head) else {
                panic!("a planned character had no key");
            };
            seen.push(server.get(&code).copied().unwrap_or(head));
            queue.pop_front();
        }
        seen
    }

    #[test]
    fn text_with_more_new_characters_than_spare_keys_is_typed_in_full() {
        let mut keys = layout();
        let long: Vec<u32> = (0..40u32).map(|index| 0x0100_4e00 + index % 23).collect();
        assert_eq!(typed(&mut keys, &long), long);
        let mixed: Vec<u32> = (0..60u32)
            .map(|index| {
                if index % 3 == 0 {
                    0x61
                } else {
                    0x0100_3040 + index % 19
                }
            })
            .collect();
        assert_eq!(typed(&mut keys, &mixed), mixed);
        let repeated = vec![0x0100_5000; 30];
        assert_eq!(typed(&mut keys, &repeated), repeated);
    }

    #[test]
    fn a_layout_without_spare_keys_ignores_unknown_characters() {
        let mut keys = Keymap::new(8, 1, &[0x61, 0x62], &[]);
        assert!(keys.place(0x0100_3042).is_none());
    }
}
