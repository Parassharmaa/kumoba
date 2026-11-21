use std::collections::HashMap;

const MAX_WINDOWS: usize = 512;
const MAX_HINTS: usize = 3;

#[derive(Default)]
pub struct Hints {
    windows: HashMap<u32, (i32, i32)>,
    origins: HashMap<u32, (i32, i32)>,
    pointer: Option<(i32, i32)>,
    pointer_origin: Option<(i32, i32)>,
}

impl Hints {
    pub fn window(&mut self, id: u32, x: i32, y: i32) {
        if self.windows.len() >= MAX_WINDOWS && !self.windows.contains_key(&id) {
            self.windows.clear();
            self.origins.clear();
        }
        if let Some(previous) = self.windows.insert(id, (x, y)) {
            self.origins.entry(id).or_insert(previous);
        }
    }

    pub fn pointer(&mut self, x: i32, y: i32) {
        self.pointer = Some((x, y));
        self.pointer_origin.get_or_insert((x, y));
    }

    pub fn take(&mut self) -> Vec<(i32, i32)> {
        let mut moves = Vec::new();
        for (id, origin) in self.origins.drain() {
            if let Some(&now) = self.windows.get(&id) {
                let delta = (now.0 - origin.0, now.1 - origin.1);
                if delta != (0, 0) && !moves.contains(&delta) {
                    moves.push(delta);
                }
            }
        }
        if let (Some(now), Some(origin)) = (self.pointer, self.pointer_origin) {
            let delta = (now.0 - origin.0, now.1 - origin.1);
            if delta != (0, 0) && !moves.contains(&delta) {
                moves.push(delta);
            }
        }
        self.pointer_origin = self.pointer;
        moves.truncate(MAX_HINTS);
        moves
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_moved_window_gives_its_displacement_once() {
        let mut hints = Hints::default();
        hints.window(7, 100, 100);
        assert!(hints.take().is_empty());
        hints.window(7, 110, 96);
        hints.window(7, 130, 90);
        assert_eq!(hints.take(), vec![(30, -10)]);
        assert!(hints.take().is_empty());
    }

    #[test]
    fn a_window_that_returns_gives_nothing() {
        let mut hints = Hints::default();
        hints.window(7, 100, 100);
        hints.take();
        hints.window(7, 140, 100);
        hints.window(7, 100, 100);
        assert!(hints.take().is_empty());
    }

    #[test]
    fn the_pointer_displacement_is_a_candidate_too() {
        let mut hints = Hints::default();
        hints.pointer(10, 10);
        assert!(hints.take().is_empty());
        hints.pointer(25, 4);
        hints.window(3, 0, 0);
        hints.window(3, 15, -6);
        assert_eq!(hints.take(), vec![(15, -6)]);
        hints.pointer(30, 4);
        assert_eq!(hints.take(), vec![(5, 0)]);
    }

    #[test]
    fn many_windows_do_not_grow_without_bound() {
        let mut hints = Hints::default();
        for id in 0..5000 {
            hints.window(id, 0, 0);
            hints.window(id, 1, 1);
            assert!(hints.windows.len() <= MAX_WINDOWS);
            assert!(hints.origins.len() <= MAX_WINDOWS);
        }
        assert!(hints.take().len() <= MAX_HINTS);
    }
}
