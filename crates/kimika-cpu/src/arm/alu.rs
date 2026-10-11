//! arithmetic shared by the ARM and Thumb decoders, and the data-processing
//! operations themselves, which both encode the same sixteen of.

use crate::{Cpu, PC};

#[inline(always)]
pub fn add_with_flags(a: u32, b: u32) -> (u32, bool, bool) {
    let (result, carry) = a.overflowing_add(b);
    let overflow = ((a ^ result) & (b ^ result)) >> 31 != 0;
    (result, carry, overflow)
}

#[inline(always)]
pub fn adc_with_flags(a: u32, b: u32, carry_in: bool) -> (u32, bool, bool) {
    let sum = a as u64 + b as u64 + carry_in as u64;
    let result = sum as u32;
    let carry = sum > 0xFFFF_FFFF;
    let overflow = ((a ^ result) & (b ^ result)) >> 31 != 0;
    (result, carry, overflow)
}

/// ARM subtraction, where the carry flag means no borrow.
#[inline(always)]
pub fn sub_with_flags(a: u32, b: u32) -> (u32, bool, bool) {
    let result = a.wrapping_sub(b);
    let carry = a >= b;
    let overflow = ((a ^ b) & (a ^ result)) >> 31 != 0;
    (result, carry, overflow)
}

#[inline(always)]
pub fn sbc_with_flags(a: u32, b: u32, carry_in: bool) -> (u32, bool, bool) {
    adc_with_flags(a, !b, carry_in)
}

/// signed saturation to 32 bits, for QADD and the rest.
#[inline(always)]
pub fn saturate_i32(value: i64) -> (u32, bool) {
    if value > i32::MAX as i64 {
        (i32::MAX as u32, true)
    } else if value < i32::MIN as i64 {
        (i32::MIN as u32, true)
    } else {
        (value as u32, false)
    }
}

/// signed saturation to bits bits, SSAT.
#[inline(always)]
pub fn signed_saturate(value: i64, bits: u32) -> (u32, bool) {
    let max = (1i64 << (bits - 1)) - 1;
    let min = -(1i64 << (bits - 1));
    if value > max {
        (max as u32, true)
    } else if value < min {
        (min as u32, true)
    } else {
        (value as u32, false)
    }
}

/// unsigned saturation to bits bits, USAT.
#[inline(always)]
pub fn unsigned_saturate(value: i64, bits: u32) -> (u32, bool) {
    let max = if bits >= 32 { u32::MAX as i64 } else { (1i64 << bits) - 1 };
    if value > max {
        (max as u32, true)
    } else if value < 0 {
        (0, true)
    } else {
        (value as u32, false)
    }
}

/// the sixteen data-processing operations, numbered as ARM encodes them.
/// Thumb's ORN takes the slot ARM gives RSC, which Thumb lacks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Operation {
    And,
    Eor,
    Sub,
    Rsb,
    Add,
    Adc,
    Sbc,
    Rsc,
    Tst,
    Teq,
    Cmp,
    Cmn,
    Orr,
    Mov,
    Bic,
    Mvn,
    Orn,
}

impl Operation {
    pub fn from_arm(opcode: u32) -> Operation {
        use Operation::*;
        [And, Eor, Sub, Rsb, Add, Adc, Sbc, Rsc, Tst, Teq, Cmp, Cmn, Orr, Mov, Bic, Mvn][(opcode & 0xF) as usize]
    }

    /// the comparisons, which only set flags.
    pub fn is_compare(self) -> bool {
        matches!(self, Operation::Tst | Operation::Teq | Operation::Cmp | Operation::Cmn)
    }
}

/// runs a data-processing operation on a, Rn's value, and b, the shifted
/// operand, writing Rd unless it compares, and the flags when set_flags,
/// the logical operations taking the shifter's carry and leaving V alone.
#[inline(always)]
pub fn data_processing(
    cpu: &mut Cpu,
    operation: Operation,
    set_flags: bool,
    rd: usize,
    a: u32,
    b: u32,
    shifter_carry: bool,
) {
    let carry_in = cpu.cpsr.c;
    let (result, carry, overflow) = match operation {
        Operation::And | Operation::Tst => (a & b, shifter_carry, None),
        Operation::Eor | Operation::Teq => (a ^ b, shifter_carry, None),
        Operation::Orr => (a | b, shifter_carry, None),
        Operation::Orn => (a | !b, shifter_carry, None),
        Operation::Mov => (b, shifter_carry, None),
        Operation::Bic => (a & !b, shifter_carry, None),
        Operation::Mvn => (!b, shifter_carry, None),
        arithmetic => {
            let (result, carry, overflow) = match arithmetic {
                Operation::Sub | Operation::Cmp => sub_with_flags(a, b),
                Operation::Rsb => sub_with_flags(b, a),
                Operation::Add | Operation::Cmn => add_with_flags(a, b),
                Operation::Adc => adc_with_flags(a, b, carry_in),
                Operation::Sbc => sbc_with_flags(a, b, carry_in),
                _ => sbc_with_flags(b, a, carry_in),
            };
            (result, carry, Some(overflow))
        }
    };
    if set_flags {
        cpu.set_nz(result);
        cpu.cpsr.c = carry;
        if let Some(overflow) = overflow {
            cpu.cpsr.v = overflow;
        }
    }
    if !operation.is_compare() {
        if rd == PC {
            cpu.alu_write_pc(result);
        } else {
            cpu.regs[rd] = result;
        }
    }
}
