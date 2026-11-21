use crate::bits::BitReader;
use crate::blocks::{self, Canvas, Layout, Predictor, Tables};
use crate::dct::QUALITIES;
use crate::stream::Reader;
use crate::{
    BAND, BLOCK, FLAG_KEY, MAX_HEIGHT, MAX_WIDTH, Move, RUN_INLINE, Rect, TAG_ABOVE, TAG_DIFF,
    TAG_LUMA, TAG_RGB, TAG_RUN, TAG_SKIP, apply_moves, cache_slot,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DecodeError {
    Truncated,
    OutOfBounds,
    Invalid,
}

pub struct Decoder {
    width: usize,
    height: usize,
    pixels: Vec<u32>,
    scratch: Vec<u32>,
    moves: Vec<Move>,
}

const OPAQUE: u32 = 0xff00_0000;
const MAX_MOVES: usize = 1024;

#[inline]
fn to_canvas(rgb: u32) -> u32 {
    Canvas::pack(rgb)
}

#[inline]
fn from_canvas(pixel: u32) -> u32 {
    Canvas::unpack(pixel)
}

fn inside(start: usize, length: usize, limit: usize) -> bool {
    length > 0 && start.checked_add(length).is_some_and(|end| end <= limit)
}

impl Decoder {
    pub fn new(width: usize, height: usize) -> Option<Self> {
        if width == 0 || height == 0 || width > MAX_WIDTH || height > MAX_HEIGHT {
            return None;
        }
        Some(Self {
            width,
            height,
            pixels: vec![OPAQUE; width * height],
            scratch: Vec::new(),
            moves: Vec::new(),
        })
    }

    pub fn width(&self) -> usize {
        self.width
    }

    pub fn height(&self) -> usize {
        self.height
    }

    pub fn pixels(&self) -> &[u32] {
        &self.pixels
    }

    pub fn decode(&mut self, data: &[u8]) -> Result<Rect, DecodeError> {
        use DecodeError::{Invalid, OutOfBounds, Truncated};
        let (width, height) = (self.width, self.height);
        let mut reader = Reader::new(data);
        let flags = reader.byte().ok_or(Truncated)?;
        let mut dirty = Rect {
            x: 0,
            y: 0,
            width: 0,
            height: 0,
        };
        if flags & FLAG_KEY != 0 {
            self.pixels.fill(OPAQUE);
            dirty = Rect {
                x: 0,
                y: 0,
                width,
                height,
            };
        }
        let count = reader.varint().ok_or(Truncated)?;
        if count > MAX_MOVES {
            return Err(Invalid);
        }
        self.moves.clear();
        let mut moved = 0usize;
        for _ in 0..count {
            let x = reader.varint().ok_or(Truncated)?;
            let y = reader.varint().ok_or(Truncated)?;
            let w = reader.varint().ok_or(Truncated)?;
            let h = reader.varint().ok_or(Truncated)?;
            let dx = reader.signed().ok_or(Truncated)?;
            let dy = reader.signed().ok_or(Truncated)?;
            if !inside(x, w, width) || !inside(y, h, height) {
                return Err(OutOfBounds);
            }
            let from_x = x as i64 - i64::from(dx);
            let from_y = y as i64 - i64::from(dy);
            if from_x < 0
                || from_y < 0
                || from_x + w as i64 > width as i64
                || from_y + h as i64 > height as i64
            {
                return Err(OutOfBounds);
            }
            moved += w * h;
            if moved > width * height {
                return Err(Invalid);
            }
            self.moves.push(Move {
                x,
                y,
                width: w,
                height: h,
                dx,
                dy,
            });
            dirty = dirty.union(Rect {
                x,
                y,
                width: w,
                height: h,
            });
        }
        apply_moves(&mut self.pixels, &mut self.scratch, width, &self.moves);
        let length = reader.varint().ok_or(Truncated)?;
        if length > 0 {
            let coded = reader.take(length).ok_or(Truncated)?;
            let mut bits = BitReader::new(coded);
            let quality = bits.get(4).ok_or(Truncated)? as usize;
            if quality >= QUALITIES {
                return Err(Invalid);
            }
            let tables = Tables::new(quality);
            let mut predictor = Predictor::default();
            let cells = width.div_ceil(BLOCK) * height.div_ceil(BLOCK);
            let mut next = 0usize;
            loop {
                let gap = bits.unsigned().ok_or(Truncated)? as usize;
                if gap == 0 {
                    break;
                }
                let index = next.checked_add(gap - 1).ok_or(Invalid)?;
                if index >= cells {
                    return Err(OutOfBounds);
                }
                next = index + 1;
                let at = blocks::place(index, width, height);
                if bits.get(1).ok_or(Truncated)? == 1 {
                    blocks::decode_exact::<Canvas>(&mut bits, &mut self.pixels, width, at)
                        .ok_or(Invalid)?;
                } else {
                    blocks::decode_lossy::<Canvas>(
                        &mut bits,
                        &tables,
                        &mut predictor,
                        &mut self.pixels,
                        width,
                        at,
                    )
                    .ok_or(Invalid)?;
                }
                dirty = dirty.union(Rect {
                    x: at.x,
                    y: at.y,
                    width: at.width,
                    height: at.height,
                });
            }
        }
        while !reader.done() {
            let y = reader.varint().ok_or(Truncated)?;
            let h = reader.varint().ok_or(Truncated)?;
            let x = reader.varint().ok_or(Truncated)?;
            let w = reader.varint().ok_or(Truncated)?;
            let length = reader.varint().ok_or(Truncated)?;
            if h > BAND || !inside(x, w, width) || !inside(y, h, height) {
                return Err(OutOfBounds);
            }
            let tokens = reader.take(length).ok_or(Truncated)?;
            let rect = Rect {
                x,
                y,
                width: w,
                height: h,
            };
            decode_band(&mut self.pixels, width, rect, tokens)?;
            dirty = dirty.union(rect);
        }
        Ok(dirty)
    }
}

fn decode_band(
    pixels: &mut [u32],
    width: usize,
    rect: Rect,
    tokens: &[u8],
) -> Result<(), DecodeError> {
    use DecodeError::{Invalid, Truncated};
    let mut reader = Reader::new(tokens);
    let mut cache = [0u32; 64];
    let mut prev = 0u32;
    for y in rect.y..rect.y + rect.height {
        let (before, rest) = pixels.split_at_mut(y * width);
        let above = (y > rect.y).then(|| {
            let start = (y - 1) * width + rect.x;
            &before[start..start + rect.width]
        });
        let target = &mut rest[rect.x..rect.x + rect.width];
        let mut x = 0;
        while x < rect.width {
            let tag = reader.byte().ok_or(Truncated)?;
            if (TAG_RUN..TAG_RGB).contains(&tag) {
                let mut length = usize::from(tag & 0x0f);
                if length == RUN_INLINE {
                    length = length
                        .checked_add(reader.varint().ok_or(Truncated)?)
                        .ok_or(Invalid)?;
                }
                if length == 0 || length > rect.width - x {
                    return Err(Invalid);
                }
                match tag & 0xf0 {
                    TAG_SKIP => {}
                    TAG_ABOVE => {
                        let above = above.ok_or(Invalid)?;
                        target[x..x + length].copy_from_slice(&above[x..x + length]);
                    }
                    _ => target[x..x + length].fill(to_canvas(prev)),
                }
                x += length;
                prev = from_canvas(target[x - 1]);
                continue;
            }
            let pixel = if tag < TAG_DIFF {
                cache[usize::from(tag)]
            } else if tag < TAG_LUMA {
                let dr = i32::from((tag >> 4) & 3) - 2;
                let dg = i32::from((tag >> 2) & 3) - 2;
                let db = i32::from(tag & 3) - 2;
                shift(prev, dr, dg, db)
            } else if tag < TAG_RUN {
                let second = reader.byte().ok_or(Truncated)?;
                let dg = i32::from(tag & 0x3f) - 32;
                let dr = dg + i32::from(second >> 4) - 8;
                let db = dg + i32::from(second & 0x0f) - 8;
                shift(prev, dr, dg, db)
            } else if tag == TAG_RGB {
                let rgb = reader.take(3).ok_or(Truncated)?;
                u32::from(rgb[0]) << 16 | u32::from(rgb[1]) << 8 | u32::from(rgb[2])
            } else {
                return Err(Invalid);
            };
            cache[cache_slot(pixel)] = pixel;
            target[x] = to_canvas(pixel);
            prev = pixel;
            x += 1;
        }
    }
    if reader.done() { Ok(()) } else { Err(Invalid) }
}

#[inline]
fn shift(prev: u32, dr: i32, dg: i32, db: i32) -> u32 {
    let r = (((prev >> 16) & 0xff) as i32 + dr) as u32 & 0xff;
    let g = (((prev >> 8) & 0xff) as i32 + dg) as u32 & 0xff;
    let b = ((prev & 0xff) as i32 + db) as u32 & 0xff;
    r << 16 | g << 8 | b
}
