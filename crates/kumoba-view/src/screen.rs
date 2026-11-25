use kumoba_view_codec::Rect;
use std::os::fd::RawFd;

pub type Failure = Box<dyn std::error::Error + Send + Sync>;

pub struct Cursor {
    pub width: u16,
    pub height: u16,
    pub hot_x: u16,
    pub hot_y: u16,
    pub rgba: Vec<u8>,
}

pub trait Screen {
    fn size(&self) -> (usize, usize);
    fn fd(&self) -> RawFd;
    fn pixels(&self) -> &[u32];
    fn drain(&mut self) -> Result<bool, Failure>;
    fn resized(&self) -> bool;
    fn grab(&mut self, full: bool) -> Result<Rect, Failure>;
    fn moves(&mut self) -> Vec<(i32, i32)>;
    fn cursor(&mut self) -> Result<Option<Cursor>, Failure>;
    fn position(&mut self) -> Result<(u16, u16), Failure>;
    fn pointer(&mut self, x: i16, y: i16) -> Result<(), Failure>;
    fn button(&mut self, button: u8, down: bool) -> Result<(), Failure>;
    fn key(&mut self, keycode: u8, down: bool) -> Result<(), Failure>;
    fn symbol(&mut self, symbol: u32, down: bool) -> Result<(), Failure>;
    fn needs_key(&self, symbol: u32) -> bool;
    fn bind(&mut self, symbols: impl Iterator<Item = u32>) -> Result<bool, Failure>;
    fn chord(&mut self, letter: u8, shift: bool) -> Result<(), Failure>;
    fn offer(&mut self, text: &[u8]) -> Result<(), Failure>;
    fn expect_copy(&mut self);
    fn pull(&mut self) -> Result<(), Failure>;
    fn copied(&mut self) -> Option<Vec<u8>>;
    fn chords(&self) -> bool {
        false
    }
}
