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
    vector: (i32, i32),
}

const SEARCH_STEPS: usize = 8;
const VECTOR_COST: u32 = 6;
const MAX_VECTOR: i32 = 8192;
const SHARP_ENOUGH: u32 = 2 * AREA as u32;

fn shifted(at: Place, vector: (i32, i32), width: usize, height: usize) -> Option<Place> {
    let x = i64::try_from(at.x).ok()? + i64::from(vector.0);
    let y = i64::try_from(at.y).ok()? + i64::from(vector.1);
    if x < 0 || y < 0 {
        return None;
    }
    let (x, y) = (usize::try_from(x).ok()?, usize::try_from(y).ok()?);
    if x.checked_add(at.width)? > width || y.checked_add(at.height)? > height {
        return None;
    }
    Some(Place { x, y, ..at })
}

#[inline]
fn blend(a: u32, b: u32) -> u32 {
    (a | b) - (((a ^ b) & 0xfefe_fefe) >> 1)
}

fn predict<L: Layout>(
    target: &[u32],
    stride: usize,
    at: Place,
    vector: (i32, i32),
) -> Option<[u32; AREA]> {
    let height = target.len() / stride;
    let (half_x, half_y) = ((vector.0 & 1) as usize, (vector.1 & 1) as usize);
    let reach = Place {
        width: at.width + half_x,
        height: at.height + half_y,
        ..at
    };
    let from = shifted(reach, (vector.0 >> 1, vector.1 >> 1), stride, height)?;
    let mut block = [0u32; AREA];
    let below = half_y * stride;
    for (row, out) in block.as_chunks_mut::<BLOCK>().0.iter_mut().enumerate() {
        let line = (from.y + row.min(at.height - 1)) * stride + from.x;
        let top = &target[line..line + reach.width];
        let bottom = &target[line + below..line + below + reach.width];
        for (column, pixel) in out.iter_mut().enumerate().take(at.width) {
            let mixed = blend(top[column], bottom[column]);
            *pixel = L::unpack(if half_x == 1 {
                blend(mixed, blend(top[column + 1], bottom[column + 1]))
            } else {
                mixed
            });
        }
        let last = out[at.width - 1];
        out[at.width..].fill(last);
    }
    Some(block)
}

fn distance(source: &[u32; AREA], candidate: &[u32; AREA]) -> u32 {
    let mut total = 0u32;
    for (a, b) in source.iter().zip(candidate) {
        total += ((a >> 8) & 0xff).abs_diff((b >> 8) & 0xff);
    }
    total
}

fn whole_distance(
    source: &[u32],
    target: &[u32],
    stride: usize,
    at: Place,
    vector: (i32, i32),
) -> Option<u32> {
    let from = shifted(
        at,
        (vector.0 >> 1, vector.1 >> 1),
        stride,
        target.len() / stride,
    )?;
    let mut total = 0u32;
    for row in 0..at.height {
        let new = (at.y + row) * stride + at.x;
        let old = (from.y + row) * stride + from.x;
        let pairs = source[new..new + at.width]
            .iter()
            .zip(&target[old..old + at.width]);
        for (a, b) in pairs {
            total += ((a >> 8) & 0xff).abs_diff((b >> 8) & 0xff);
        }
    }
    Some(total * (AREA / (at.width * at.height)) as u32)
}

fn search(
    block: &[u32; AREA],
    source: &[u32],
    target: &[u32],
    stride: usize,
    at: Place,
    guess: (i32, i32),
) -> (i32, i32) {
    let bits = |vector: (i32, i32)| {
        let spent = (vector.0 - guess.0).unsigned_abs() + (vector.1 - guess.1).unsigned_abs();
        spent.min(64) * VECTOR_COST
    };
    let reachable = |vector: (i32, i32)| {
        vector.0.unsigned_abs() < MAX_VECTOR as u32 && vector.1.unsigned_abs() < MAX_VECTOR as u32
    };
    let whole = |vector: (i32, i32)| {
        if !reachable(vector) {
            return None;
        }
        Some(whole_distance(source, target, stride, at, vector)? + bits(vector))
    };
    let mut best = ((0, 0), whole((0, 0)).unwrap_or(u32::MAX));
    if best.1 < AREA as u32 {
        return best.0;
    }
    let start = (guess.0 & !1, guess.1 & !1);
    if let Some(value) = whole(start).filter(|value| *value < best.1) {
        best = (start, value);
    }
    for _ in 0..SEARCH_STEPS {
        let centre = best.0;
        for step in [(2, 0), (-2, 0), (0, 2), (0, -2)] {
            let vector = (centre.0 + step.0, centre.1 + step.1);
            if let Some(value) = whole(vector).filter(|value| *value < best.1) {
                best = (vector, value);
            }
        }
        if best.0 == centre {
            break;
        }
    }
    if best.1 < SHARP_ENOUGH {
        return best.0;
    }
    let centre = best.0;
    let mut tried = best;
    for step in [(1, 0), (-1, 0), (0, 1), (0, -1)] {
        let vector = (centre.0 + step.0, centre.1 + step.1);
        let Some(candidate) = predict::<Native>(target, stride, at, vector) else {
            continue;
        };
        let value = distance(block, &candidate) + bits(vector);
        if value < tried.1 {
            tried = (vector, value);
        }
    }
    if tried.0 != centre {
        let side = (tried.0.0 - centre.0, tried.0.1 - centre.1);
        for other in [1, -1] {
            let vector = if side.0 != 0 {
                (tried.0.0, centre.1 + other)
            } else {
                (centre.0 + other, tried.0.1)
            };
            let Some(candidate) = predict::<Native>(target, stride, at, vector) else {
                continue;
            };
            let value = distance(block, &candidate) + bits(vector);
            if value < tried.1 {
                tried = (vector, value);
            }
        }
    }
    best = tried;
    best.0
}

fn put_plane(writer: &mut BitWriter, plane: &[i32; 64], dc: i32) {
    writer.signed(dc);
    let mut run = 0u32;
    for &position in &ZIGZAG[1..] {
        let level = plane[usize::from(position)];
        if level == 0 {
            run += 1;
            continue;
        }
        if run == 0 {
            writer.put(1, 1);
        } else {
            writer.put(0, 2);
            writer.unsigned(run - 1);
        }
        if level.unsigned_abs() == 1 {
            writer.put(1, 1);
        } else {
            writer.put(0, 1);
            writer.unsigned(level.unsigned_abs() - 2);
        }
        writer.put(u32::from(level < 0), 1);
        run = 0;
    }
    writer.put(1, 2);
}

fn get_plane(reader: &mut BitReader<'_>, plane: &mut [i32; 64]) -> Option<i32> {
    let dc = reader.signed()?;
    let mut next = 1usize;
    loop {
        let run = if reader.get(1)? == 1 {
            0
        } else if reader.get(1)? == 1 {
            break;
        } else {
            reader.unsigned()?.checked_add(1)?
        };
        next = next.checked_add(run as usize)?;
        if next >= 64 {
            return None;
        }
        let magnitude = if reader.get(1)? == 1 {
            1
        } else {
            reader.unsigned()?.checked_add(2)?
        };
        if magnitude > MAX_LEVEL {
            return None;
        }
        let level = magnitude as i32;
        plane[usize::from(ZIGZAG[next])] = if reader.get(1)? == 1 { -level } else { level };
        next += 1;
    }
    Some(dc)
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
) -> bool {
    let block = load::<Native>(source, stride, at);
    let mut planes = split(&block);
    let vector = search(&block, source, target, stride, at, predictor.vector);
    let base = split(&predict::<Native>(target, stride, at, vector).unwrap_or(block));
    let inter = spread(&planes, Some(&base)) < spread(&planes, None);
    writer.put(u32::from(inter), 1);
    if inter {
        writer.signed(vector.0 - predictor.vector.0);
        writer.signed(vector.1 - predictor.vector.1);
        predictor.vector = vector;
        for (plane, old) in planes.iter_mut().zip(&base) {
            for (value, old) in plane.iter_mut().zip(old) {
                *value -= old;
            }
        }
    }
    let mut pattern = 0u32;
    let mut first = [0i32; 6];
    for (index, plane) in planes.iter_mut().enumerate() {
        let steps = if index < 4 {
            &tables.luma
        } else {
            &tables.chroma
        };
        dct::forward(plane);
        for (value, step) in plane.iter_mut().zip(steps) {
            *value =
                dct::quantise(*value, *step, inter).clamp(-(MAX_LEVEL as i32), MAX_LEVEL as i32);
        }
        let slot = index.saturating_sub(3);
        first[index] = if inter {
            plane[0]
        } else {
            plane[0] - predictor.dc[slot]
        };
        if first[index] != 0 || plane[1..].iter().any(|level| *level != 0) {
            pattern |= 1 << index;
            if !inter {
                predictor.dc[slot] = plane[0];
            }
        }
    }
    writer.put(pattern, 6);
    for (index, plane) in planes.iter().enumerate() {
        if pattern & (1 << index) != 0 {
            put_plane(writer, plane, first[index]);
        }
    }
    let block = rebuild(&mut planes, tables, inter.then_some(&base));
    store::<Native>(target, stride, at, &block);
    inter && vector != (0, 0)
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
    let base = if inter {
        let vector = (
            predictor.vector.0.checked_add(reader.signed()?)?,
            predictor.vector.1.checked_add(reader.signed()?)?,
        );
        if vector.0.unsigned_abs() > MAX_VECTOR as u32
            || vector.1.unsigned_abs() > MAX_VECTOR as u32
        {
            return None;
        }
        predictor.vector = vector;
        Some(split(&predict::<L>(target, stride, at, vector)?))
    } else {
        None
    };
    let pattern = reader.get(6)?;
    let mut planes = [[0i32; 64]; 6];
    for (index, plane) in planes.iter_mut().enumerate() {
        let slot = index.saturating_sub(3);
        if pattern & (1 << index) == 0 {
            if !inter {
                plane[0] = predictor.dc[slot];
            }
            continue;
        }
        let first = get_plane(reader, plane)?;
        let dc = if inter {
            first
        } else {
            let value = predictor.dc[slot].checked_add(first)?;
            predictor.dc[slot] = value;
            value
        };
        if dc.unsigned_abs() > MAX_LEVEL {
            return None;
        }
        plane[0] = dc;
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
