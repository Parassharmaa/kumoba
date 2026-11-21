use crate::{BLOCK, Move, Rect};

const STRIPS: usize = 8;
const MIN_VOTES: u32 = 12;
const MAX_CANDIDATES: usize = 3;
const MAX_MOVES: usize = 512;
const MIN_CHANGED_ROWS: usize = 24;

#[inline]
fn hash_row(row: &[u32]) -> (u64, bool) {
    let first = row[0];
    let (mut low, mut high, mut differs) = (0u32, 0u32, 0u32);
    for &pixel in row {
        differs |= pixel ^ first;
        low = low.wrapping_add(pixel.wrapping_mul(0x9e37_79b1));
        high = high.wrapping_add((pixel ^ (pixel >> 7)).wrapping_mul(0x85eb_ca6b));
    }
    let edges = u64::from(first)
        ^ (u64::from(row[row.len() - 1]) << 29)
        ^ (u64::from(row[row.len() / 2]) << 13);
    (
        (u64::from(high) << 32 | u64::from(low)) ^ edges.wrapping_mul(0xff51_afd7_ed55_8ccd),
        differs == 0,
    )
}

#[derive(Default)]
pub(crate) struct Finder {
    table: Vec<(u64, u32)>,
    votes: Vec<u32>,
    cells: Vec<u8>,
    runs: Vec<(u16, u16, u8)>,
    open: Vec<(u16, u16, u8, usize)>,
}

impl Finder {
    pub(crate) fn find(
        &mut self,
        reference: &[u32],
        current: &[u32],
        width: usize,
        area: Rect,
        hints: &[(i32, i32)],
        moves: &mut Vec<Move>,
    ) {
        moves.clear();
        if area.width < BLOCK * 2 || area.height < MIN_CHANGED_ROWS {
            return;
        }
        let (top, bottom) = (area.y, area.y + area.height);
        let left = area.x / BLOCK * BLOCK;
        let right = (area.x + area.width).div_ceil(BLOCK) * BLOCK;
        let right = right.min(width);
        let mut candidates: Vec<(i32, i32)> = Vec::new();
        for &hint in hints {
            if hint != (0, 0) && !candidates.contains(&hint) {
                candidates.push(hint);
            }
        }
        let strip_width = ((right - left) / STRIPS).max(BLOCK * 2);
        let height = reference.len() / width;
        self.votes.clear();
        self.votes.resize(height * 2 + 1, 0);
        let mut strip_left = left;
        while strip_left < right {
            let strip_right = (strip_left + strip_width).min(right);
            if strip_right - strip_left >= BLOCK {
                self.vote(
                    reference,
                    current,
                    width,
                    top,
                    bottom,
                    strip_left,
                    strip_right,
                );
            }
            strip_left = strip_right;
        }
        for _ in 0..MAX_CANDIDATES {
            let Some((index, &count)) = self
                .votes
                .iter()
                .enumerate()
                .max_by_key(|&(_, count)| *count)
            else {
                break;
            };
            if count < MIN_VOTES {
                break;
            }
            self.votes[index] = 0;
            let shift = index as i32 - height as i32;
            if shift != 0 && !candidates.contains(&(0, shift)) {
                candidates.push((0, shift));
            }
        }
        if candidates.is_empty() {
            return;
        }
        candidates.truncate(MAX_CANDIDATES + hints.len().min(2));
        let columns = (right - left).div_ceil(BLOCK);
        self.cells.clear();
        self.cells.resize(columns * (bottom - top), 0);
        let mut matched = 0usize;
        for y in top..bottom {
            let row = &current[y * width..(y + 1) * width];
            let same = &reference[y * width..(y + 1) * width];
            for column in 0..columns {
                let x0 = left + column * BLOCK;
                let x1 = (x0 + BLOCK).min(right);
                let changed = row[x0..x1] != same[x0..x1];
                for (index, &(dx, dy)) in candidates.iter().enumerate() {
                    let from_y = y as i64 - i64::from(dy);
                    let from_x0 = x0 as i64 - i64::from(dx);
                    let from_x1 = x1 as i64 - i64::from(dx);
                    if from_y < 0
                        || from_y >= height as i64
                        || from_x0 < 0
                        || from_x1 > width as i64
                    {
                        continue;
                    }
                    let base = from_y as usize * width;
                    if row[x0..x1] == reference[base + from_x0 as usize..base + from_x1 as usize] {
                        self.cells[(y - top) * columns + column] = index as u8 + 1;
                        matched += usize::from(changed);
                        break;
                    }
                }
            }
        }
        if matched < columns.min(8) * 4 {
            return;
        }
        self.open.clear();
        for y in top..=bottom {
            self.runs.clear();
            if y < bottom {
                let cells = &self.cells[(y - top) * columns..(y - top + 1) * columns];
                let mut column = 0;
                while column < columns {
                    let kind = cells[column];
                    if kind == 0 {
                        column += 1;
                        continue;
                    }
                    let start = column;
                    while column < columns && cells[column] == kind {
                        column += 1;
                    }
                    self.runs.push((start as u16, column as u16, kind));
                }
            }
            let mut index = 0;
            while index < self.open.len() {
                let (start, end, kind, from) = self.open[index];
                if self.runs.contains(&(start, end, kind)) {
                    index += 1;
                    continue;
                }
                let (dx, dy) = candidates[usize::from(kind) - 1];
                let x = left + usize::from(start) * BLOCK;
                moves.push(Move {
                    x,
                    y: from,
                    width: (left + usize::from(end) * BLOCK).min(right) - x,
                    height: y - from,
                    dx,
                    dy,
                });
                self.open.swap_remove(index);
            }
            for &(start, end, kind) in &self.runs {
                if !self
                    .open
                    .iter()
                    .any(|&(s, e, k, _)| (s, e, k) == (start, end, kind))
                {
                    self.open.push((start, end, kind, y));
                }
            }
            if moves.len() > MAX_MOVES {
                moves.clear();
                return;
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn vote(
        &mut self,
        reference: &[u32],
        current: &[u32],
        width: usize,
        top: usize,
        bottom: usize,
        left: usize,
        right: usize,
    ) {
        self.table.clear();
        for y in top..bottom {
            let (hash, flat) = hash_row(&reference[y * width + left..y * width + right]);
            if !flat {
                self.table.push((hash, y as u32));
            }
        }
        if self.table.len() < MIN_VOTES as usize {
            return;
        }
        self.table.sort_unstable();
        let height = reference.len() / width;
        for y in top..bottom {
            let row = &current[y * width + left..y * width + right];
            if row == &reference[y * width + left..y * width + right] {
                continue;
            }
            let (hash, flat) = hash_row(row);
            if flat {
                continue;
            }
            let start = self.table.partition_point(|&(other, _)| other < hash);
            for &(other, from) in self.table[start..].iter().take(4) {
                if other != hash {
                    break;
                }
                self.votes[y + height - from as usize] += 1;
            }
        }
    }
}
