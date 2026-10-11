//! the multiplies, plain, long, halfword, dual and most significant word,
//! and the saturating additions that share their encoding space in ARM.
//! each takes its registers by number, as ARM and Thumb lay them out
//! differently, with n and m the operands and a the accumulator.

use super::alu::saturate_i32;
use crate::Cpu;

/// the bottom or top halfword of value, signed.
#[inline(always)]
fn half(value: u32, top: bool) -> i64 {
    if top {
        (value >> 16) as i16 as i64
    } else {
        value as i16 as i64
    }
}

fn pair(cpu: &Cpu, low: usize, high: usize) -> u64 {
    (cpu.regs[high] as u64) << 32 | cpu.regs[low] as u64
}

fn set_pair(cpu: &mut Cpu, low: usize, high: usize, value: u64) {
    cpu.regs[low] = value as u32;
    cpu.regs[high] = (value >> 32) as u32;
}

/// MUL and MLA. the flags take N and Z only.
pub fn multiply(cpu: &mut Cpu, d: usize, n: usize, m: usize, a: Option<usize>, set_flags: bool) {
    let mut result = cpu.regs[n].wrapping_mul(cpu.regs[m]);
    if let Some(a) = a {
        result = result.wrapping_add(cpu.regs[a]);
    }
    cpu.regs[d] = result;
    if set_flags {
        cpu.set_nz(result);
    }
}

/// MLS, the accumulator minus the product.
pub fn multiply_subtract(cpu: &mut Cpu, d: usize, n: usize, m: usize, a: usize) {
    cpu.regs[d] = cpu.regs[a].wrapping_sub(cpu.regs[n].wrapping_mul(cpu.regs[m]));
}

/// UMULL, UMLAL, SMULL and SMLAL into the low and high pair.
#[allow(clippy::too_many_arguments)]
pub fn long(
    cpu: &mut Cpu,
    low: usize,
    high: usize,
    n: usize,
    m: usize,
    signed: bool,
    accumulate: bool,
    set_flags: bool,
) {
    let product = if signed {
        (cpu.regs[n] as i32 as i64).wrapping_mul(cpu.regs[m] as i32 as i64) as u64
    } else {
        cpu.regs[n] as u64 * cpu.regs[m] as u64
    };
    let result = if accumulate { product.wrapping_add(pair(cpu, low, high)) } else { product };
    set_pair(cpu, low, high, result);
    if set_flags {
        cpu.cpsr.n = result >> 63 != 0;
        cpu.cpsr.z = result == 0;
    }
}

/// UMAAL, the product plus both halves of the pair, which can't overflow.
pub fn umaal(cpu: &mut Cpu, low: usize, high: usize, n: usize, m: usize) {
    let sum = cpu.regs[n] as u64 * cpu.regs[m] as u64 + cpu.regs[low] as u64 + cpu.regs[high] as u64;
    set_pair(cpu, low, high, sum);
}

/// SMLA<x><y> and SMUL<x><y>, sixteen by sixteen bits. the accumulation
/// wraps, an overflow only sets Q.
pub fn halfword(cpu: &mut Cpu, d: usize, n: usize, m: usize, a: Option<usize>, top_n: bool, top_m: bool) {
    let product = half(cpu.regs[n], top_n) * half(cpu.regs[m], top_m);
    match a {
        Some(a) => {
            let sum = product + cpu.regs[a] as i32 as i64;
            cpu.regs[d] = sum as u32;
            cpu.cpsr.q |= sum != sum as i32 as i64;
        }
        None => cpu.regs[d] = product as u32,
    }
}

/// SMLAW<y> and SMULW<y>, thirty-two by sixteen bits keeping the top 32
/// of the 48 bit product.
pub fn word_by_halfword(cpu: &mut Cpu, d: usize, n: usize, m: usize, a: Option<usize>, top_m: bool) {
    let product = (cpu.regs[n] as i32 as i64 * half(cpu.regs[m], top_m)) >> 16;
    match a {
        Some(a) => {
            let sum = product + cpu.regs[a] as i32 as i64;
            cpu.regs[d] = sum as u32;
            cpu.cpsr.q |= sum != sum as i32 as i64;
        }
        None => cpu.regs[d] = product as u32,
    }
}

/// SMLAL<x><y>, sixteen by sixteen bits into a 64 bit pair.
pub fn halfword_long(cpu: &mut Cpu, low: usize, high: usize, n: usize, m: usize, top_n: bool, top_m: bool) {
    let product = half(cpu.regs[n], top_n) * half(cpu.regs[m], top_m);
    let sum = (pair(cpu, low, high) as i64).wrapping_add(product);
    set_pair(cpu, low, high, sum as u64);
}

/// the two products of the halves of n and m, m's halves swapped first
/// when swap, and their sum or difference.
fn dual_products(cpu: &Cpu, n: usize, m: usize, swap: bool, subtract: bool) -> i64 {
    let a = cpu.regs[n];
    let b = if swap { cpu.regs[m].rotate_right(16) } else { cpu.regs[m] };
    let (low, high) = (half(a, false) * half(b, false), half(a, true) * half(b, true));
    if subtract {
        low - high
    } else {
        low + high
    }
}

/// SMLAD, SMUAD, SMLSD and SMUSD. the sum wraps, an overflow sets Q.
pub fn dual(cpu: &mut Cpu, d: usize, n: usize, m: usize, a: Option<usize>, swap: bool, subtract: bool) {
    let products = dual_products(cpu, n, m, swap, subtract);
    let sum = match a {
        Some(a) => products + cpu.regs[a] as i32 as i64,
        None => products,
    };
    cpu.regs[d] = sum as u32;
    cpu.cpsr.q |= sum != sum as i32 as i64;
}

/// SMLALD and SMLSLD into a 64 bit pair.
pub fn dual_long(cpu: &mut Cpu, low: usize, high: usize, n: usize, m: usize, swap: bool, subtract: bool) {
    let products = dual_products(cpu, n, m, swap, subtract);
    let sum = (pair(cpu, low, high) as i64).wrapping_add(products);
    set_pair(cpu, low, high, sum as u64);
}

/// SMMUL, SMMLA and SMMLS, the top word of a 64 bit product added to or
/// taken from the accumulator's word shifted up, rounded when round.
pub fn most_significant(cpu: &mut Cpu, d: usize, n: usize, m: usize, a: Option<usize>, subtract: bool, round: bool) {
    let product = (cpu.regs[n] as i32 as i64).wrapping_mul(cpu.regs[m] as i32 as i64);
    let accumulator = a.map_or(0, |a| (cpu.regs[a] as i64) << 32);
    let mut total = if subtract { accumulator.wrapping_sub(product) } else { accumulator.wrapping_add(product) };
    if round {
        total = total.wrapping_add(0x8000_0000);
    }
    cpu.regs[d] = (total >> 32) as u32;
}

/// QADD, QSUB, QDADD and QDSUB: m plus or minus n, n doubled first for the
/// two that double, every step saturating and setting Q when it does.
pub fn saturating(cpu: &mut Cpu, d: usize, m: usize, n: usize, subtract: bool, double: bool) {
    let a = cpu.regs[m] as i32 as i64;
    let (b, doubled) = if double {
        let (b, saturated) = saturate_i32(cpu.regs[n] as i32 as i64 * 2);
        (b as i32 as i64, saturated)
    } else {
        (cpu.regs[n] as i32 as i64, false)
    };
    let (result, saturated) = saturate_i32(if subtract { a - b } else { a + b });
    cpu.regs[d] = result;
    cpu.cpsr.q |= doubled || saturated;
}
