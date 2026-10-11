//! the PlayStation Vita's GPU, drawn in software: its shader programs, read
//! from their GXP binaries and run by an interpreter of the USSE
//! instruction set, its textures in every layout and format, and a
//! rasterizer that draws into color, depth and stencil surfaces the way
//! the hardware's fixed functions do.
//!
//! it reaches the game's memory through [Bus], so it knows nothing of the
//! rest of the emulator.

pub mod blend;
pub mod convert;
pub mod raster;
pub mod state;

/// the game's memory, as the GPU reads and writes it.
pub trait Bus {
    /// fills buffer from address on, false where memory is missing.
    fn read(&self, address: u32, buffer: &mut [u8]) -> bool;
    /// writes data from address on, false where memory is missing.
    fn write(&self, address: u32, data: &[u8]) -> bool;

    fn read8(&self, address: u32) -> u8 {
        let mut bytes = [0; 1];
        self.read(address, &mut bytes);
        bytes[0]
    }

    fn read16(&self, address: u32) -> u16 {
        let mut bytes = [0; 2];
        self.read(address, &mut bytes);
        u16::from_le_bytes(bytes)
    }

    fn read32(&self, address: u32) -> u32 {
        let mut bytes = [0; 4];
        self.read(address, &mut bytes);
        u32::from_le_bytes(bytes)
    }

    fn write32(&self, address: u32, value: u32) {
        self.write(address, &value.to_le_bytes());
    }
}

/// a plain buffer as memory, from base on, for tests.
pub struct Flat {
    pub base: u32,
    pub bytes: std::cell::RefCell<Vec<u8>>,
}

impl Flat {
    pub fn new(base: u32, size: usize) -> Flat {
        Flat { base, bytes: std::cell::RefCell::new(vec![0; size]) }
    }

    fn range(&self, address: u32, length: usize) -> Option<std::ops::Range<usize>> {
        let start = address.checked_sub(self.base)? as usize;
        let end = start.checked_add(length)?;
        (end <= self.bytes.borrow().len()).then_some(start..end)
    }
}

impl Bus for Flat {
    fn read(&self, address: u32, buffer: &mut [u8]) -> bool {
        match self.range(address, buffer.len()) {
            Some(range) => {
                buffer.copy_from_slice(&self.bytes.borrow()[range]);
                true
            }
            None => {
                buffer.fill(0);
                false
            }
        }
    }

    fn write(&self, address: u32, data: &[u8]) -> bool {
        match self.range(address, data.len()) {
            Some(range) => {
                self.bytes.borrow_mut()[range].copy_from_slice(data);
                true
            }
            None => false,
        }
    }
}
