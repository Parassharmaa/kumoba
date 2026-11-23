use crate::bits::BitWriter;
use crate::blocks::{self, Place, Predictor, Tables};
use crate::dct::QUALITIES;
use crate::motion::Finder;
use crate::stream::{put_signed, put_varint};
use crate::{
    BAND, BLOCK, FLAG_KEY, Move, RUN_INLINE, Rect, TAG_ABOVE, TAG_DIFF, TAG_INDEX, TAG_LUMA,
    TAG_RGB, TAG_RUN, TAG_SKIP, apply_moves, cache_slot, common_prefix, same,
};

const SETTLE_FRAMES: u32 = 4;
const START_QUALITY: usize = 4;
const BYTES_PER_NEW_PIXEL: usize = 2;
const MIN_EXACT_BYTES: usize = 4096;

#[derive(Clone, Copy, Debug, Default)]
pub struct FrameStats {
    pub changed: bool,
    pub key: bool,
    pub moves: usize,
    pub bands: usize,
    pub coded_pixels: usize,
    pub lossy_blocks: usize,
    pub moved_blocks: usize,
    pub exact_blocks: usize,
    pub pending_blocks: usize,
}

#[derive(Clone, Copy, Debug)]
pub struct Budget {
    pub now: usize,
    pub steady: usize,
}

impl Budget {
    pub const UNLIMITED: Budget = Budget {
        now: usize::MAX,
        steady: usize::MAX,
    };
}

pub struct Encoder {
    width: usize,
    height: usize,
    reference: Vec<u32>,
    scratch: Vec<u32>,
    target: Vec<u32>,
    finder: Finder,
    moves: Vec<Move>,
    tokens: Vec<u8>,
    bits: BitWriter,
    busy: Vec<(usize, usize)>,
    lossy: Vec<bool>,
    origin: Vec<u64>,
    coded: Vec<u32>,
    pending: usize,
    frame: u32,
    quality: usize,
    fresh: bool,
}

fn block_hash(pixels: &[u32], stride: usize, at: Place) -> u64 {
    let mut hash = 0x9e37_79b9_7f4a_7c15u64;
    for row in 0..at.height {
        let start = (at.y + row) * stride + at.x;
        for &pixel in &pixels[start..start + at.width] {
            hash = (hash ^ u64::from(pixel)).wrapping_mul(0xff51_afd7_ed55_8ccd);
            hash ^= hash >> 29;
        }
    }
    hash
}

fn block_equal(left: &[u32], right: &[u32], stride: usize, at: Place) -> bool {
    (0..at.height).all(|row| {
        let start = (at.y + row) * stride + at.x;
        same(
            &left[start..start + at.width],
            &right[start..start + at.width],
        )
    })
}

fn new_pixels(current: &[u32], reference: &[u32], stride: usize, at: Place) -> usize {
    let mut count = 0;
    for row in 0..at.height {
        let start = (at.y + row) * stride + at.x;
        for column in 0..at.width {
            let index = start + column;
            let pixel = current[index];
            let left = at.x + column > 0 && current[index - 1] == pixel;
            let above = at.y + row > 0 && current[index - stride] == pixel;
            count += usize::from(pixel != reference[index] && !left && !above);
        }
    }
    count
}

#[inline]
fn run_value(row: &[u32], value: u32) -> usize {
    let mut at = 0;
    while at < row.len() && row[at] == value {
        at += 1;
    }
    at
}

#[inline]
fn put_run(out: &mut Vec<u8>, tag: u8, length: usize) {
    if length < RUN_INLINE {
        out.push(tag | length as u8);
    } else {
        out.push(tag | RUN_INLINE as u8);
        put_varint(out, length - RUN_INLINE);
    }
}

fn encode_band(
    reference: &mut [u32],
    current: &[u32],
    width: usize,
    rect: Rect,
    out: &mut Vec<u8>,
) {
    let mut cache = [0u32; 64];
    let mut prev = 0u32;
    for y in rect.y..rect.y + rect.height {
        let (before, rest) = reference.split_at_mut(y * width);
        let above = (y > rect.y).then(|| {
            let start = (y - 1) * width + rect.x;
            &before[start..start + rect.width]
        });
        let target = &mut rest[rect.x..rect.x + rect.width];
        let source = &current[y * width + rect.x..y * width + rect.x + rect.width];
        let mut x = 0;
        while x < rect.width {
            let pixel = source[x];
            let skip = if target[x] == pixel {
                common_prefix(&source[x..], &target[x..])
            } else {
                0
            };
            let up = match above {
                Some(above) if above[x] == pixel => common_prefix(&source[x..], &above[x..]),
                _ => 0,
            };
            let same = if pixel == prev {
                run_value(&source[x..], prev)
            } else {
                0
            };
            if skip > 0 && skip >= up && skip >= same {
                put_run(out, TAG_SKIP, skip);
                x += skip;
                prev = source[x - 1];
                continue;
            }
            if up > 0 && up >= same {
                put_run(out, TAG_ABOVE, up);
                target[x..x + up].copy_from_slice(&source[x..x + up]);
                x += up;
                prev = source[x - 1];
                continue;
            }
            if same > 0 {
                put_run(out, TAG_RUN, same);
                target[x..x + same].copy_from_slice(&source[x..x + same]);
                x += same;
                continue;
            }
            let slot = cache_slot(pixel);
            if cache[slot] == pixel {
                out.push(TAG_INDEX | slot as u8);
            } else {
                cache[slot] = pixel;
                let dr = ((pixel >> 16) & 0xff) as i32 - ((prev >> 16) & 0xff) as i32;
                let dg = ((pixel >> 8) & 0xff) as i32 - ((prev >> 8) & 0xff) as i32;
                let db = (pixel & 0xff) as i32 - (prev & 0xff) as i32;
                let (rg, bg) = (dr - dg, db - dg);
                if (-2..=1).contains(&dr) && (-2..=1).contains(&dg) && (-2..=1).contains(&db) {
                    out.push(TAG_DIFF | ((dr + 2) << 4 | (dg + 2) << 2 | (db + 2)) as u8);
                } else if (-32..=31).contains(&dg)
                    && (-8..=7).contains(&rg)
                    && (-8..=7).contains(&bg)
                {
                    out.push(TAG_LUMA | (dg + 32) as u8);
                    out.push(((rg + 8) << 4 | (bg + 8)) as u8);
                } else {
                    out.extend_from_slice(&[
                        TAG_RGB,
                        (pixel >> 16) as u8,
                        (pixel >> 8) as u8,
                        pixel as u8,
                    ]);
                }
            }
            target[x] = pixel;
            prev = pixel;
            x += 1;
        }
    }
}

impl Encoder {
    pub fn new(width: usize, height: usize) -> Self {
        let cells = width.div_ceil(BLOCK) * height.div_ceil(BLOCK);
        Self {
            width,
            height,
            reference: vec![0; width * height],
            scratch: Vec::new(),
            target: Vec::new(),
            finder: Finder::default(),
            moves: Vec::new(),
            tokens: Vec::new(),
            bits: BitWriter::default(),
            busy: Vec::new(),
            lossy: vec![false; cells],
            origin: vec![0; cells],
            coded: vec![0; cells],
            pending: 0,
            frame: 0,
            quality: START_QUALITY,
            fresh: true,
        }
    }

    pub fn reset(&mut self) {
        self.fresh = true;
    }

    pub fn reference(&self) -> &[u32] {
        &self.reference
    }

    pub fn pending(&self) -> bool {
        self.pending > 0
    }

    fn settle(&mut self, index: usize) {
        if self.lossy[index] {
            self.lossy[index] = false;
            self.pending -= 1;
        }
    }

    fn put_block(&mut self, index: usize, previous: &mut Option<usize>, exact: bool) {
        if previous.is_none() {
            self.bits.put(self.quality as u32, 4);
        }
        let gap = previous.map_or(index + 1, |last: usize| index - last);
        self.bits.unsigned(gap as u32);
        self.bits.put(u32::from(exact), 1);
        *previous = Some(index);
    }

    pub fn encode(
        &mut self,
        current: &[u32],
        area: Rect,
        hints: &[(i32, i32)],
        budget: Budget,
        out: &mut Vec<u8>,
    ) -> FrameStats {
        assert_eq!(current.len(), self.width * self.height);
        out.clear();
        self.frame = self.frame.wrapping_add(1);
        let mut stats = FrameStats::default();
        let (width, height) = (self.width, self.height);
        let mut area = Rect {
            x: area.x.min(width),
            y: area.y.min(height),
            width: area.width.min(width - area.x.min(width)),
            height: area.height.min(height - area.y.min(height)),
        };
        if self.fresh {
            self.reference.fill(0);
            self.lossy.fill(false);
            self.pending = 0;
            area = Rect {
                x: 0,
                y: 0,
                width,
                height,
            };
            stats.key = true;
        }
        let empty = area.width == 0 || area.height == 0;
        if !empty && self.pending > 0 {
            let x = area.x / BLOCK * BLOCK;
            let y = area.y / BLOCK * BLOCK;
            let right = (area.x + area.width).div_ceil(BLOCK) * BLOCK;
            let bottom = (area.y + area.height).div_ceil(BLOCK) * BLOCK;
            area = Rect {
                x,
                y,
                width: right.min(width) - x,
                height: bottom.min(height) - y,
            };
        }
        if empty && self.pending == 0 {
            return stats;
        }
        if stats.key || empty {
            self.moves.clear();
        } else {
            self.finder.find(
                &self.reference,
                current,
                width,
                area,
                hints,
                &mut self.moves,
            );
            apply_moves(&mut self.reference, &mut self.scratch, width, &self.moves);
        }
        out.push(if stats.key { FLAG_KEY } else { 0 });
        put_varint(out, self.moves.len());
        for step in &self.moves {
            put_varint(out, step.x);
            put_varint(out, step.y);
            put_varint(out, step.width);
            put_varint(out, step.height);
            put_signed(out, step.dx);
            put_signed(out, step.dy);
        }
        stats.moves = self.moves.len();

        let columns = width.div_ceil(BLOCK);
        let (left, right) = (area.x, area.x + area.width);
        let bottom = area.y + area.height;
        let cells = if empty {
            (0..0, 0..0)
        } else {
            (
                area.y / BLOCK..bottom.div_ceil(BLOCK),
                area.x / BLOCK..right.div_ceil(BLOCK),
            )
        };
        self.bits.clear();
        self.busy.clear();
        let mut masked = false;
        let mut cost = 0usize;
        for row in cells.0.clone() {
            for column in cells.1.clone() {
                let index = row * columns + column;
                let at = blocks::place(index, width, height);
                if block_equal(current, &self.reference, width, at) {
                    self.settle(index);
                    continue;
                }
                if self.lossy[index] && block_hash(current, width, at) == self.origin[index] {
                    masked = true;
                    continue;
                }
                let fresh = new_pixels(current, &self.reference, width, at);
                if fresh * 2 > at.width * at.height {
                    self.busy.push((index, fresh));
                    cost += fresh * BYTES_PER_NEW_PIXEL;
                } else {
                    self.settle(index);
                }
            }
        }
        let mut previous = None;
        let lossy = cost > budget.now;
        if lossy {
            let tables = Tables::new(self.quality);
            let mut predictor = Predictor::default();
            for at in 0..self.busy.len() {
                let index = self.busy[at].0;
                let place = blocks::place(index, width, height);
                self.put_block(index, &mut previous, false);
                let moved = blocks::encode_lossy(
                    &mut self.bits,
                    &tables,
                    &mut predictor,
                    current,
                    &mut self.reference,
                    width,
                    place,
                );
                stats.moved_blocks += usize::from(moved);
                if !self.lossy[index] {
                    self.lossy[index] = true;
                    self.pending += 1;
                }
                self.origin[index] = block_hash(current, width, place);
                self.coded[index] = self.frame;
            }
            stats.lossy_blocks = self.busy.len();
            masked = true;
            let spent = self.bits.bytes();
            if spent > budget.steady.saturating_add(budget.steady / 4) {
                let step = if spent / 2 > budget.steady { 2 } else { 1 };
                self.quality = (self.quality + step).min(QUALITIES - 1);
            } else if spent < budget.steady / 2 {
                self.quality = self.quality.saturating_sub(1);
            }
        } else {
            for at in 0..self.busy.len() {
                self.settle(self.busy[at].0);
            }
            if self.pending > 0 {
                let allowance = budget
                    .now
                    .saturating_sub(cost)
                    .min(budget.steady)
                    .max(MIN_EXACT_BYTES);
                for index in 0..self.lossy.len() {
                    if !self.lossy[index]
                        || self.frame.wrapping_sub(self.coded[index]) < SETTLE_FRAMES
                    {
                        continue;
                    }
                    let place = blocks::place(index, width, height);
                    if block_hash(current, width, place) != self.origin[index] {
                        continue;
                    }
                    if self.bits.bytes() >= allowance {
                        break;
                    }
                    self.put_block(index, &mut previous, true);
                    blocks::encode_exact(
                        &mut self.bits,
                        current,
                        &mut self.reference,
                        width,
                        place,
                    );
                    self.settle(index);
                    stats.exact_blocks += 1;
                }
            }
        }
        if previous.is_some() {
            self.bits.unsigned(0);
            let coded = self.bits.finish();
            put_varint(out, coded.len());
            out.extend_from_slice(coded);
        } else {
            put_varint(out, 0);
        }

        let source: &[u32] = if masked {
            self.target.resize(width * height, 0);
            for y in area.y..bottom {
                self.target[y * width + left..y * width + right]
                    .copy_from_slice(&current[y * width + left..y * width + right]);
            }
            for row in cells.0.clone() {
                for column in cells.1.clone() {
                    let index = row * columns + column;
                    if !self.lossy[index] {
                        continue;
                    }
                    let at = blocks::place(index, width, height);
                    let (x0, x1) = (at.x.max(left), (at.x + at.width).min(right));
                    for y in at.y.max(area.y)..(at.y + at.height).min(bottom) {
                        self.target[y * width + x0..y * width + x1]
                            .copy_from_slice(&self.reference[y * width + x0..y * width + x1]);
                    }
                }
            }
            &self.target
        } else {
            current
        };
        let mut band_top = area.y / BAND * BAND;
        while !empty && band_top < bottom {
            let y0 = band_top.max(area.y);
            let y1 = (band_top + BAND).min(bottom);
            band_top += BAND;
            let (mut x0, mut x1) = (right, left);
            for y in y0..y1 {
                let new = &source[y * width + left..y * width + right];
                let old = &self.reference[y * width + left..y * width + right];
                let first = common_prefix(new, old);
                if first == new.len() {
                    continue;
                }
                x0 = x0.min(left + first);
                let mut last = new.len();
                while new[last - 1] == old[last - 1] {
                    last -= 1;
                }
                x1 = x1.max(left + last);
            }
            if x1 <= x0 {
                continue;
            }
            let rect = Rect {
                x: x0,
                y: y0,
                width: x1 - x0,
                height: y1 - y0,
            };
            self.tokens.clear();
            encode_band(&mut self.reference, source, width, rect, &mut self.tokens);
            put_varint(out, rect.y);
            put_varint(out, rect.height);
            put_varint(out, rect.x);
            put_varint(out, rect.width);
            put_varint(out, self.tokens.len());
            out.extend_from_slice(&self.tokens);
            stats.bands += 1;
            stats.coded_pixels += rect.width * rect.height;
        }
        self.fresh = false;
        stats.pending_blocks = self.pending;
        stats.changed = stats.key || stats.moves > 0 || stats.bands > 0 || previous.is_some();
        if !stats.changed {
            out.clear();
        }
        stats
    }
}
