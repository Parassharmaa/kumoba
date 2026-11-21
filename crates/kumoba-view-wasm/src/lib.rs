#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss
)]

use kumoba_view_codec::{Decoder, Rect};
use std::cell::RefCell;

#[derive(Default)]
struct State {
    decoder: Option<Decoder>,
    input: Vec<u8>,
    dirty: [u32; 4],
}

thread_local! {
    static STATE: RefCell<State> = RefCell::new(State::default());
}

#[unsafe(no_mangle)]
pub extern "C" fn kv_open(width: u32, height: u32) -> u32 {
    STATE.with_borrow_mut(|state| {
        state.decoder = Decoder::new(width as usize, height as usize);
        u32::from(state.decoder.is_some())
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn kv_input(length: u32) -> *mut u8 {
    STATE.with_borrow_mut(|state| {
        state.input.clear();
        state.input.resize(length as usize, 0);
        state.input.as_mut_ptr()
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn kv_decode() -> u32 {
    STATE.with_borrow_mut(|state| {
        let State {
            decoder,
            input,
            dirty,
        } = state;
        let Some(decoder) = decoder.as_mut() else {
            return 0;
        };
        match decoder.decode(input) {
            Ok(Rect {
                x,
                y,
                width,
                height,
            }) => {
                *dirty = [x as u32, y as u32, width as u32, height as u32];
                1
            }
            Err(_) => 0,
        }
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn kv_dirty(index: u32) -> u32 {
    STATE.with_borrow(|state| state.dirty.get(index as usize).copied().unwrap_or(0))
}

#[unsafe(no_mangle)]
pub extern "C" fn kv_pixels() -> *const u32 {
    STATE.with_borrow(|state| {
        state
            .decoder
            .as_ref()
            .map_or(std::ptr::null(), |decoder| decoder.pixels().as_ptr())
    })
}
