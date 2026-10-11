//! the screen: the framebuffer the game last set, which a set for the next
//! frame replaces at the vertical blank, and the blanks themselves, which
//! the frontend raises about 60 times a second and threads wait for.
//!
//! the framebuffer is read from guest memory every time the screen is
//! shown, since games draw into it with plain stores after setting it.

use std::sync::{Condvar, Mutex};

use crate::memory::Memory;

/// SCE_DISPLAY_PIXELFORMAT_A8B8G8R8, each pixel a word with red in the
/// low byte, RGBA in memory.
pub const A8B8G8R8: u32 = 0;
/// SCE_DISPLAY_PIXELFORMAT_A2B10G10R10, ten bits a colour.
pub const A2B10G10R10: u32 = 0x6080_0000;

/// a framebuffer as SceDisplayFrameBuf gives it, the pitch in pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameBuffer {
    pub base: u32,
    pub pitch: u32,
    pub format: u32,
    pub width: u32,
    pub height: u32,
}

/// what the screen shows, straight RGBA8, opaque.
#[derive(Debug, Clone)]
pub struct Screen {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>,
}

#[derive(Default)]
struct State {
    shown: Option<FrameBuffer>,
    /// what a set for the next frame asked for, until the blank.
    next: Option<Option<FrameBuffer>>,
    vblanks: u64,
}

#[derive(Default)]
pub struct Display {
    state: Mutex<State>,
    blank: Condvar,
}

impl Display {
    /// shows a framebuffer, or nothing, now or from the next blank.
    pub fn set(&self, frame: Option<FrameBuffer>, next_frame: bool) {
        let mut state = self.state.lock().unwrap();
        if next_frame {
            state.next = Some(frame);
        } else {
            state.shown = frame;
            state.next = None;
        }
    }

    /// the framebuffer last set, shown or about to be.
    pub fn latest(&self) -> Option<FrameBuffer> {
        let state = self.state.lock().unwrap();
        state.next.unwrap_or(state.shown)
    }

    pub fn shown(&self) -> Option<FrameBuffer> {
        self.state.lock().unwrap().shown
    }

    /// a vertical blank: a set for the next frame takes effect, and threads
    /// waiting for a blank go on. the count so far.
    pub fn vblank(&self) -> u64 {
        let mut state = self.state.lock().unwrap();
        if let Some(next) = state.next.take() {
            state.shown = next;
        }
        state.vblanks += 1;
        self.blank.notify_all();
        state.vblanks
    }

    pub fn vblanks(&self) -> u64 {
        self.state.lock().unwrap().vblanks
    }

    /// waits for the next vertical blank, or for the process to end.
    pub fn wait_vblank(&self, stopping: impl Fn() -> bool) {
        let mut state = self.state.lock().unwrap();
        let start = state.vblanks;
        while state.vblanks == start && !stopping() {
            state = self.blank.wait(state).unwrap();
        }
    }

    /// waits until a framebuffer set for the next frame has taken effect.
    pub fn wait_set(&self, stopping: impl Fn() -> bool) {
        let mut state = self.state.lock().unwrap();
        while state.next.is_some() && !stopping() {
            state = self.blank.wait(state).unwrap();
        }
    }

    pub fn wake_everyone(&self) {
        let _state = self.state.lock().unwrap();
        self.blank.notify_all();
    }

    /// what the screen shows now, read out of guest memory.
    pub fn screen(&self, memory: &Memory) -> Option<Screen> {
        let frame = self.shown()?;
        let (width, height) = (frame.width, frame.height);
        let mut pixels = vec![0; (width * height * 4) as usize];
        let mut row = vec![0; (width * 4) as usize];
        for y in 0..height {
            memory.read_bytes(frame.base.wrapping_add(y * frame.pitch * 4), &mut row);
            let out = &mut pixels[(y * width * 4) as usize..][..(width * 4) as usize];
            match frame.format {
                A2B10G10R10 => {
                    for (pixel, word) in out.as_chunks_mut::<4>().0.iter_mut().zip(row.as_chunks::<4>().0) {
                        let word = u32::from_le_bytes(*word);
                        // the top eight of each colour's ten bits, red lowest
                        let channel = |low: u32| (word >> (low + 2)) as u8;
                        pixel.copy_from_slice(&[channel(0), channel(10), channel(20), 255]);
                    }
                }
                _ => {
                    out.copy_from_slice(&row);
                    for pixel in out.as_chunks_mut::<4>().0 {
                        pixel[3] = 255;
                    }
                }
            }
        }
        Some(Screen { width, height, pixels })
    }
}
