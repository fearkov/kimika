//! shifts by an immediate, the conversions to and from fixed point that
//! share their encoding, and the immediates themselves.

use super::{float, int, lane, read, registers, saturate, saturated, set_lane, shift_by, undefined, write};
use crate::bits::{bit, bits};
use crate::{Cpu, Exit};

/// A7.4.4, two registers and a shift amount. L:imm6's top bit gives the lane
/// size, and the rest the shift, counted down from twice the size for the
/// shifts right and up from the size for those left.
pub(super) fn shift(cpu: &mut Cpu, op: u32) -> Option<Exit> {
    let (unsigned, imm6, opc, l, quad) = (bit(op, 24), bits(op, 16, 21), bits(op, 8, 11), bit(op, 7), bit(op, 6));
    let (d, _, m) = registers(op);
    let esize = if l { 64 } else { 1 << (31 - imm6.leading_zeros()) };
    let right = if l { 64 - imm6 } else { 2 * esize - imm6 };
    let left = if l { imm6 } else { imm6 - esize };
    let mut result = [0; 4];
    let mut qc = false;
    match opc {
        // the same size in and out: VSHR, VSRA, VRSHR, VRSRA, VSRI, VSHL,
        // VSLI, VQSHLU and VQSHL
        0b0000..=0b0111 => {
            if (quad && (d | m) & 1 == 1) || (matches!(opc, 0b0100 | 0b0110) && !unsigned) {
                return undefined(cpu, op);
            }
            let regs = if quad { 2 } else { 1 };
            let (a, old) = (read(cpu, m, regs), read(cpu, d, regs));
            let size_mask = super::mask(esize);
            for e in 0..regs * 64 / esize as usize {
                let (x, z) = (lane(&a, e, esize), lane(&old, e, esize));
                let value = int(x, esize, unsigned);
                let shifted = |round| shift_by(value, -(right as i32), esize, round) as u64;
                let lane_value = match opc {
                    0b0000 => shifted(false),
                    0b0001 => z.wrapping_add(shifted(false)),
                    0b0010 => shifted(true),
                    0b0011 => z.wrapping_add(shifted(true)),
                    // VSRI and VSLI keep the bits the shift brings in from d
                    0b0100 if right >= 64 => z,
                    0b0100 => (z & !(size_mask >> right)) | x >> right,
                    0b0101 if !unsigned => x << left,
                    0b0101 => (z & !(size_mask << left)) | x << left,
                    _ => {
                        // VQSHLU takes signed lanes to unsigned ones
                        let source_unsigned = unsigned && opc == 0b0111;
                        let (v, clipped) = saturate(int(x, esize, source_unsigned) << left, esize, unsigned);
                        qc |= clipped;
                        v
                    }
                };
                set_lane(&mut result, e, esize, lane_value);
            }
            write(cpu, d, &result, regs);
        }
        // narrowing, lanes of twice the size in: VSHRN and VRSHRN, VQSHRUN
        // and VQRSHRUN, VQSHRN and VQRSHRN, bit 6 rounding
        0b1000 | 0b1001 => {
            if l || m & 1 == 1 {
                return undefined(cpu, op);
            }
            let (source_unsigned, saturating) = match (opc, unsigned) {
                (0b1000, false) => (false, false),
                (0b1000, true) => (false, true),
                _ => (unsigned, true),
            };
            let a = read(cpu, m, 2);
            let long = 2 * esize;
            for e in 0..64 / esize as usize {
                let value = int(lane(&a, e, long), long, source_unsigned);
                let shifted = shift_by(value, -(right as i32), long, quad);
                let lane_value = if saturating {
                    let (v, clipped) = saturate(shifted, esize, unsigned);
                    qc |= clipped;
                    v
                } else {
                    shifted as u64
                };
                set_lane(&mut result, e, esize, lane_value);
            }
            write(cpu, d, &result, 1);
        }
        // VSHLL, and VMOVL, which shifts by nothing
        0b1010 => {
            if l || quad || d & 1 == 1 {
                return undefined(cpu, op);
            }
            let a = read(cpu, m, 1);
            for e in 0..64 / esize as usize {
                let value = int(lane(&a, e, esize), esize, unsigned) << left;
                set_lane(&mut result, e, 2 * esize, value as u64);
            }
            write(cpu, d, &result, 2);
        }
        // VCVT between single precision and fixed point, bit 8 to fixed
        0b1110 | 0b1111 => {
            if l || imm6 < 32 || (quad && (d | m) & 1 == 1) {
                return undefined(cpu, op);
            }
            let regs = if quad { 2 } else { 1 };
            let fraction = 64 - imm6;
            let a = read(cpu, m, regs);
            for e in 0..regs * 2 {
                let x = lane(&a, e, 32) as u32;
                let value = if bit(op, 8) {
                    float::to_fixed(x, fraction, unsigned)
                } else {
                    float::from_fixed(x, fraction, unsigned)
                };
                set_lane(&mut result, e, 32, value as u64);
            }
            write(cpu, d, &result, regs);
        }
        _ => return undefined(cpu, op),
    }
    saturated(cpu, qc);
    None
}

/// AdvSIMDExpandImm: imm8 placed and repeated by cmode, or with op and
/// cmode 0b1110 each bit made a byte. ARMv7 left a zero imm8 unpredictable
/// where it gives zero anyway, and ARMv8 defined it so.
fn expand(cmode: u32, op: bool, imm8: u32) -> Option<u64> {
    let imm = imm8 as u64;
    let twice = |word: u64| word << 32 | word;
    let four = |half: u64| half << 48 | half << 32 | half << 16 | half;
    Some(match cmode >> 1 {
        0b000 => twice(imm),
        0b001 => twice(imm << 8),
        0b010 => twice(imm << 16),
        0b011 => twice(imm << 24),
        0b100 => four(imm),
        0b101 => four(imm << 8),
        0b110 if cmode & 1 == 0 => twice(imm << 8 | 0xFF),
        0b110 => twice(imm << 16 | 0xFFFF),
        _ => match (cmode & 1, op) {
            (0, false) => super::replicate(imm, 8),
            (0, true) => (0..8).filter(|i| imm >> i & 1 == 1).fold(0, |word, i| word | 0xFF << (8 * i)),
            (_, false) => {
                // a single: the sign, the exponent's top bit the inverse of
                // the next, which repeats five times, and six bits more
                let b = (imm >> 6) & 1;
                let word =
                    (imm >> 7) << 31 | (b ^ 1) << 30 | (if b == 1 { 0x1F } else { 0 }) << 25 | (imm & 0x3F) << 19;
                twice(word)
            }
            _ => return None,
        },
    })
}

/// A7.4.6, one register and a modified immediate: VMOV, VMVN, VORR and VBIC.
pub(super) fn immediate(cpu: &mut Cpu, op: u32) -> Option<Exit> {
    let (cmode, quad, invert) = (bits(op, 8, 11), bit(op, 6), bit(op, 5));
    let imm8 = (bit(op, 24) as u32) << 7 | bits(op, 16, 18) << 4 | (op & 0xF);
    let d = registers(op).0;
    let Some(imm) = expand(cmode, invert, imm8) else {
        return undefined(cpu, op);
    };
    if quad && d & 1 == 1 {
        return undefined(cpu, op);
    }
    // the odd cmodes below 0b1100 are VORR and VBIC, the rest VMOV and VMVN,
    // but for the byte mask, which op makes a VMOV
    let bitwise = cmode < 0b1100 && cmode & 1 == 1;
    for r in 0..if quad { 2 } else { 1 } {
        let old = cpu.vfp.d(d + r);
        let value = match (bitwise, invert) {
            (true, false) => old | imm,
            (true, true) => old & !imm,
            (false, true) if cmode != 0b1110 => !imm,
            _ => imm,
        };
        cpu.vfp.set_d(d + r, value);
    }
    None
}
