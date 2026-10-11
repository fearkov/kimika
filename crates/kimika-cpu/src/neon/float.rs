//! NEON's floating point, single precision lanes under ARM's standard
//! FPSCR: flush to zero, default NaN, round to nearest.

use super::{all, lane, read, registers, set_lane, undefined, write};
use crate::bits::{bit, bits};
use crate::fp::{self, Operation, STANDARD};
use crate::{Cpu, Exit};

const SIGN: u32 = 0x8000_0000;
const TWO: u32 = 0x4000_0000;
const THREE: u32 = 0x4040_0000;

#[inline(always)]
fn input(x: u32) -> f32 {
    fp::input::<f32>(STANDARD, x)
}

pub(super) fn add(a: u32, b: u32) -> u32 {
    fp::arithmetic::<f32>(STANDARD, Operation::Add, a, b)
}

pub(super) fn subtract(a: u32, b: u32) -> u32 {
    fp::arithmetic::<f32>(STANDARD, Operation::Subtract, a, b)
}

pub(super) fn multiply(a: u32, b: u32) -> u32 {
    fp::arithmetic::<f32>(STANDARD, Operation::Multiply, a, b)
}

/// VMLA and VMLS: the product rounded, then added to the accumulator or
/// subtracted from it.
pub(super) fn multiply_add(accumulator: u32, a: u32, b: u32, subtract: bool) -> u32 {
    let product = multiply(a, b);
    add(accumulator, if subtract { product ^ SIGN } else { product })
}

pub(super) fn equal(a: u32, b: u32) -> bool {
    input(a) == input(b)
}

pub(super) fn greater_equal(a: u32, b: u32) -> bool {
    input(a) >= input(b)
}

pub(super) fn greater(a: u32, b: u32) -> bool {
    input(a) > input(b)
}

/// VMAX and VMIN: of two zeros the maximum is +0 if either is, the minimum
/// -0, and a subnormal is a zero.
fn extreme(a: u32, b: u32, maximum: bool) -> u32 {
    if let Some(nan) = fp::nan_operand::<f32>(STANDARD, &[a, b]) {
        return nan;
    }
    let (x, y) = (input(a), input(b));
    let chosen = if (maximum && x > y) || (!maximum && x < y) { x } else { y };
    if chosen == 0.0 {
        if maximum {
            a & b & SIGN
        } else {
            (a | b) & SIGN
        }
    } else {
        chosen.to_bits()
    }
}

/// VRECPS, 2 - a * b, and VRSQRTS, (3 - a * b) / 2, each step rounded. an
/// infinity times a zero is taken as zero.
fn step(a: u32, b: u32, square_root: bool) -> u32 {
    if let Some(nan) = fp::nan_operand::<f32>(STANDARD, &[a, b]) {
        return nan;
    }
    let (x, y) = (input(a), input(b));
    let product = if (x.is_infinite() && y == 0.0) || (x == 0.0 && y.is_infinite()) { 0 } else { multiply(a, b) };
    if square_root {
        fp::arithmetic::<f32>(STANDARD, Operation::Divide, subtract(THREE, product), TWO)
    } else {
        subtract(TWO, product)
    }
}

/// A7.4.1's floating-point half, opc 0b1100 and up, the size's low bit
/// clear for single precision and its high bit choosing between two.
pub(super) fn three_same(cpu: &mut Cpu, op: u32) -> Option<Exit> {
    let (u, size, opc, quad, o1) = (bit(op, 24), bits(op, 20, 21), bits(op, 8, 11), bit(op, 6), bit(op, 4));
    let (d, n, m) = registers(op);
    let high = size & 2 != 0;
    if size & 1 == 1 {
        return undefined(cpu, op);
    }
    // the operation on the accumulator, n's lane and m's
    let operation: fn(u32, u32, u32) -> u32 = match (opc, u, high, o1) {
        (0b1101, false, false, false) => |_, x, y| add(x, y),
        (0b1101, false, true, false) => |_, x, y| subtract(x, y),
        (0b1101, false, false, true) => |z, x, y| multiply_add(z, x, y, false),
        (0b1101, false, true, true) => |z, x, y| multiply_add(z, x, y, true),
        (0b1101, true, false, false) => |_, x, y| add(x, y),
        (0b1101, true, false, true) => |_, x, y| multiply(x, y),
        (0b1101, true, true, false) => |_, x, y| subtract(x, y) & !SIGN,
        (0b1110, false, false, false) => |_, x, y| all(equal(x, y)) as u32,
        (0b1110, true, false, false) => |_, x, y| all(greater_equal(x, y)) as u32,
        (0b1110, true, true, false) => |_, x, y| all(greater(x, y)) as u32,
        (0b1110, true, false, true) => |_, x, y| all(greater_equal(x & !SIGN, y & !SIGN)) as u32,
        (0b1110, true, true, true) => |_, x, y| all(greater(x & !SIGN, y & !SIGN)) as u32,
        (0b1111, _, false, false) => |_, x, y| extreme(x, y, true),
        (0b1111, _, true, false) => |_, x, y| extreme(x, y, false),
        (0b1111, false, false, true) => |_, x, y| step(x, y, false),
        (0b1111, false, true, true) => |_, x, y| step(x, y, true),
        // VFMA and VFMS are VFPv4's, VMAXNM and VMINNM ARMv8's
        _ => return undefined(cpu, op),
    };
    // VPADD, VPMAX and VPMIN work on pairs
    let pairwise = u && !o1 && (opc == 0b1111 || (opc == 0b1101 && !high));
    if pairwise && quad {
        return undefined(cpu, op);
    }
    let regs = if quad { 2 } else { 1 };
    let (a, b) = (read(cpu, n, regs), read(cpu, m, regs));
    let mut result = [0; 4];
    if pairwise {
        for (i, source) in [&a, &b].into_iter().enumerate() {
            let (x, y) = (lane(source, 0, 32) as u32, lane(source, 1, 32) as u32);
            set_lane(&mut result, i, 32, operation(0, x, y) as u64);
        }
    } else {
        let old = read(cpu, d, regs);
        for e in 0..regs * 2 {
            let (x, y, z) = (lane(&a, e, 32) as u32, lane(&b, e, 32) as u32, lane(&old, e, 32) as u32);
            set_lane(&mut result, e, 32, operation(z, x, y) as u64);
        }
    }
    write(cpu, d, &result, regs);
    None
}

/// the comparisons with zero in A7.4.5: greater, greater or equal, equal,
/// less or equal, less.
pub(super) fn compare_zero(x: u32, kind: u32) -> bool {
    match kind {
        0 => greater(x, 0),
        1 => greater_equal(x, 0),
        2 => equal(x, 0),
        3 => greater_equal(0, x),
        _ => greater(0, x),
    }
}

/// ARM's reciprocal estimate of a in [0.5, 1): 1 / a to eight bits, a
/// multiple of 1/256 in [1, 2), from a's top nine bits.
fn reciprocal(a: f64) -> f64 {
    let q = (a * 512.0) as i64;
    let r = 1.0 / ((q as f64 + 0.5) / 512.0);
    let s = (256.0 * r + 0.5) as i64;
    s as f64 / 256.0
}

/// ARM's reciprocal square root estimate of a in [0.25, 1), likewise.
fn reciprocal_root(a: f64) -> f64 {
    let r = if a < 0.5 {
        let q = (a * 512.0) as i64;
        1.0 / ((q as f64 + 0.5) / 512.0).sqrt()
    } else {
        let q = (a * 256.0) as i64;
        1.0 / ((q as f64 + 0.5) / 256.0).sqrt()
    };
    let s = (256.0 * r + 0.5) as i64;
    s as f64 / 256.0
}

/// VRECPE.F32.
pub(super) fn reciprocal_estimate(x: u32) -> u32 {
    if let Some(nan) = fp::nan_operand::<f32>(STANDARD, &[x]) {
        return nan;
    }
    let sign = x & SIGN;
    let exponent = (x >> 23) & 0xFF;
    let value = input(x);
    if value.is_infinite() {
        return sign;
    }
    if value == 0.0 {
        return sign | 0x7F80_0000;
    }
    // past 2^126 the reciprocal is subnormal, flushed
    if exponent >= 253 {
        return sign;
    }
    // the fraction under an exponent making it [0.5, 1)
    let scaled = f64::from_bits(0x3FE0_0000_0000_0000 | ((x & 0x7F_FFFF) as u64) << 29);
    let estimate = reciprocal(scaled).to_bits();
    sign | (253 - exponent) << 23 | ((estimate >> 29) as u32 & 0x7F_FFFF)
}

/// VRSQRTE.F32.
pub(super) fn reciprocal_root_estimate(x: u32) -> u32 {
    if let Some(nan) = fp::nan_operand::<f32>(STANDARD, &[x]) {
        return nan;
    }
    let value = input(x);
    if value == 0.0 {
        return (x & SIGN) | 0x7F80_0000;
    }
    if x & SIGN != 0 {
        return fp::DEFAULT_NAN_32;
    }
    if value.is_infinite() {
        return 0;
    }
    // the fraction made [0.25, 1), keeping whether the exponent was odd
    let exponent = (x >> 23) & 0xFF;
    let scaled_exponent: u64 = if exponent & 1 == 0 { 0x3FE } else { 0x3FD };
    let scaled = f64::from_bits(scaled_exponent << 52 | ((x & 0x7F_FFFF) as u64) << 29);
    let estimate = reciprocal_root(scaled).to_bits();
    ((380 - exponent) / 2) << 23 | ((estimate >> 29) as u32 & 0x7F_FFFF)
}

/// VRECPE.U32, the operand a fixed-point fraction under 1.
pub(super) fn unsigned_reciprocal_estimate(x: u32) -> u32 {
    if x & SIGN == 0 {
        return u32::MAX;
    }
    let scaled = f64::from_bits(0x3FE0_0000_0000_0000 | ((x & 0x7FFF_FFFF) as u64) << 21);
    SIGN | ((reciprocal(scaled).to_bits() >> 21) as u32 & 0x7FFF_FFFF)
}

/// VRSQRTE.U32.
pub(super) fn unsigned_reciprocal_root_estimate(x: u32) -> u32 {
    if x >> 30 == 0 {
        return u32::MAX;
    }
    let scaled = if x & SIGN != 0 {
        0x3FE0_0000_0000_0000 | ((x & 0x7FFF_FFFF) as u64) << 21
    } else {
        0x3FD0_0000_0000_0000 | ((x & 0x3FFF_FFFF) as u64) << 22
    };
    SIGN | ((reciprocal_root(f64::from_bits(scaled)).to_bits() >> 21) as u32 & 0x7FFF_FFFF)
}

/// VCVT from single precision to a fixed-point number with fraction bits,
/// rounding toward zero and saturating.
pub(super) fn to_fixed(x: u32, fraction: u32, unsigned: bool) -> u32 {
    fp::to_fixed(input(x) as f64, fraction, 32, !unsigned)
}

/// VCVT from a fixed-point number to single precision, rounding to nearest.
pub(super) fn from_fixed(x: u32, fraction: u32, unsigned: bool) -> u32 {
    fp::narrow(STANDARD, fp::from_fixed(x, fraction, 32, !unsigned))
}

pub(super) fn to_half(x: u32) -> u16 {
    fp::single_to_half(STANDARD, x)
}

pub(super) fn from_half(x: u16) -> u32 {
    fp::half_to_single(STANDARD, x)
}
