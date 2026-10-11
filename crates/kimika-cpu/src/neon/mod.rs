//! NEON, the Advanced SIMD instructions, which work on the VFP's registers
//! as doublewords and quadwords of lanes. the encodings are ARM's, A7.4 for
//! data processing and A7.7 for loads and stores; Thumb's have their top
//! byte moved, which its decoder puts back before they get here.
//!
//! an instruction reads all its operands before it writes anything, as ARM's
//! pseudocode reads Din and Qin, so a destination that overlaps a source of
//! another size comes out the way the hardware has it. floating point is
//! ARM's standard whatever FPSCR says: flush to zero, default NaN, round to
//! nearest. what saturates sets FPSCR's QC.

mod float;
mod integer;
mod memory;
mod misc;
mod shift;

pub use memory::load_store;
pub use misc::duplicate_core;

use crate::bits::{bit, bits};
use crate::fp;
use crate::{Cpu, Exit};

/// A7.4, data processing, as ARM encodes it.
pub fn data_processing(cpu: &mut Cpu, op: u32) -> Option<Exit> {
    if !bit(op, 23) {
        return integer::three_same(cpu, op);
    }
    if bit(op, 4) {
        // with imm3 and L clear the immediate is a value, else a shift
        return if bits(op, 19, 21) == 0 && !bit(op, 7) { shift::immediate(cpu, op) } else { shift::shift(cpu, op) };
    }
    if bits(op, 20, 21) != 0b11 {
        return if bit(op, 6) { integer::scalar(cpu, op) } else { integer::different_lengths(cpu, op) };
    }
    if !bit(op, 24) {
        return misc::extract(cpu, op);
    }
    match bits(op, 10, 11) {
        0b00 | 0b01 => misc::two_registers(cpu, op),
        0b10 => misc::table(cpu, op),
        _ => misc::duplicate_scalar(cpu, op),
    }
}

/// an instruction NEON does not have, given as it was fetched: in Thumb data
/// processing is 111U 1111 where ARM has 1111 001U, and the loads and stores
/// 1111 1001 where ARM has 1111 0100.
#[cold]
fn undefined(cpu: &Cpu, op: u32) -> Option<Exit> {
    let fetched = match (cpu.cpsr.thumb, op >> 25) {
        (false, _) => op,
        (true, 0b111_1001) => 0xEF00_0000 | (op >> 24 & 1) << 28 | (op & 0x00FF_FFFF),
        (true, _) => 0xF900_0000 | (op & 0x00FF_FFFF),
    };
    cpu.undefined(fetched)
}

/// up to four doublewords of lanes: a D register's, a Q register's, two Q
/// registers' or a table's.
type Words = [u64; 4];

/// the D registers an instruction names: d is D:Vd, n is N:Vn and m is M:Vm.
fn registers(op: u32) -> (usize, usize, usize) {
    let d = (bit(op, 22) as usize) << 4 | bits(op, 12, 15) as usize;
    let n = (bit(op, 7) as usize) << 4 | bits(op, 16, 19) as usize;
    let m = (bit(op, 5) as usize) << 4 | (op & 0xF) as usize;
    (d, n, m)
}

/// count D registers from first on.
fn read(cpu: &Cpu, first: usize, count: usize) -> Words {
    let mut words = [0; 4];
    for (i, word) in words.iter_mut().take(count).enumerate() {
        *word = cpu.vfp.d(first + i);
    }
    words
}

fn write(cpu: &mut Cpu, first: usize, words: &Words, count: usize) {
    for (i, &word) in words.iter().take(count).enumerate() {
        cpu.vfp.set_d(first + i, word);
    }
}

#[inline(always)]
fn mask(size: u32) -> u64 {
    if size >= 64 {
        u64::MAX
    } else {
        (1 << size) - 1
    }
}

/// lane e of size bits, counting on from the first word's bottom.
#[inline(always)]
fn lane(words: &[u64], e: usize, size: u32) -> u64 {
    let at = e * size as usize;
    (words[at / 64] >> (at % 64)) & mask(size)
}

#[inline(always)]
fn set_lane(words: &mut [u64], e: usize, size: u32, value: u64) {
    let at = e * size as usize;
    let shift = at % 64;
    let word = &mut words[at / 64];
    *word = (*word & !(mask(size) << shift)) | (value & mask(size)) << shift;
}

/// a lane as a number, by its sign or not.
#[inline(always)]
fn int(value: u64, size: u32, unsigned: bool) -> i128 {
    if unsigned {
        value as i128
    } else {
        ((value << (64 - size)) as i64 >> (64 - size)) as i128
    }
}

/// value saturated to a lane of size bits, and whether it had to be.
#[inline(always)]
fn saturate(value: i128, size: u32, unsigned: bool) -> (u64, bool) {
    let (min, max) =
        if unsigned { (0, (1i128 << size) - 1) } else { (-(1i128 << (size - 1)), (1i128 << (size - 1)) - 1) };
    if value < min {
        (min as u64 & mask(size), true)
    } else if value > max {
        (max as u64, true)
    } else {
        (value as u64 & mask(size), false)
    }
}

/// value shifted left by shift, or right where it is negative, the bits
/// shifted out on the right rounded off when round is set. shifting a lane
/// left by its size or more gives a number past any lane's range with the
/// value's sign, which saturating takes to the limit and truncating to zero.
#[inline(always)]
fn shift_by(value: i128, shift: i32, size: u32, round: bool) -> i128 {
    if shift >= 0 {
        if shift as u32 >= size {
            value.signum() << 100
        } else {
            value << shift
        }
    } else {
        let right = shift.unsigned_abs();
        if !round {
            value >> right.min(127)
        } else if right > 65 {
            // a lane is under 2^64 either way, so half of 2^66 or more
            // rounds it to zero
            0
        } else {
            (value + (1i128 << (right - 1))) >> right
        }
    }
}

/// a lane repeated across a doubleword.
fn replicate(value: u64, size: u32) -> u64 {
    let mut word = 0;
    for e in 0..64 / size as usize {
        set_lane(std::slice::from_mut(&mut word), e, size, value);
    }
    word
}

/// carryless multiplication, of the polynomials over GF(2) two lanes are.
fn polynomial(a: u64, b: u64, size: u32) -> u64 {
    (0..size).filter(|i| b >> i & 1 == 1).fold(0, |product, i| product ^ a << i)
}

/// all ones or all zeros, as comparisons give.
#[inline(always)]
fn all(test: bool) -> u64 {
    if test {
        u64::MAX
    } else {
        0
    }
}

/// QC, sticky until software clears it.
#[inline(always)]
fn saturated(cpu: &mut Cpu, saturated: bool) {
    if saturated {
        cpu.vfp.fpscr |= fp::QC;
    }
}
