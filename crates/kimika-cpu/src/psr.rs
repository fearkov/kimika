//! the program status register as user mode sees it, the APSR, plus the
//! execution state bits that matter to it: Thumb and the IT block.

use crate::bits::bit;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Psr {
    pub n: bool,
    pub z: bool,
    pub c: bool,
    pub v: bool,
    /// sticky saturation flag, set by the DSP and saturating arithmetic.
    pub q: bool,
    /// the media instructions' greater than or equal flags, one per byte
    /// lane.
    pub ge: u8,
    pub thumb: bool,
    /// ITSTATE, the condition and mask of the IT block the next Thumb
    /// instructions are in, zero outside one.
    pub it: u8,
}

impl Psr {
    /// the CPSR as MRS reads it in user mode, which always says User.
    pub fn to_bits(self) -> u32 {
        (self.n as u32) << 31
            | (self.z as u32) << 30
            | (self.c as u32) << 29
            | (self.v as u32) << 28
            | (self.q as u32) << 27
            | (self.ge as u32 & 0xF) << 16
            | (self.thumb as u32) << 5
            | 0b10000
    }

    /// what MSR may write from user mode: the flags with mask bit 3, the GE
    /// bits with mask bit 2.
    pub fn write_masked(&mut self, value: u32, mask: u32) {
        if mask & 0b1000 != 0 {
            self.n = bit(value, 31);
            self.z = bit(value, 30);
            self.c = bit(value, 29);
            self.v = bit(value, 28);
            self.q = bit(value, 27);
        }
        if mask & 0b0100 != 0 {
            self.ge = ((value >> 16) & 0xF) as u8;
        }
    }

    /// whether an IT block is underway.
    #[inline(always)]
    pub fn in_it_block(&self) -> bool {
        self.it & 0xF != 0
    }

    /// the condition the IT block puts on the next instruction.
    #[inline(always)]
    pub fn it_condition(&self) -> u32 {
        (self.it >> 4) as u32
    }

    /// moves the IT block on by one instruction, ending it after the last.
    #[inline(always)]
    pub fn advance_it(&mut self) {
        if self.it & 0x7 == 0 {
            self.it = 0;
        } else {
            self.it = (self.it & 0xE0) | ((self.it << 1) & 0x1F);
        }
    }
}

#[inline(always)]
pub fn check_condition(cond: u32, psr: &Psr) -> bool {
    match cond & 0xF {
        0x0 => psr.z,
        0x1 => !psr.z,
        0x2 => psr.c,
        0x3 => !psr.c,
        0x4 => psr.n,
        0x5 => !psr.n,
        0x6 => psr.v,
        0x7 => !psr.v,
        0x8 => psr.c && !psr.z,
        0x9 => !psr.c || psr.z,
        0xA => psr.n == psr.v,
        0xB => psr.n != psr.v,
        0xC => !psr.z && (psr.n == psr.v),
        0xD => psr.z || (psr.n != psr.v),
        _ => true,
    }
}
