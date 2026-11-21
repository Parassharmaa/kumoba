use crate::BLOCK;
use crate::bits::{BitReader, BitWriter};
use crate::dct::{self, ZIGZAG};

pub(crate) const AREA: usize = BLOCK * BLOCK;
const MAX_LEVEL: u32 = 1 << 14;
const MAX_SHIFT: u32 = 7;

pub(crate) trait Layout {
    fn pack(rgb: u32) -> u32;
    fn unpack(pixel: u32) -> u32;
}

pub(crate) struct Native;
pub(crate) struct Canvas;

impl Layout for Native {
    #[inline]
    fn pack(rgb: u32) -> u32 {
        rgb
    }
    #[inline]
    fn unpack(pixel: u32) -> u32 {
        pixel
    }
}

impl Layout for Canvas {
    #[inline]
    fn pack(rgb: u32) -> u32 {
        0xff00_0000 | ((rgb & 0xff) << 16) | (rgb & 0xff00) | ((rgb >> 16) & 0xff)
    }
    #[inline]
    fn unpack(pixel: u32) -> u32 {
        ((pixel & 0xff) << 16) | (pixel & 0xff00) | ((pixel >> 16) & 0xff)
    }
}

#[derive(Clone, Copy)]
pub(crate) struct Place {
    pub x: usize,
    pub y: usize,
    pub width: usize,
    pub height: usize,
}

pub(crate) fn place(index: usize, width: usize, height: usize) -> Place {
    let columns = width.div_ceil(BLOCK);
    let (x, y) = (index % columns * BLOCK, index / columns * BLOCK);
    Place {
        x,
        y,
        width: BLOCK.min(width - x),
        height: BLOCK.min(height - y),
    }
}

pub(crate) struct Tables {
    luma: [i32; 64],
    chroma: [i32; 64],
}

impl Tables {
    pub(crate) fn new(quality: usize) -> Self {
        Self {
            luma: dct::steps(quality, false),
            chroma: dct::steps(quality, true),
        }
    }
}

#[derive(Default)]
pub(crate) struct Predictor {
    dc: [i32; 3],
}

fn load<L: Layout>(pixels: &[u32], stride: usize, at: Place) -> [u32; AREA] {
    let mut block = [0u32; AREA];
    for row in 0..BLOCK {
        let y = at.y + row.min(at.height - 1);
        let line = &pixels[y * stride + at.x..y * stride + at.x + at.width];
        for column in 0..BLOCK {
            block[row * BLOCK + column] = L::unpack(line[column.min(at.width - 1)]);
        }
    }
    block
}

fn store<L: Layout>(pixels: &mut [u32], stride: usize, at: Place, block: &[u32; AREA]) {
    for row in 0..at.height {
        let line =
            &mut pixels[(at.y + row) * stride + at.x..(at.y + row) * stride + at.x + at.width];
        for (column, pixel) in line.iter_mut().enumerate() {
            *pixel = L::pack(block[row * BLOCK + column]);
        }
    }
}

fn split(block: &[u32; AREA]) -> [[i32; 64]; 6] {
    let mut planes = [[0i32; 64]; 6];
    let mut blue = [0i32; AREA];
    let mut red = [0i32; AREA];
    for (index, &pixel) in block.iter().enumerate() {
        let (r, g, b) = (
            ((pixel >> 16) & 0xff) as i32,
            ((pixel >> 8) & 0xff) as i32,
            (pixel & 0xff) as i32,
        );
        let (row, column) = (index / BLOCK, index % BLOCK);
        let quadrant = (row / 8) * 2 + column / 8;
        planes[quadrant][(row % 8) * 8 + column % 8] = ((r + 2 * g + b) >> 2) - 128;
        blue[index] = b - g;
        red[index] = r - g;
    }
    for row in 0..8 {
        for column in 0..8 {
            let at = row * 2 * BLOCK + column * 2;
            let sum = |plane: &[i32; AREA]| {
                (plane[at] + plane[at + 1] + plane[at + BLOCK] + plane[at + BLOCK + 1] + 2) >> 2
            };
            planes[4][row * 8 + column] = sum(&blue);
            planes[5][row * 8 + column] = sum(&red);
        }
    }
    planes
}

fn join(planes: &[[i32; 64]; 6]) -> [u32; AREA] {
    let mut block = [0u32; AREA];
    for (index, pixel) in block.iter_mut().enumerate() {
        let (row, column) = (index / BLOCK, index % BLOCK);
        let quadrant = (row / 8) * 2 + column / 8;
        let luma = planes[quadrant][(row % 8) * 8 + column % 8] + 128;
        let shared = (row / 2) * 8 + column / 2;
        let (blue, red) = (planes[4][shared], planes[5][shared]);
        let g = luma - ((blue + red) >> 2);
        let (r, b) = (red + g, blue + g);
        *pixel =
            (r.clamp(0, 255) as u32) << 16 | (g.clamp(0, 255) as u32) << 8 | b.clamp(0, 255) as u32;
    }
    block
}

fn rebuild(
    levels: &mut [[i32; 64]; 6],
    tables: &Tables,
    base: Option<&[[i32; 64]; 6]>,
) -> [u32; AREA] {
    for (index, plane) in levels.iter_mut().enumerate() {
        let steps = if index < 4 {
            &tables.luma
        } else {
            &tables.chroma
        };
        for (value, step) in plane.iter_mut().zip(steps) {
            *value = (*value * step).clamp(-2047, 2047);
        }
        dct::inverse(plane);
        if let Some(base) = base {
            for (value, old) in plane.iter_mut().zip(&base[index]) {
                *value += old;
            }
        }
    }
    join(levels)
}

fn spread(planes: &[[i32; 64]; 6], base: Option<&[[i32; 64]; 6]>) -> u32 {
    let mut total = 0u32;
    for (index, plane) in planes.iter().take(4).enumerate() {
        match base {
            Some(base) => {
                for (value, old) in plane.iter().zip(&base[index]) {
                    total += (value - old).unsigned_abs();
                }
            }
            None => {
                let mean = plane.iter().sum::<i32>() / 64;
                for value in plane {
                    total += (value - mean).unsigned_abs();
                }
            }
        }
    }
    total
}

pub(crate) fn encode_lossy(
    writer: &mut BitWriter,
    tables: &Tables,
    predictor: &mut Predictor,
    source: &[u32],
    target: &mut [u32],
    stride: usize,
    at: Place,
) {
    let mut planes = split(&load::<Native>(source, stride, at));
    let base = split(&load::<Native>(target, stride, at));
    let inter = spread(&planes, Some(&base)) < spread(&planes, None);
    writer.put(u32::from(inter), 1);
    if inter {
        for (plane, old) in planes.iter_mut().zip(&base) {
            for (value, old) in plane.iter_mut().zip(old) {
                *value -= old;
            }
        }
    }
    for (index, plane) in planes.iter_mut().enumerate() {
        let steps = if index < 4 {
            &tables.luma
        } else {
            &tables.chroma
        };
        dct::forward(plane);
        for (value, step) in plane.iter_mut().zip(steps) {
            *value = dct::quantise(*value, *step).clamp(-(MAX_LEVEL as i32), MAX_LEVEL as i32);
        }
        let slot = index.saturating_sub(3);
        if inter {
            writer.signed(plane[0]);
        } else {
            writer.signed(plane[0] - predictor.dc[slot]);
            predictor.dc[slot] = plane[0];
        }
        let mut run = 0u32;
        for &position in &ZIGZAG[1..] {
            let level = plane[usize::from(position)];
            if level == 0 {
                run += 1;
                continue;
            }
            writer.unsigned(run + 1);
            writer.unsigned(level.unsigned_abs() - 1);
            writer.put(u32::from(level < 0), 1);
            run = 0;
        }
        writer.unsigned(0);
    }
    let block = rebuild(&mut planes, tables, inter.then_some(&base));
    store::<Native>(target, stride, at, &block);
}

pub(crate) fn decode_lossy<L: Layout>(
    reader: &mut BitReader<'_>,
    tables: &Tables,
    predictor: &mut Predictor,
    target: &mut [u32],
    stride: usize,
    at: Place,
) -> Option<()> {
    let inter = reader.get(1)? == 1;
    let base = inter.then(|| split(&load::<L>(target, stride, at)));
    let mut planes = [[0i32; 64]; 6];
    for (index, plane) in planes.iter_mut().enumerate() {
        let slot = index.saturating_sub(3);
        let dc = if inter {
            reader.signed()?
        } else {
            let value = predictor.dc[slot].checked_add(reader.signed()?)?;
            predictor.dc[slot] = value;
            value
        };
        if dc.unsigned_abs() > MAX_LEVEL {
            return None;
        }
        plane[0] = dc;
        let mut next = 1usize;
        loop {
            let run = reader.unsigned()?;
            if run == 0 {
                break;
            }
            next = next.checked_add(run as usize - 1)?;
            if next >= 64 {
                return None;
            }
            let magnitude = reader.unsigned()?.checked_add(1)?;
            if magnitude > MAX_LEVEL {
                return None;
            }
            let negative = reader.get(1)? == 1;
            let level = magnitude as i32;
            plane[usize::from(ZIGZAG[next])] = if negative { -level } else { level };
            next += 1;
        }
    }
    let block = rebuild(&mut planes, tables, base.as_ref());
    store::<L>(target, stride, at, &block);
    Some(())
}

pub(crate) fn encode_exact(
    writer: &mut BitWriter,
    source: &[u32],
    target: &mut [u32],
    stride: usize,
    at: Place,
) {
    let mut total = 0u64;
    for row in 0..at.height {
        let line = (at.y + row) * stride + at.x;
        for column in 0..at.width {
            let (new, old) = (source[line + column], target[line + column]);
            for shift in [16, 8, 0] {
                total += u64::from(
                    (((new >> shift) & 0xff) as i32 - ((old >> shift) & 0xff) as i32)
                        .unsigned_abs(),
                );
            }
        }
    }
    let mean = total * 2 / (at.width * at.height * 3) as u64;
    let shift = (64 - (mean + 1).leading_zeros() - 1).min(MAX_SHIFT);
    writer.put(shift, 3);
    for row in 0..at.height {
        let line = (at.y + row) * stride + at.x;
        for column in 0..at.width {
            let (new, old) = (source[line + column], target[line + column]);
            for channel in [16, 8, 0] {
                let delta = ((new >> channel) & 0xff) as i32 - ((old >> channel) & 0xff) as i32;
                writer.rice(delta, shift);
            }
            target[line + column] = new;
        }
    }
}

pub(crate) fn decode_exact<L: Layout>(
    reader: &mut BitReader<'_>,
    target: &mut [u32],
    stride: usize,
    at: Place,
) -> Option<()> {
    let shift = reader.get(3)?;
    for row in 0..at.height {
        let line = (at.y + row) * stride + at.x;
        for column in 0..at.width {
            let old = L::unpack(target[line + column]);
            let mut new = 0u32;
            for channel in [16, 8, 0] {
                let delta = reader.rice(shift)?;
                if !(-255..=255).contains(&delta) {
                    return None;
                }
                let value = ((old >> channel) & 0xff) as i32 + delta;
                new |= (value as u32 & 0xff) << channel;
            }
            target[line + column] = L::pack(new);
        }
    }
    Some(())
}
