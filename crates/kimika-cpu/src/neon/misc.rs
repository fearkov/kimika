//! two registers misc, which has the unary operations, the permutations and
//! the narrowing moves, and VEXT, VTBL, VTBX and VDUP.

use super::{
    all, float, int, lane, mask, read, registers, replicate, saturate, saturated, set_lane, undefined, write, Words,
};
use crate::bits::{bit, bits};
use crate::{Cpu, Exit, PC};

/// A7.4.5, two registers misc. size, opc1 at 17:16 and opc2 at 10:7 pick the
/// operation, and Q is bit 6 as usual.
pub(super) fn two_registers(cpu: &mut Cpu, op: u32) -> Option<Exit> {
    let (size, opc1, opc2, quad) = (bits(op, 18, 19), bits(op, 16, 17), bits(op, 7, 10), bit(op, 6));
    let (d, _, m) = registers(op);
    let esize = 8 << size;
    let regs = if quad { 2 } else { 1 };
    let mut result: Words = [0; 4];

    // the narrowing moves, the widening VSHLL and the conversions between
    // halves and singles each have a doubleword on one side
    match (opc1, opc2) {
        (0b10, 0b0100 | 0b0101) => {
            if size == 3 || m & 1 == 1 {
                return undefined(cpu, op);
            }
            // bits 7:6: VMOVN, VQMOVUN, VQMOVN signed and unsigned
            let kind = bits(op, 6, 7);
            let a = read(cpu, m, 2);
            let long = 2 * esize;
            let mut qc = false;
            for e in 0..64 / esize as usize {
                let x = lane(&a, e, long);
                let value = if kind == 0 {
                    x
                } else {
                    let (value, clipped) = saturate(int(x, long, kind == 0b11), esize, kind != 0b10);
                    qc |= clipped;
                    value
                };
                set_lane(&mut result, e, esize, value);
            }
            write(cpu, d, &result, 1);
            saturated(cpu, qc);
            return None;
        }
        (0b10, 0b0110) => {
            if quad || size == 3 || d & 1 == 1 {
                return undefined(cpu, op);
            }
            // VSHLL by the lane's size
            let a = read(cpu, m, 1);
            for e in 0..64 / esize as usize {
                set_lane(&mut result, e, 2 * esize, lane(&a, e, esize) << esize);
            }
            write(cpu, d, &result, 2);
            return None;
        }
        (0b10, 0b1100 | 0b1110) => {
            let to_single = bit(op, 8);
            if quad || size != 1 || (to_single && d & 1 == 1) || (!to_single && m & 1 == 1) {
                return undefined(cpu, op);
            }
            if to_single {
                let a = read(cpu, m, 1);
                for e in 0..4 {
                    set_lane(&mut result, e, 32, float::from_half(lane(&a, e, 16) as u16) as u64);
                }
                write(cpu, d, &result, 2);
            } else {
                let a = read(cpu, m, 2);
                for e in 0..4 {
                    set_lane(&mut result, e, 16, float::to_half(lane(&a, e, 32) as u32) as u64);
                }
                write(cpu, d, &result, 1);
            }
            return None;
        }
        _ => {}
    }

    // the rest keep the size, and a quadword is an even pair
    if quad && (d | m) & 1 == 1 {
        return undefined(cpu, op);
    }
    if opc1 == 0b10 {
        return permute(cpu, op, opc2, size, quad, d, m);
    }
    let a = read(cpu, m, regs);
    let lanes = regs * 64 / esize as usize;
    let mut qc = false;
    match (opc1, opc2) {
        // VREV64, VREV32 and VREV16, reversing the lanes in each group
        (0b00, 0b0000..=0b0010) => {
            if opc2 + size >= 3 {
                return undefined(cpu, op);
            }
            let reverse = (1 << (3 - opc2 - size)) - 1;
            for e in 0..lanes {
                set_lane(&mut result, e ^ reverse, esize, lane(&a, e, esize));
            }
        }
        // VPADDL and VPADAL, adding pairs into lanes twice the size, bit 7
        // unsigned
        (0b00, 0b0100 | 0b0101 | 0b1100 | 0b1101) => {
            if size == 3 {
                return undefined(cpu, op);
            }
            let unsigned = bit(op, 7);
            let accumulate = opc2 >= 0b1100;
            let old = read(cpu, d, regs);
            let long = 2 * esize;
            for e in 0..lanes / 2 {
                let sum =
                    int(lane(&a, 2 * e, esize), esize, unsigned) + int(lane(&a, 2 * e + 1, esize), esize, unsigned);
                let base = if accumulate { lane(&old, e, long) } else { 0 };
                set_lane(&mut result, e, long, base.wrapping_add(sum as u64));
            }
        }
        // VCLS, VCLZ, VCNT and VMVN
        (0b00, 0b1000..=0b1011) => {
            if size == 3 || (opc2 >= 0b1010 && size != 0) {
                return undefined(cpu, op);
            }
            for e in 0..lanes {
                let x = lane(&a, e, esize);
                let value = match opc2 {
                    0b1000 => {
                        // the bits after the sign that equal it
                        let flipped = (x ^ (x >> 1)) & mask(esize - 1);
                        (flipped.leading_zeros() - (64 - (esize - 1))) as u64
                    }
                    0b1001 => (x.leading_zeros() - (64 - esize)) as u64,
                    0b1010 => x.count_ones() as u64,
                    _ => !x,
                };
                set_lane(&mut result, e, esize, value);
            }
        }
        // VQABS and VQNEG
        (0b00, 0b1110 | 0b1111) => {
            if size == 3 {
                return undefined(cpu, op);
            }
            for e in 0..lanes {
                let x = int(lane(&a, e, esize), esize, false);
                let (value, clipped) = saturate(if opc2 == 0b1110 { x.abs() } else { -x }, esize, false);
                qc |= clipped;
                set_lane(&mut result, e, esize, value);
            }
        }
        // the comparisons with zero, VABS and VNEG, bit 10 choosing floating
        // point
        (0b01, _) => {
            let floating = bit(op, 10);
            let kind = opc2 & 0b111;
            if size == 3 || (floating && size != 2) || kind == 0b101 {
                return undefined(cpu, op);
            }
            for e in 0..lanes {
                let x = lane(&a, e, esize);
                let value = match (floating, kind) {
                    (true, 0b110) => x & 0x7FFF_FFFF,
                    (true, 0b111) => x ^ 0x8000_0000,
                    (true, _) => all(float::compare_zero(x as u32, kind)),
                    (false, 0b110) => int(x, esize, false).unsigned_abs() as u64,
                    (false, 0b111) => (-int(x, esize, false)) as u64,
                    (false, _) => {
                        let x = int(x, esize, false);
                        all(match kind {
                            0 => x > 0,
                            1 => x >= 0,
                            2 => x == 0,
                            3 => x <= 0,
                            _ => x < 0,
                        })
                    }
                };
                set_lane(&mut result, e, esize, value);
            }
        }
        // VRECPE and VRSQRTE, bit 8 floating point, and VCVT between single
        // precision and integers, bit 8 to integers and bit 7 unsigned
        (0b11, 0b1000..=0b1111) => {
            if size != 2 {
                return undefined(cpu, op);
            }
            let (high, low) = (bit(op, 8), bit(op, 7));
            for e in 0..lanes {
                let x = lane(&a, e, 32) as u32;
                let value = match (opc2 >= 0b1100, high, low) {
                    (false, false, false) => float::unsigned_reciprocal_estimate(x),
                    (false, false, true) => float::unsigned_reciprocal_root_estimate(x),
                    (false, true, false) => float::reciprocal_estimate(x),
                    (false, true, true) => float::reciprocal_root_estimate(x),
                    (true, false, unsigned) => float::from_fixed(x, 0, unsigned),
                    (true, true, unsigned) => float::to_fixed(x, 0, unsigned),
                };
                set_lane(&mut result, e, 32, value as u64);
            }
        }
        // AESE, AESD, AESMC and AESIMC are ARMv8's, and the rest of opc1
        // 0b11 its rounding conversions
        _ => return undefined(cpu, op),
    }
    write(cpu, d, &result, regs);
    saturated(cpu, qc);
    None
}

/// VSWP, VTRN, VUZP and VZIP, which write both registers. with the same
/// register twice ARM leaves the result unknown, and here it is d's half,
/// written after m's, as QEMU has it but for its VTRN.32.
fn permute(cpu: &mut Cpu, op: u32, opc2: u32, size: u32, quad: bool, d: usize, m: usize) -> Option<Exit> {
    let refused = match opc2 {
        0b0000 => size != 0,
        0b0001 => size == 3,
        0b0010 | 0b0011 => size == 3 || (!quad && size == 2),
        _ => true,
    };
    if refused {
        return undefined(cpu, op);
    }
    let esize = 8 << size;
    let regs = if quad { 2 } else { 1 };
    let (mut x, mut y) = (read(cpu, d, regs), read(cpu, m, regs));
    let lanes = regs * 64 / esize as usize;
    match opc2 {
        0b0000 => std::mem::swap(&mut x, &mut y),
        0b0001 => {
            // each odd lane of d with the even one below it in m
            for e in (0..lanes).step_by(2) {
                let (a, b) = (lane(&x, e + 1, esize), lane(&y, e, esize));
                set_lane(&mut x, e + 1, esize, b);
                set_lane(&mut y, e, esize, a);
            }
        }
        _ => {
            // d's lanes then m's, taken apart by evens and odds or put
            // together alternately
            let joined: Vec<u64> =
                (0..lanes).map(|e| lane(&x, e, esize)).chain((0..lanes).map(|e| lane(&y, e, esize))).collect();
            for e in 0..lanes {
                if opc2 == 0b0010 {
                    set_lane(&mut x, e, esize, joined[2 * e]);
                    set_lane(&mut y, e, esize, joined[2 * e + 1]);
                } else {
                    let (low, high) = (e / 2, lanes / 2 + e / 2);
                    let source = if e % 2 == 0 { 0 } else { lanes };
                    set_lane(&mut x, e, esize, joined[source + low]);
                    set_lane(&mut y, e, esize, joined[source + high]);
                }
            }
        }
    }
    write(cpu, m, &y, regs);
    write(cpu, d, &x, regs);
    None
}

/// VEXT: the bytes of n then m, from imm4 on.
pub(super) fn extract(cpu: &mut Cpu, op: u32) -> Option<Exit> {
    let (quad, imm4) = (bit(op, 6), bits(op, 8, 11) as usize);
    let (d, n, m) = registers(op);
    if (quad && (d | n | m) & 1 == 1) || (!quad && imm4 >= 8) {
        return undefined(cpu, op);
    }
    let regs = if quad { 2 } else { 1 };
    let (a, b) = (read(cpu, n, regs), read(cpu, m, regs));
    let bytes = regs * 8;
    let mut result = [0; 4];
    for e in 0..bytes {
        let at = e + imm4;
        let byte = if at < bytes { lane(&a, at, 8) } else { lane(&b, at - bytes, 8) };
        set_lane(&mut result, e, 8, byte);
    }
    write(cpu, d, &result, regs);
    None
}

/// VTBL and VTBX: each byte of m an index into a table of one to four
/// registers from n, a byte past it zero for VTBL and left alone by VTBX.
pub(super) fn table(cpu: &mut Cpu, op: u32) -> Option<Exit> {
    let (length, extend) = (bits(op, 8, 9) as usize + 1, bit(op, 6));
    let (d, n, m) = registers(op);
    if n + length > 32 {
        return undefined(cpu, op);
    }
    let (table, indices, old) = (read(cpu, n, length), read(cpu, m, 1), read(cpu, d, 1));
    let mut result = [0; 4];
    for e in 0..8 {
        let index = lane(&indices, e, 8) as usize;
        let byte = if index < 8 * length {
            lane(&table, index, 8)
        } else if extend {
            lane(&old, e, 8)
        } else {
            0
        };
        set_lane(&mut result, e, 8, byte);
    }
    write(cpu, d, &result, 1);
    None
}

/// VDUP of a scalar, imm4's lowest set bit giving its size and the bits
/// above it the index.
pub(super) fn duplicate_scalar(cpu: &mut Cpu, op: u32) -> Option<Exit> {
    let (imm4, quad) = (bits(op, 16, 19), bit(op, 6));
    let (d, _, m) = registers(op);
    let (esize, index) = match imm4 {
        _ if imm4 & 1 == 1 => (8, imm4 >> 1),
        _ if imm4 & 2 == 2 => (16, imm4 >> 2),
        _ if imm4 & 4 == 4 => (32, imm4 >> 3),
        _ => return undefined(cpu, op),
    };
    if bits(op, 7, 9) != 0 || (quad && d & 1 == 1) {
        return undefined(cpu, op);
    }
    let value = replicate(lane(&[cpu.vfp.d(m)], index as usize, esize), esize);
    write(cpu, d, &[value; 4], if quad { 2 } else { 1 });
    None
}

/// VDUP of a core register, a transfer on coprocessor 11 with bit 23 set:
/// B and E give the size, Q bit 21 and D:Vd at bit 7 and 19:16.
pub fn duplicate_core(cpu: &mut Cpu, op: u32) -> Option<Exit> {
    let (b, quad, e) = (bit(op, 22), bit(op, 21), bit(op, 5));
    let d = (bit(op, 7) as usize) << 4 | bits(op, 16, 19) as usize;
    let t = bits(op, 12, 15) as usize;
    let esize = match (b, e) {
        (false, false) => 32,
        (false, true) => 16,
        (true, false) => 8,
        _ => return cpu.undefined(op),
    };
    if bit(op, 6) || op & 0xF != 0 || (quad && d & 1 == 1) || t == PC {
        return cpu.undefined(op);
    }
    let value = replicate(cpu.regs[t] as u64 & mask(esize), esize);
    write(cpu, d, &[value; 4], if quad { 2 } else { 1 });
    None
}
