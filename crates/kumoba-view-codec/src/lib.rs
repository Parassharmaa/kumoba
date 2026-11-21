#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss
)]

mod bits;
mod blocks;
mod dct;
mod decode;
mod encode;
#[cfg(test)]
mod frames;
mod motion;
mod stream;

pub use decode::{DecodeError, Decoder};
pub use encode::{Budget, Encoder, FrameStats};

pub const BLOCK: usize = 16;
pub const BAND: usize = 64;
pub const MAX_WIDTH: usize = 4096;
pub const MAX_HEIGHT: usize = 4096;

pub(crate) const TAG_INDEX: u8 = 0x00;
pub(crate) const TAG_DIFF: u8 = 0x40;
pub(crate) const TAG_LUMA: u8 = 0x80;
pub(crate) const TAG_RUN: u8 = 0xc0;
pub(crate) const TAG_ABOVE: u8 = 0xd0;
pub(crate) const TAG_SKIP: u8 = 0xe0;
pub(crate) const TAG_RGB: u8 = 0xf0;
pub(crate) const RUN_INLINE: usize = 15;

pub(crate) const FLAG_KEY: u8 = 1;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rect {
    pub x: usize,
    pub y: usize,
    pub width: usize,
    pub height: usize,
}

impl Rect {
    pub fn union(self, other: Rect) -> Rect {
        if self.width == 0 || self.height == 0 {
            return other;
        }
        if other.width == 0 || other.height == 0 {
            return self;
        }
        let x = self.x.min(other.x);
        let y = self.y.min(other.y);
        let right = (self.x + self.width).max(other.x + other.width);
        let bottom = (self.y + self.height).max(other.y + other.height);
        Rect {
            x,
            y,
            width: right - x,
            height: bottom - y,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Move {
    pub x: usize,
    pub y: usize,
    pub width: usize,
    pub height: usize,
    pub dx: i32,
    pub dy: i32,
}

#[inline]
pub(crate) fn cache_slot(pixel: u32) -> usize {
    (pixel.wrapping_mul(0x9e37_79b1) >> 26) as usize
}

pub(crate) fn apply_moves(
    pixels: &mut [u32],
    scratch: &mut Vec<u32>,
    width: usize,
    moves: &[Move],
) {
    if moves.is_empty() {
        return;
    }
    scratch.clear();
    scratch.extend_from_slice(pixels);
    for step in moves {
        for row in 0..step.height {
            let to = (step.y + row) * width + step.x;
            let from_y = (step.y + row).wrapping_add_signed(-(step.dy as isize));
            let from_x = step.x.wrapping_add_signed(-(step.dx as isize));
            let from = from_y * width + from_x;
            pixels[to..to + step.width].copy_from_slice(&scratch[from..from + step.width]);
        }
    }
}
