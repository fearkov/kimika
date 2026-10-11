//! the integer arithmetic: three registers of the same length, of different
//! lengths, and two registers and a scalar.

use super::{
    all, float, int, lane, polynomial, read, registers, saturate, saturated, set_lane, shift_by, undefined, write,
};
use crate::bits::{bit, bits};
use crate::{Cpu, Exit};

/// A7.4.1, three registers of the same length. U is the signedness where
/// the operation has one, else it picks between two.
pub(super) fn three_same(cpu: &mut Cpu, op: u32) -> Option<Exit> {
    let (unsigned, size, opc, quad, o1) = (bit(op, 24), bits(op, 20, 21), bits(op, 8, 11), bit(op, 6), bit(op, 4));
    let (d, n, m) = registers(op);
    if quad && (d | n | m) & 1 == 1 {
        return undefined(cpu, op);
    }
    if opc >= 0b1100 {
        return float::three_same(cpu, op);
    }
    let regs = if quad { 2 } else { 1 };
    let (a, b, old) = (read(cpu, n, regs), read(cpu, m, regs), read(cpu, d, regs));

    // the bitwise operations, their size bits naming which
    if (opc, o1) == (0b0001, true) {
        let mut result = [0; 4];
        for r in 0..regs {
            let (x, y, z) = (a[r], b[r], old[r]);
            result[r] = match (unsigned, size) {
                (false, 0) => x & y,
                (false, 1) => x & !y,
                (false, 2) => x | y,
                (false, _) => x | !y,
                (true, 0) => x ^ y,
                // VBSL, VBIT and VBIF
                (true, 1) => (x & z) | (y & !z),
                (true, 2) => (z & !y) | (x & y),
                (true, _) => (z & y) | (x & !y),
            };
        }
        write(cpu, d, &result, regs);
        return None;
    }

    let fits = match (opc, o1) {
        // the saturating ones, the shifts and VADD and VSUB have 64-bit lanes
        (0b0000 | 0b0010, true) | (0b0100 | 0b0101, _) | (0b1000, false) => true,
        // VMUL of polynomials has bytes only
        (0b1001, true) if unsigned => size == 0,
        // VQDMULH and VQRDMULH halfwords and words
        (0b1011, false) => size == 1 || size == 2,
        // VPADD, and with U the ARMv8.1 VQRDMLAH
        (0b1011, true) => !unsigned && size != 3,
        _ => size != 3,
    };
    let pairwise = opc == 0b1010 || (opc, o1) == (0b1011, true);
    if !fits || (pairwise && quad) {
        return undefined(cpu, op);
    }
    let esize = 8 << size;
    let int = |x| int(x, esize, unsigned);
    let signed = |x| super::int(x, esize, false);

    if pairwise {
        // VPMAX, VPMIN and VPADD: n's pairs, then m's
        let half = 32 / esize as usize;
        let mut result = [0; 4];
        for (i, source) in [&a, &b].into_iter().enumerate() {
            for e in 0..half {
                let (x, y) = (lane(source, 2 * e, esize), lane(source, 2 * e + 1, esize));
                let value = match (opc, o1) {
                    (0b1010, false) => int(x).max(int(y)) as u64,
                    (0b1010, true) => int(x).min(int(y)) as u64,
                    _ => x.wrapping_add(y),
                };
                set_lane(&mut result, i * half + e, esize, value);
            }
        }
        write(cpu, d, &result, 1);
        return None;
    }

    let mut qc = false;
    let mut result = [0; 4];
    for e in 0..regs * 64 / esize as usize {
        let (x, y, z) = (lane(&a, e, esize), lane(&b, e, esize), lane(&old, e, esize));
        // the shifts by register shift m's lanes by the bottom byte of n's
        let amount = x as u8 as i8 as i32;
        let mut saturating = |value| {
            let (value, clipped) = saturate(value, esize, unsigned);
            qc |= clipped;
            value
        };
        let value = match (opc, o1) {
            (0b0000, false) => ((int(x) + int(y)) >> 1) as u64,
            (0b0000, true) => saturating(int(x) + int(y)),
            (0b0001, _) => ((int(x) + int(y) + 1) >> 1) as u64,
            (0b0010, false) => ((int(x) - int(y)) >> 1) as u64,
            (0b0010, true) => saturating(int(x) - int(y)),
            (0b0011, false) => all(int(x) > int(y)),
            (0b0011, true) => all(int(x) >= int(y)),
            (0b0100, false) => shift_by(int(y), amount, esize, false) as u64,
            (0b0100, true) => saturating(shift_by(int(y), amount, esize, false)),
            (0b0101, false) => shift_by(int(y), amount, esize, true) as u64,
            (0b0101, true) => saturating(shift_by(int(y), amount, esize, true)),
            (0b0110, false) => int(x).max(int(y)) as u64,
            (0b0110, true) => int(x).min(int(y)) as u64,
            (0b0111, false) => (int(x) - int(y)).unsigned_abs() as u64,
            (0b0111, true) => z.wrapping_add((int(x) - int(y)).unsigned_abs() as u64),
            (0b1000, false) if unsigned => x.wrapping_sub(y),
            (0b1000, false) => x.wrapping_add(y),
            (0b1000, true) => all(if unsigned { x == y } else { x & y != 0 }),
            (0b1001, false) if unsigned => z.wrapping_sub(x.wrapping_mul(y)),
            (0b1001, false) => z.wrapping_add(x.wrapping_mul(y)),
            (0b1001, true) if unsigned => polynomial(x, y, esize),
            (0b1001, true) => x.wrapping_mul(y),
            // VQDMULH and VQRDMULH, signed whatever U says
            _ => {
                let round = if unsigned { 1i128 << (esize - 1) } else { 0 };
                let (value, clipped) = saturate((2 * signed(x) * signed(y) + round) >> esize, esize, false);
                qc |= clipped;
                value
            }
        };
        set_lane(&mut result, e, esize, value);
    }
    write(cpu, d, &result, regs);
    saturated(cpu, qc);
    None
}

/// A7.4.2, three registers of different lengths: long results, wide
/// operands, and results narrowed to their high halves.
pub(super) fn different_lengths(cpu: &mut Cpu, op: u32) -> Option<Exit> {
    let (unsigned, size, opc) = (bit(op, 24), bits(op, 20, 21), bits(op, 8, 11));
    let (d, n, m) = registers(op);
    let esize = 8 << size;
    let long = 2 * esize;
    let elements = 64 / esize as usize;
    let mut result = [0; 4];

    if opc == 0b0100 || opc == 0b0110 {
        // VADDHN, VSUBHN, and with U rounding, VRADDHN and VRSUBHN
        if (n | m) & 1 == 1 {
            return undefined(cpu, op);
        }
        let (a, b) = (read(cpu, n, 2), read(cpu, m, 2));
        let round = if unsigned { 1u64 << (esize - 1) } else { 0 };
        for e in 0..elements {
            let (x, y) = (lane(&a, e, long), lane(&b, e, long));
            let total = if opc == 0b0100 { x.wrapping_add(y) } else { x.wrapping_sub(y) };
            set_lane(&mut result, e, esize, (total.wrapping_add(round) & super::mask(long)) >> esize);
        }
        write(cpu, d, &result, 1);
        return None;
    }

    // the rest have a quadword destination; VADDW and VSUBW a quadword n
    let wide = opc == 0b0001 || opc == 0b0011;
    let doubling = matches!(opc, 0b1001 | 0b1011 | 0b1101);
    let refused = d & 1 == 1
        || (wide && n & 1 == 1)
        || (doubling && (unsigned || size == 0))
        || (opc == 0b1110 && (unsigned || size != 0))
        || opc == 0b1111;
    if refused {
        return undefined(cpu, op);
    }
    let a = read(cpu, n, if wide { 2 } else { 1 });
    let (b, old) = (read(cpu, m, 1), read(cpu, d, 2));
    let mut qc = false;
    for e in 0..elements {
        let x = if wide { int(lane(&a, e, long), long, unsigned) } else { int(lane(&a, e, esize), esize, unsigned) };
        let y = int(lane(&b, e, esize), esize, unsigned);
        let z = lane(&old, e, long);
        let value = match opc {
            0b0000 | 0b0001 => (x + y) as u64,
            0b0010 | 0b0011 => (x - y) as u64,
            0b0101 => z.wrapping_add((x - y).unsigned_abs() as u64),
            0b0111 => (x - y).unsigned_abs() as u64,
            0b1000 => z.wrapping_add((x * y) as u64),
            0b1010 => z.wrapping_sub((x * y) as u64),
            0b1100 => (x * y) as u64,
            0b1110 => polynomial(lane(&a, e, 8), lane(&b, e, 8), 8),
            _ => {
                let accumulate = match opc {
                    0b1001 => Accumulate::Add,
                    0b1011 => Accumulate::Subtract,
                    _ => Accumulate::Not,
                };
                let (value, clipped) = doubling_long(x, y, z, long, accumulate);
                qc |= clipped;
                value
            }
        };
        set_lane(&mut result, e, long, value);
    }
    write(cpu, d, &result, 2);
    saturated(cpu, qc);
    None
}

/// what a long doubling multiply does with its product.
#[derive(Clone, Copy)]
enum Accumulate {
    Not,
    Add,
    Subtract,
}

/// VQDMULL, VQDMLAL and VQDMLSL: twice x times y saturated to a long lane,
/// then added to the accumulator or subtracted from it, saturating again.
fn doubling_long(x: i128, y: i128, accumulator: u64, long: u32, accumulate: Accumulate) -> (u64, bool) {
    let (product, first) = saturate(2 * x * y, long, false);
    let (accumulator, product_value) = (int(accumulator, long, false), int(product, long, false));
    let (value, second) = match accumulate {
        Accumulate::Not => (product, false),
        Accumulate::Add => saturate(accumulator + product_value, long, false),
        Accumulate::Subtract => saturate(accumulator - product_value, long, false),
    };
    (value, first || second)
}

/// A7.4.3, two registers and a scalar, a lane of m used for every lane.
pub(super) fn scalar(cpu: &mut Cpu, op: u32) -> Option<Exit> {
    let (top, size, opc) = (bit(op, 24), bits(op, 20, 21), bits(op, 8, 11));
    let (d, n, _) = registers(op);
    if size == 0 {
        return undefined(cpu, op);
    }
    let esize = 8 << size;
    // halfword scalars are in d0-d7, M:Vm<3> their index; words in any
    let (m, index) = if size == 1 {
        ((op & 7) as usize, (bit(op, 5) as usize) << 1 | bit(op, 3) as usize)
    } else {
        ((op & 0xF) as usize, bit(op, 5) as usize)
    };
    let scalar = lane(&[cpu.vfp.d(m)], index, esize);
    let mut qc = false;
    let mut result = [0; 4];
    match opc {
        // VMLA, VMLS and VMUL, F choosing floating point, and VQDMULH and
        // VQRDMULH, all with Q on top
        0b0000 | 0b0001 | 0b0100 | 0b0101 | 0b1000 | 0b1001 | 0b1100 | 0b1101 => {
            let quad = top;
            let floating = opc < 0b1100 && bit(op, 8);
            if (quad && (d | n) & 1 == 1) || (floating && size != 2) {
                return undefined(cpu, op);
            }
            let regs = if quad { 2 } else { 1 };
            let (a, old) = (read(cpu, n, regs), read(cpu, d, regs));
            for e in 0..regs * 64 / esize as usize {
                let (x, z) = (lane(&a, e, esize), lane(&old, e, esize));
                let value = match opc {
                    0b0001 => float::multiply_add(z as u32, x as u32, scalar as u32, false) as u64,
                    0b0101 => float::multiply_add(z as u32, x as u32, scalar as u32, true) as u64,
                    0b1001 => float::multiply(x as u32, scalar as u32) as u64,
                    0b0000 => z.wrapping_add(x.wrapping_mul(scalar)),
                    0b0100 => z.wrapping_sub(x.wrapping_mul(scalar)),
                    0b1000 => x.wrapping_mul(scalar),
                    _ => {
                        let round = if opc == 0b1101 { 1i128 << (esize - 1) } else { 0 };
                        let product = 2 * int(x, esize, false) * int(scalar, esize, false);
                        let (value, clipped) = saturate((product + round) >> esize, esize, false);
                        qc |= clipped;
                        value
                    }
                };
                set_lane(&mut result, e, esize, value);
            }
            write(cpu, d, &result, regs);
        }
        // VMLAL, VMLSL and VMULL, U the signedness, and VQDMLAL, VQDMLSL and
        // VQDMULL, which are signed
        0b0010 | 0b0110 | 0b1010 | 0b0011 | 0b0111 | 0b1011 => {
            let doubling = opc & 1 == 1;
            if d & 1 == 1 || (doubling && top) {
                return undefined(cpu, op);
            }
            let unsigned = top;
            let long = 2 * esize;
            let (a, old) = (read(cpu, n, 1), read(cpu, d, 2));
            let y = int(scalar, esize, unsigned);
            for e in 0..64 / esize as usize {
                let x = int(lane(&a, e, esize), esize, unsigned);
                let z = lane(&old, e, long);
                let value = match opc {
                    0b0010 => z.wrapping_add((x * y) as u64),
                    0b0110 => z.wrapping_sub((x * y) as u64),
                    0b1010 => (x * y) as u64,
                    _ => {
                        let accumulate = match opc {
                            0b0011 => Accumulate::Add,
                            0b0111 => Accumulate::Subtract,
                            _ => Accumulate::Not,
                        };
                        let (value, clipped) = doubling_long(x, y, z, long, accumulate);
                        qc |= clipped;
                        value
                    }
                };
                set_lane(&mut result, e, long, value);
            }
            write(cpu, d, &result, 2);
        }
        _ => return undefined(cpu, op),
    }
    saturated(cpu, qc);
    None
}
