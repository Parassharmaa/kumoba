pub const SHIFT: u16 = 56;
pub const SHIFT_RIGHT: u16 = 60;
pub const COMMAND: u16 = 55;

const CHARACTERS: [(char, char, u16); 47] = [
    ('a', 'A', 0),
    ('s', 'S', 1),
    ('d', 'D', 2),
    ('f', 'F', 3),
    ('h', 'H', 4),
    ('g', 'G', 5),
    ('z', 'Z', 6),
    ('x', 'X', 7),
    ('c', 'C', 8),
    ('v', 'V', 9),
    ('b', 'B', 11),
    ('q', 'Q', 12),
    ('w', 'W', 13),
    ('e', 'E', 14),
    ('r', 'R', 15),
    ('y', 'Y', 16),
    ('t', 'T', 17),
    ('1', '!', 18),
    ('2', '@', 19),
    ('3', '#', 20),
    ('4', '$', 21),
    ('6', '^', 22),
    ('5', '%', 23),
    ('=', '+', 24),
    ('9', '(', 25),
    ('7', '&', 26),
    ('-', '_', 27),
    ('8', '*', 28),
    ('0', ')', 29),
    (']', '}', 30),
    ('o', 'O', 31),
    ('u', 'U', 32),
    ('[', '{', 33),
    ('i', 'I', 34),
    ('p', 'P', 35),
    ('l', 'L', 37),
    ('j', 'J', 38),
    ('\'', '"', 39),
    ('k', 'K', 40),
    (';', ':', 41),
    ('\\', '|', 42),
    (',', '<', 43),
    ('/', '?', 44),
    ('n', 'N', 45),
    ('m', 'M', 46),
    ('.', '>', 47),
    ('`', '~', 50),
];

const CODES: [(u16, u16); 108] = [
    (1, 53),
    (2, 18),
    (3, 19),
    (4, 20),
    (5, 21),
    (6, 23),
    (7, 22),
    (8, 26),
    (9, 28),
    (10, 25),
    (11, 29),
    (12, 27),
    (13, 24),
    (14, 51),
    (15, 48),
    (16, 12),
    (17, 13),
    (18, 14),
    (19, 15),
    (20, 17),
    (21, 16),
    (22, 32),
    (23, 34),
    (24, 31),
    (25, 35),
    (26, 33),
    (27, 30),
    (28, 36),
    (29, 59),
    (30, 0),
    (31, 1),
    (32, 2),
    (33, 3),
    (34, 5),
    (35, 4),
    (36, 38),
    (37, 40),
    (38, 37),
    (39, 41),
    (40, 39),
    (41, 50),
    (42, 56),
    (43, 42),
    (44, 6),
    (45, 7),
    (46, 8),
    (47, 9),
    (48, 11),
    (49, 45),
    (50, 46),
    (51, 43),
    (52, 47),
    (53, 44),
    (54, 60),
    (55, 67),
    (56, 58),
    (57, 49),
    (58, 57),
    (59, 122),
    (60, 120),
    (61, 99),
    (62, 118),
    (63, 96),
    (64, 97),
    (65, 98),
    (66, 100),
    (67, 101),
    (68, 109),
    (71, 89),
    (72, 91),
    (73, 92),
    (74, 78),
    (75, 86),
    (76, 87),
    (77, 88),
    (78, 69),
    (79, 83),
    (80, 84),
    (81, 85),
    (82, 82),
    (83, 65),
    (87, 103),
    (88, 111),
    (96, 76),
    (97, 62),
    (98, 75),
    (100, 61),
    (102, 115),
    (103, 126),
    (104, 116),
    (105, 123),
    (106, 124),
    (107, 119),
    (108, 125),
    (109, 121),
    (110, 114),
    (111, 117),
    (113, 74),
    (114, 73),
    (115, 72),
    (117, 81),
    (125, 55),
    (126, 54),
    (183, 105),
    (184, 107),
    (185, 113),
    (186, 106),
    (187, 64),
];

pub fn for_code(code: u16) -> Option<u16> {
    CODES
        .iter()
        .find(|(from, _)| *from == code)
        .map(|(_, key)| *key)
}

pub fn for_symbol(symbol: u32) -> Option<(u16, bool)> {
    match symbol {
        0xff0d => return Some((36, false)),
        0xff09 => return Some((48, false)),
        0x20 => return Some((49, false)),
        _ => {}
    }
    let character = char::from_u32(symbol)?;
    CHARACTERS.iter().find_map(|&(plain, shifted, key)| {
        if character == plain {
            Some((key, false))
        } else if character == shifted {
            Some((key, true))
        } else {
            None
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_printable_ascii_character_has_a_key() {
        for symbol in 0x20..0x7f {
            assert!(for_symbol(symbol).is_some(), "no key for {symbol:#x}");
        }
    }

    #[test]
    fn characters_that_need_shift_say_so() {
        assert_eq!(for_symbol(u32::from('a')), Some((0, false)));
        assert_eq!(for_symbol(u32::from('A')), Some((0, true)));
        assert_eq!(for_symbol(u32::from('/')), Some((44, false)));
        assert_eq!(for_symbol(u32::from('?')), Some((44, true)));
        assert_eq!(for_symbol(0xff0d), Some((36, false)));
    }

    #[test]
    fn characters_off_the_layout_have_no_key() {
        assert_eq!(for_symbol(0xe9), None);
        assert_eq!(for_symbol(0x0100_0000 + 0x3042), None);
    }

    #[test]
    fn no_two_characters_or_codes_share_a_slot() {
        let mut keys: Vec<u16> = CHARACTERS.iter().map(|entry| entry.2).collect();
        keys.sort_unstable();
        keys.dedup();
        assert_eq!(keys.len(), CHARACTERS.len());
        let mut codes: Vec<u16> = CODES.iter().map(|entry| entry.0).collect();
        codes.sort_unstable();
        codes.dedup();
        assert_eq!(codes.len(), CODES.len());
    }

    #[test]
    fn a_key_and_the_character_printed_on_it_reach_the_same_key() {
        let rows = [
            (2u16, "1234567890-="),
            (16, "qwertyuiop[]"),
            (30, "asdfghjkl;'"),
            (44, "zxcvbnm,./"),
            (41, "`"),
            (43, "\\"),
        ];
        for (first, row) in rows {
            for (code, character) in (first..).zip(row.chars()) {
                assert_eq!(
                    for_code(code),
                    for_symbol(u32::from(character)).map(|(key, _)| key),
                    "key {code} and {character:?} disagree"
                );
            }
        }
    }

    #[test]
    fn modifiers_arrows_and_unknown_codes() {
        assert_eq!(for_code(42), Some(SHIFT));
        assert_eq!(for_code(54), Some(SHIFT_RIGHT));
        assert_eq!(for_code(125), Some(COMMAND));
        assert_eq!(for_code(103), Some(126));
        assert_eq!(for_code(200), None);
    }
}
