const CONST_BITS: i32 = 13;
const PASS_BITS: i32 = 2;

const C0_298: i32 = 2446;
const C0_390: i32 = 3196;
const C0_541: i32 = 4433;
const C0_765: i32 = 6270;
const C0_899: i32 = 7373;
const C1_175: i32 = 9633;
const C1_501: i32 = 12299;
const C1_847: i32 = 15137;
const C1_961: i32 = 16069;
const C2_053: i32 = 16819;
const C2_562: i32 = 20995;
const C3_072: i32 = 25172;

pub(crate) const ZIGZAG: [u8; 64] = [
    0, 1, 8, 16, 9, 2, 3, 10, 17, 24, 32, 25, 18, 11, 4, 5, 12, 19, 26, 33, 40, 48, 41, 34, 27, 20,
    13, 6, 7, 14, 21, 28, 35, 42, 49, 56, 57, 50, 43, 36, 29, 22, 15, 23, 30, 37, 44, 51, 58, 59,
    52, 45, 38, 31, 39, 46, 53, 60, 61, 54, 47, 55, 62, 63,
];

pub(crate) const QUALITIES: usize = 12;
const SCALE: [i32; QUALITIES] = [6, 8, 11, 16, 22, 32, 45, 64, 90, 128, 180, 256];

#[inline]
fn descale(value: i32, bits: i32) -> i32 {
    (value + (1 << (bits - 1))) >> bits
}

pub(crate) fn steps(quality: usize, chroma: bool) -> [i32; 64] {
    let scale = SCALE[quality.min(QUALITIES - 1)];
    let mut table = [1i32; 64];
    for v in 0..8 {
        for u in 0..8 {
            let (u32_, v32) = (u as i32, v as i32);
            let base = if chroma {
                14 + 8 * (u32_ + v32) + u32_ * v32
            } else {
                10 + 5 * (u32_ + v32) + u32_ * v32
            };
            table[v * 8 + u] = (base * scale / 32).max(1);
        }
    }
    table
}

fn forward_pass(data: &mut [i32; 64], stride: usize, step: usize, first: bool) {
    for line in 0..8 {
        let at = |index: usize| line * step + index * stride;
        let (d0, d1, d2, d3) = (data[at(0)], data[at(1)], data[at(2)], data[at(3)]);
        let (d4, d5, d6, d7) = (data[at(4)], data[at(5)], data[at(6)], data[at(7)]);
        let (t0, t7, t1, t6) = (d0 + d7, d0 - d7, d1 + d6, d1 - d6);
        let (t2, t5, t3, t4) = (d2 + d5, d2 - d5, d3 + d4, d3 - d4);
        let (t10, t13, t11, t12) = (t0 + t3, t0 - t3, t1 + t2, t1 - t2);
        let even = if first {
            CONST_BITS - PASS_BITS
        } else {
            CONST_BITS + PASS_BITS
        };
        if first {
            data[at(0)] = (t10 + t11) << PASS_BITS;
            data[at(4)] = (t10 - t11) << PASS_BITS;
        } else {
            data[at(0)] = descale(t10 + t11, PASS_BITS);
            data[at(4)] = descale(t10 - t11, PASS_BITS);
        }
        let z1 = (t12 + t13) * C0_541;
        data[at(2)] = descale(z1 + t13 * C0_765, even);
        data[at(6)] = descale(z1 - t12 * C1_847, even);
        let (z1, z2, z3, z4) = (t4 + t7, t5 + t6, t4 + t6, t5 + t7);
        let z5 = (z3 + z4) * C1_175;
        let (t4, t5, t6, t7) = (t4 * C0_298, t5 * C2_053, t6 * C3_072, t7 * C1_501);
        let (z1, z2) = (-z1 * C0_899, -z2 * C2_562);
        let (z3, z4) = (-z3 * C1_961 + z5, -z4 * C0_390 + z5);
        data[at(7)] = descale(t4 + z1 + z3, even);
        data[at(5)] = descale(t5 + z2 + z4, even);
        data[at(3)] = descale(t6 + z2 + z3, even);
        data[at(1)] = descale(t7 + z1 + z4, even);
    }
}

pub(crate) fn forward(data: &mut [i32; 64]) {
    forward_pass(data, 1, 8, true);
    forward_pass(data, 8, 1, false);
}

fn inverse_pass(data: &mut [i32; 64], stride: usize, step: usize, first: bool) {
    let shift = if first {
        CONST_BITS - PASS_BITS
    } else {
        CONST_BITS + PASS_BITS + 3
    };
    for line in 0..8 {
        let at = |index: usize| line * step + index * stride;
        let (z2, z3) = (data[at(2)], data[at(6)]);
        let z1 = (z2 + z3) * C0_541;
        let t2 = z1 - z3 * C1_847;
        let t3 = z1 + z2 * C0_765;
        let (z2, z3) = (data[at(0)], data[at(4)]);
        let t0 = (z2 + z3) << CONST_BITS;
        let t1 = (z2 - z3) << CONST_BITS;
        let (t10, t13, t11, t12) = (t0 + t3, t0 - t3, t1 + t2, t1 - t2);
        let (t0, t1, t2, t3) = (data[at(7)], data[at(5)], data[at(3)], data[at(1)]);
        let (z1, z2, z3, z4) = (t0 + t3, t1 + t2, t0 + t2, t1 + t3);
        let z5 = (z3 + z4) * C1_175;
        let (t0, t1, t2, t3) = (t0 * C0_298, t1 * C2_053, t2 * C3_072, t3 * C1_501);
        let (z1, z2) = (-z1 * C0_899, -z2 * C2_562);
        let (z3, z4) = (-z3 * C1_961 + z5, -z4 * C0_390 + z5);
        let (t0, t1, t2, t3) = (t0 + z1 + z3, t1 + z2 + z4, t2 + z2 + z3, t3 + z1 + z4);
        data[at(0)] = descale(t10 + t3, shift);
        data[at(7)] = descale(t10 - t3, shift);
        data[at(1)] = descale(t11 + t2, shift);
        data[at(6)] = descale(t11 - t2, shift);
        data[at(2)] = descale(t12 + t1, shift);
        data[at(5)] = descale(t12 - t1, shift);
        data[at(3)] = descale(t13 + t0, shift);
        data[at(4)] = descale(t13 - t0, shift);
    }
}

pub(crate) fn inverse(data: &mut [i32; 64]) {
    inverse_pass(data, 8, 1, true);
    for value in data.iter_mut() {
        *value = (*value).clamp(-16383, 16383);
    }
    inverse_pass(data, 1, 8, false);
}

#[inline]
pub(crate) fn quantise(value: i32, step: i32, coarse: bool) -> i32 {
    let divisor = step * 8;
    let rounding = if coarse { divisor / 3 } else { divisor / 2 };
    let magnitude = (value.abs() + rounding) / divisor;
    if value < 0 { -magnitude } else { magnitude }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn noise(seed: &mut u32) -> i32 {
        *seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        (*seed >> 24) as i32 - 128
    }

    #[test]
    fn the_transform_inverts_itself() {
        let mut seed = 7;
        for round in 0..200 {
            let mut block = [0i32; 64];
            for (index, value) in block.iter_mut().enumerate() {
                *value = match round % 4 {
                    0 => noise(&mut seed),
                    1 => (index as i32 % 8) * 30 - 105,
                    2 => 100,
                    _ => noise(&mut seed) / 8 + (index as i32 / 8) * 20 - 70,
                };
            }
            let original = block;
            forward(&mut block);
            for value in &mut block {
                *value = quantise(*value, 1, false);
            }
            inverse(&mut block);
            for (after, before) in block.iter().zip(&original) {
                assert!(
                    (after - before).abs() <= 2,
                    "{after} vs {before} in round {round}"
                );
            }
        }
    }

    #[test]
    fn a_flat_block_has_only_a_dc_term() {
        let mut block = [37i32; 64];
        forward(&mut block);
        assert_eq!(block[0], 37 * 64);
        assert!(block[1..].iter().all(|value| *value == 0));
    }

    #[test]
    fn coarser_qualities_use_larger_steps() {
        for chroma in [false, true] {
            for quality in 1..QUALITIES {
                let (fine, coarse) = (steps(quality - 1, chroma), steps(quality, chroma));
                assert!(coarse.iter().zip(&fine).all(|(a, b)| a >= b));
            }
        }
    }
}
