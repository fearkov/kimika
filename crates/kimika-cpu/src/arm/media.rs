//! the media instructions: parallel arithmetic on halfword and byte lanes,
//! saturation, packing, extension, reversal, selection, sums of
//! differences and bit fields. the operations take registers by number for
//! both decoders, then ARM's media space is decoded at the end.

use super::alu::{signed_saturate, unsigned_saturate};
use super::multiply;
use super::shifter::ShiftType;
use crate::bits::{bit, bits};
use crate::{Cpu, Exit};

/// which lanes a parallel instruction works on and how.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lanes {
    Add16,
    /// adds the top halves and subtracts the bottom ones, crossing b's
    Asx,
    /// the other way round
    Sax,
    Sub16,
    Add8,
    Sub8,
}

/// what happens to a lane's result: kept as it is, which sets GE,
/// saturated, or halved.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Flavor {
    Plain,
    Saturating,
    Halving,
}

/// one lane of width bits, and whether it sets its GE bits.
fn lane(a: u32, b: u32, width: u32, subtract: bool, signed: bool, flavor: Flavor) -> (u32, bool) {
    let mask = (1u32 << width) - 1;
    let extend = |value: u32| {
        if signed {
            ((value << (32 - width)) as i32 >> (32 - width)) as i64
        } else {
            (value & mask) as i64
        }
    };
    let wide = if subtract { extend(a) - extend(b) } else { extend(a) + extend(b) };
    // unsigned GE is no borrow for a subtraction and a carry out for an
    // addition, signed GE the result's sign
    let ge = if signed || subtract { wide >= 0 } else { wide > mask as i64 };
    let value = match flavor {
        Flavor::Plain => wide,
        Flavor::Halving => wide >> 1,
        Flavor::Saturating if signed => signed_saturate(wide, width).0 as i32 as i64,
        Flavor::Saturating => unsigned_saturate(wide, width).0 as i64,
    };
    (value as u32 & mask, ge)
}

pub fn parallel(cpu: &mut Cpu, d: usize, n: usize, m: usize, lanes: Lanes, signed: bool, flavor: Flavor) {
    let (a, b) = (cpu.regs[n], cpu.regs[m]);
    let mut ge = 0u8;
    let result = match lanes {
        Lanes::Add8 | Lanes::Sub8 => {
            let mut out = 0;
            for i in 0..4 {
                let (value, set) = lane(a >> (i * 8), b >> (i * 8), 8, lanes == Lanes::Sub8, signed, flavor);
                out |= value << (i * 8);
                ge |= (set as u8) << i;
            }
            out
        }
        _ => {
            // the exchanging forms take b's halves the other way round, and
            // subtract in one lane and add in the other
            let crossed = matches!(lanes, Lanes::Asx | Lanes::Sax);
            let (b_low, b_high) = if crossed { (b >> 16, b & 0xFFFF) } else { (b & 0xFFFF, b >> 16) };
            let (subtract_low, subtract_high) = match lanes {
                Lanes::Add16 => (false, false),
                Lanes::Asx => (true, false),
                Lanes::Sax => (false, true),
                _ => (true, true),
            };
            let (low, low_ge) = lane(a & 0xFFFF, b_low, 16, subtract_low, signed, flavor);
            let (high, high_ge) = lane(a >> 16, b_high, 16, subtract_high, signed, flavor);
            ge = if low_ge { 0b0011 } else { 0 } | if high_ge { 0b1100 } else { 0 };
            low | high << 16
        }
    };
    cpu.regs[d] = result;
    if flavor == Flavor::Plain {
        cpu.cpsr.ge = ge;
    }
}

/// SSAT and USAT to width bits of n shifted left or arithmetically right,
/// setting Q when it had to.
pub fn saturate(cpu: &mut Cpu, d: usize, n: usize, width: u32, shift: ShiftType, amount: u32, unsigned: bool) {
    let value = cpu.regs[n];
    let shifted = match shift {
        // an arithmetic shift of 0 means 32, which leaves only the sign
        ShiftType::Asr if amount == 0 => (value as i32 >> 31) as i64,
        ShiftType::Asr => (value as i32 >> amount) as i64,
        _ => (value << amount) as i32 as i64,
    };
    let (result, saturated) =
        if unsigned { unsigned_saturate(shifted, width) } else { signed_saturate(shifted, width) };
    cpu.regs[d] = result;
    cpu.cpsr.q |= saturated;
}

/// SSAT16 and USAT16, each halfword to width bits.
pub fn saturate16(cpu: &mut Cpu, d: usize, n: usize, width: u32, unsigned: bool) {
    let value = cpu.regs[n];
    let mut out = 0;
    let mut saturated = false;
    for i in 0..2 {
        let lane = (value >> (i * 16)) as i16 as i64;
        let (result, sat) = if unsigned { unsigned_saturate(lane, width) } else { signed_saturate(lane, width) };
        out |= (result & 0xFFFF) << (i * 16);
        saturated |= sat;
    }
    cpu.regs[d] = out;
    cpu.cpsr.q |= saturated;
}

/// the six extensions, byte to halfwords, byte and halfword, signed or not.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Extension {
    SignedBytes16,
    SignedByte,
    SignedHalfword,
    UnsignedBytes16,
    UnsignedByte,
    UnsignedHalfword,
}

/// SXTB and the rest, from m rotated right by rotation bits, added to n
/// for the accumulating forms, lane by lane for the two that make
/// halfwords.
pub fn extend(cpu: &mut Cpu, d: usize, n: Option<usize>, m: usize, rotation: u32, extension: Extension) {
    let rotated = cpu.regs[m].rotate_right(rotation);
    let (value, lanes) = match extension {
        Extension::SignedBytes16 => {
            let low = rotated as u8 as i8 as u16 as u32;
            let high = (rotated >> 16) as u8 as i8 as u16 as u32;
            (low | high << 16, true)
        }
        Extension::SignedByte => (rotated as u8 as i8 as i32 as u32, false),
        Extension::SignedHalfword => (rotated as u16 as i16 as i32 as u32, false),
        Extension::UnsignedBytes16 => (rotated & 0x00FF_00FF, true),
        Extension::UnsignedByte => (rotated & 0xFF, false),
        Extension::UnsignedHalfword => (rotated & 0xFFFF, false),
    };
    cpu.regs[d] = match n {
        None => value,
        Some(n) if lanes => {
            let accumulator = cpu.regs[n];
            let low = (accumulator as u16).wrapping_add(value as u16);
            let high = ((accumulator >> 16) as u16).wrapping_add((value >> 16) as u16);
            low as u32 | (high as u32) << 16
        }
        Some(n) => cpu.regs[n].wrapping_add(value),
    };
}

/// PKHBT, n's bottom half and m shifted left's top, and PKHTB, n's top
/// half and m shifted right's bottom, where a shift of 0 means 32.
pub fn pack(cpu: &mut Cpu, d: usize, n: usize, m: usize, top_bottom: bool, amount: u32) {
    let (a, b) = (cpu.regs[n], cpu.regs[m]);
    cpu.regs[d] = if top_bottom {
        let low = if amount == 0 { (b as i32 >> 31) as u32 } else { (b as i32 >> amount) as u32 };
        (low & 0xFFFF) | (a & 0xFFFF_0000)
    } else {
        (a & 0xFFFF) | ((b << amount) & 0xFFFF_0000)
    };
}

/// SEL, each byte from n where its GE bit is set and from m where not.
pub fn select(cpu: &mut Cpu, d: usize, n: usize, m: usize) {
    let (a, b) = (cpu.regs[n], cpu.regs[m]);
    let mask = (0..4).fold(0u32, |mask, i| if cpu.cpsr.ge & (1 << i) != 0 { mask | 0xFF << (i * 8) } else { mask });
    cpu.regs[d] = (a & mask) | (b & !mask);
}

/// USAD8 and USADA8, the sum of the bytes' absolute differences.
pub fn sum_of_differences(cpu: &mut Cpu, d: usize, n: usize, m: usize, a: Option<usize>) {
    let (x, y) = (cpu.regs[n], cpu.regs[m]);
    let mut sum = a.map_or(0, |a| cpu.regs[a]);
    for i in 0..4 {
        let (p, q) = ((x >> (i * 8)) & 0xFF, (y >> (i * 8)) & 0xFF);
        sum = sum.wrapping_add(p.abs_diff(q));
    }
    cpu.regs[d] = sum;
}

/// SBFX and UBFX, width bits of n from lsb, extended.
pub fn extract(cpu: &mut Cpu, d: usize, n: usize, lsb: u32, width: u32, signed: bool) {
    let field = cpu.regs[n] >> lsb;
    let shift = 32 - width;
    cpu.regs[d] = if signed { ((field << shift) as i32 >> shift) as u32 } else { (field << shift) >> shift };
}

/// BFI, n's bottom bits into d from lsb to msb, and BFC, zeros there.
/// none when msb comes before lsb, which is unpredictable.
pub fn insert(cpu: &mut Cpu, d: usize, n: Option<usize>, lsb: u32, msb: u32) -> Option<()> {
    if msb < lsb {
        return None;
    }
    let mask = (u32::MAX >> (31 - msb)) & (u32::MAX << lsb);
    let value = n.map_or(0, |n| cpu.regs[n] << lsb);
    cpu.regs[d] = (cpu.regs[d] & !mask) | (value & mask);
    Some(())
}

/// REV, REV16, REVSH and RBIT.
pub fn reverse(value: u32, kind: u32) -> u32 {
    match kind {
        0 => value.swap_bytes(),
        1 => ((value & 0x00FF_00FF) << 8) | ((value & 0xFF00_FF00) >> 8),
        2 => (value as u16).swap_bytes() as i16 as i32 as u32,
        _ => value.reverse_bits(),
    }
}

// ARM's media space, bits 27:25 = 011 and bit 4 set.

pub fn execute(cpu: &mut Cpu, op: u32) -> Option<Exit> {
    let op1 = bits(op, 20, 24);
    let op2 = bits(op, 5, 7);
    let d = bits(op, 12, 15) as usize;
    let n = bits(op, 16, 19) as usize;
    let m = (op & 0xF) as usize;
    match op1 >> 3 {
        0b00 => {
            let signed = op1 & 0b100 == 0;
            let flavor = match op1 & 0b11 {
                0b01 => Flavor::Plain,
                0b10 => Flavor::Saturating,
                0b11 => Flavor::Halving,
                _ => return cpu.undefined(op),
            };
            let lanes = match op2 {
                0b000 => Lanes::Add16,
                0b001 => Lanes::Asx,
                0b010 => Lanes::Sax,
                0b011 => Lanes::Sub16,
                0b100 => Lanes::Add8,
                0b111 => Lanes::Sub8,
                _ => return cpu.undefined(op),
            };
            parallel(cpu, d, n, m, lanes, signed, flavor);
        }
        0b01 => return pack_saturate_extend(cpu, op, op1, op2),
        0b10 => return signed_multiply(cpu, op, op1, op2),
        _ => {
            let lsb = bits(op, 7, 11);
            let msb = bits(op, 16, 20);
            match (op1, op2) {
                (0b11000, 0b000) => {
                    // USAD8 and USADA8 put Rd at 19:16 and Ra at 15:12
                    let a = (d != 15).then_some(d);
                    sum_of_differences(cpu, n, m, bits(op, 8, 11) as usize, a);
                }
                (0b11010 | 0b11011, 0b010 | 0b110) | (0b11110 | 0b11111, 0b010 | 0b110) => {
                    let width = msb + 1;
                    if lsb + width > 32 {
                        return cpu.undefined(op);
                    }
                    extract(cpu, d, m, lsb, width, op1 >> 2 == 0b110);
                }
                (0b11100 | 0b11101, 0b000 | 0b100) => {
                    let source = (m != 15).then_some(m);
                    if insert(cpu, d, source, lsb, msb).is_none() {
                        return cpu.undefined(op);
                    }
                }
                _ => return cpu.undefined(op),
            }
        }
    }
    None
}

fn pack_saturate_extend(cpu: &mut Cpu, op: u32, op1: u32, op2: u32) -> Option<Exit> {
    let d = bits(op, 12, 15) as usize;
    let n = bits(op, 16, 19) as usize;
    let m = (op & 0xF) as usize;
    let rotation = bits(op, 10, 11) * 8;
    let accumulate = (n != 15).then_some(n);
    match (op1, op2) {
        (0b01000, 0b000 | 0b010 | 0b100 | 0b110) => pack(cpu, d, n, m, bit(op, 6), bits(op, 7, 11)),
        (0b01000, 0b011) => extend(cpu, d, accumulate, m, rotation, Extension::SignedBytes16),
        (0b01000, 0b101) => select(cpu, d, n, m),
        (0b01010, 0b001) => saturate16(cpu, d, m, bits(op, 16, 19) + 1, false),
        (0b01010, 0b011) => extend(cpu, d, accumulate, m, rotation, Extension::SignedByte),
        (0b01011, 0b001) => cpu.regs[d] = reverse(cpu.regs[m], 0),
        (0b01011, 0b011) => extend(cpu, d, accumulate, m, rotation, Extension::SignedHalfword),
        (0b01011, 0b101) => cpu.regs[d] = reverse(cpu.regs[m], 1),
        (0b01100, 0b011) => extend(cpu, d, accumulate, m, rotation, Extension::UnsignedBytes16),
        (0b01110, 0b001) => saturate16(cpu, d, m, bits(op, 16, 19), true),
        (0b01110, 0b011) => extend(cpu, d, accumulate, m, rotation, Extension::UnsignedByte),
        (0b01111, 0b001) => cpu.regs[d] = reverse(cpu.regs[m], 3),
        (0b01111, 0b011) => extend(cpu, d, accumulate, m, rotation, Extension::UnsignedHalfword),
        (0b01111, 0b101) => cpu.regs[d] = reverse(cpu.regs[m], 2),
        // SSAT is 01x1x and USAT 11x1x in op1, with op2 xx0
        (0b01010 | 0b01011 | 0b01110 | 0b01111, o) if o & 1 == 0 => {
            let unsigned = op1 & 0b100 != 0;
            let width = bits(op, 16, 20) + if unsigned { 0 } else { 1 };
            let shift = if bit(op, 6) { ShiftType::Asr } else { ShiftType::Lsl };
            saturate(cpu, d, m, width, shift, bits(op, 7, 11), unsigned);
        }
        _ => return cpu.undefined(op),
    }
    None
}

fn signed_multiply(cpu: &mut Cpu, op: u32, op1: u32, op2: u32) -> Option<Exit> {
    // Rd at 19:16, Ra at 15:12 (15 for none), Rm at 11:8, Rn at 3:0
    let d = bits(op, 16, 19) as usize;
    let a = bits(op, 12, 15) as usize;
    let m = bits(op, 8, 11) as usize;
    let n = (op & 0xF) as usize;
    let accumulate = (a != 15).then_some(a);
    let swap = bit(op, 5);
    let subtract = bit(op, 6);
    match (op1, op2 & 0b110) {
        (0b10000, 0b000 | 0b010) => multiply::dual(cpu, d, n, m, accumulate, swap, subtract),
        (0b10100, 0b000 | 0b010) => multiply::dual_long(cpu, a, d, n, m, swap, subtract),
        (0b10101, 0b000) => multiply::most_significant(cpu, d, n, m, accumulate, false, swap),
        (0b10101, 0b110) => multiply::most_significant(cpu, d, n, m, Some(a), true, swap),
        // SDIV and UDIV came after the Cortex-A9
        _ => return cpu.undefined(op),
    }
    None
}
