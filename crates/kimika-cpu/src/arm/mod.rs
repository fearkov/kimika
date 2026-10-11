//! ARM state, decoded the way the ARMv7 manual lays it out, chapter A5.

pub mod alu;
pub mod loadstore;
pub mod media;
pub mod misc;
pub mod multiply;
pub mod shifter;

use alu::Operation;
use shifter::{arm_expand_immediate, shift_by_immediate, shift_by_register, ShiftType};

use crate::bits::{bit, bits};
use crate::psr::check_condition;
use crate::{Bus, Cpu, Exit, PC};

pub fn execute<B: Bus>(cpu: &mut Cpu, bus: &mut B, op: u32) -> Option<Exit> {
    let cond = op >> 28;
    if cond == 0xF {
        return unconditional(cpu, bus, op);
    }
    if !check_condition(cond, &cpu.cpsr) {
        return None;
    }
    match bits(op, 25, 27) {
        0b000 | 0b001 => data_processing_and_misc(cpu, bus, op),
        0b010 => loadstore::single(cpu, bus, op),
        0b011 if bit(op, 4) => media::execute(cpu, op),
        0b011 => loadstore::single(cpu, bus, op),
        0b100 => loadstore::block(cpu, bus, op),
        0b101 => misc::branch(cpu, op),
        _ => coprocessor(cpu, bus, op),
    }
}

/// A5.2, data processing and everything that hides in its unused corners.
fn data_processing_and_misc<B: Bus>(cpu: &mut Cpu, bus: &mut B, op: u32) -> Option<Exit> {
    let immediate = bit(op, 25);
    let op1 = bits(op, 20, 24);
    let op2 = bits(op, 4, 7);
    // op1 10xx0, the compares with S clear, is where the rest lives
    let hole = op1 & 0b11001 == 0b10000;
    if immediate {
        return match op1 {
            0b10000 => move_wide(cpu, op, false),
            0b10100 => move_wide(cpu, op, true),
            0b10010 | 0b10110 => {
                let value = arm_expand_immediate(op & 0xFFF, cpu.cpsr.c).0;
                let mask = bits(op, 16, 19);
                // bits 15:12 are meant to be ones, and a hint's 11:8 zeros
                if bits(op, 12, 15) != 0xF {
                    return cpu.undefined(op);
                }
                if op1 == 0b10010 && mask == 0 {
                    if bits(op, 8, 11) != 0 {
                        return cpu.undefined(op);
                    }
                    misc::hint(op & 0xFF)
                } else {
                    misc::write_status(cpu, op, value, mask, bit(op, 22))
                }
            }
            _ => {
                let (operand, carry) = arm_expand_immediate(op & 0xFFF, cpu.cpsr.c);
                data_processing(cpu, op, operand, carry)
            }
        };
    }
    if op2 == 0b1001 {
        return if op1 & 0b10000 == 0 { multiply(cpu, op, op1) } else { loadstore::synchronization(cpu, bus, op) };
    }
    if op2 & 0b1001 == 0b1001 {
        return loadstore::extra(cpu, bus, op);
    }
    if hole {
        return if op2 & 0b1000 == 0 { miscellaneous(cpu, op) } else { halfword_multiply(cpu, op) };
    }
    let carry_in = cpu.cpsr.c;
    let m = (op & 0xF) as usize;
    let ty = ShiftType::from_bits(bits(op, 5, 6));
    let (operand, carry) = if bit(op, 4) {
        // shifted by a register, where none may be the PC
        let (n, d, s) = (bits(op, 16, 19) as usize, bits(op, 12, 15) as usize, bits(op, 8, 11) as usize);
        if [n, d, s, m].contains(&PC) {
            return cpu.undefined(op);
        }
        shift_by_register(ty, cpu.regs[m], cpu.regs[s], carry_in)
    } else {
        shift_by_immediate(ty, cpu.regs[m], bits(op, 7, 11), carry_in)
    };
    data_processing(cpu, op, operand, carry)
}

fn data_processing(cpu: &mut Cpu, op: u32, operand: u32, carry: bool) -> Option<Exit> {
    let operation = Operation::from_arm(bits(op, 21, 24));
    let set_flags = bit(op, 20);
    let d = bits(op, 12, 15) as usize;
    let n = bits(op, 16, 19) as usize;
    // flags with the PC as the destination is an exception return
    if set_flags && d == PC && !operation.is_compare() {
        return cpu.undefined(op);
    }
    // the compares have no destination and MOV and MVN no first operand,
    // and those fields are meant to be zero
    let unused = match operation {
        Operation::Tst | Operation::Teq | Operation::Cmp | Operation::Cmn => d,
        Operation::Mov | Operation::Mvn => n,
        _ => 0,
    };
    if unused != 0 {
        return cpu.undefined(op);
    }
    // ADR is ADD and SUB with the PC, whose base is word aligned
    let a = if n == PC { cpu.aligned_pc() } else { cpu.regs[n] };
    alu::data_processing(cpu, operation, set_flags, d, a, operand, carry);
    None
}

/// MOVW and MOVT.
fn move_wide(cpu: &mut Cpu, op: u32, top: bool) -> Option<Exit> {
    let d = bits(op, 12, 15) as usize;
    if d == PC {
        return cpu.undefined(op);
    }
    let value = bits(op, 16, 19) << 12 | (op & 0xFFF);
    cpu.regs[d] = if top { (cpu.regs[d] & 0xFFFF) | value << 16 } else { value };
    None
}

fn multiply(cpu: &mut Cpu, op: u32, op1: u32) -> Option<Exit> {
    // Rd or RdHi at 19:16, Ra or RdLo at 15:12, Rm at 11:8, Rn at 3:0
    let d = bits(op, 16, 19) as usize;
    let a = bits(op, 12, 15) as usize;
    let m = bits(op, 8, 11) as usize;
    let n = (op & 0xF) as usize;
    let set_flags = bit(op, 20);
    // none of them takes the PC, and MUL has no accumulator, its field meant
    // to be zero
    let plain = op1 >> 1 == 0;
    if [d, m, n].contains(&PC) || (plain && a != 0) || (!plain && a == PC) {
        return cpu.undefined(op);
    }
    match op1 >> 1 {
        0b000 => multiply::multiply(cpu, d, n, m, None, set_flags),
        0b001 => multiply::multiply(cpu, d, n, m, Some(a), set_flags),
        0b010 if !set_flags => multiply::umaal(cpu, a, d, n, m),
        0b011 if !set_flags => multiply::multiply_subtract(cpu, d, n, m, a),
        0b100..=0b111 => {
            if a == d {
                return cpu.undefined(op);
            }
            multiply::long(cpu, a, d, n, m, bit(op, 22), bit(op, 21), set_flags)
        }
        _ => return cpu.undefined(op),
    }
    None
}

fn halfword_multiply(cpu: &mut Cpu, op: u32) -> Option<Exit> {
    let d = bits(op, 16, 19) as usize;
    let a = bits(op, 12, 15) as usize;
    let m = bits(op, 8, 11) as usize;
    let n = (op & 0xF) as usize;
    let (top_n, top_m) = (bit(op, 5), bit(op, 6));
    // SMUL<x><y> and SMULW<y> have no accumulator, its field meant to be
    // zero, and SMLAL<x><y> needs two registers for its result
    let kind = bits(op, 21, 22);
    let plain = kind == 0b11 || (kind == 0b01 && top_n);
    if [d, m, n].contains(&PC) || (plain && a != 0) || (!plain && a == PC) || (kind == 0b10 && a == d) {
        return cpu.undefined(op);
    }
    match kind {
        0b00 => multiply::halfword(cpu, d, n, m, Some(a), top_n, top_m),
        // SMLAW<y> with bit 5 clear, SMULW<y> with it set
        0b01 if top_n => multiply::word_by_halfword(cpu, d, n, m, None, top_m),
        0b01 => multiply::word_by_halfword(cpu, d, n, m, Some(a), top_m),
        0b10 => multiply::halfword_long(cpu, a, d, n, m, top_n, top_m),
        _ => multiply::halfword(cpu, d, n, m, None, top_n, top_m),
    }
    None
}

/// A5.2.12, the status register, BX, CLZ, the saturating additions and
/// BKPT.
fn miscellaneous(cpu: &mut Cpu, op: u32) -> Option<Exit> {
    let d = bits(op, 12, 15) as usize;
    let m = (op & 0xF) as usize;
    let n = bits(op, 16, 19) as usize;
    // the fields each of these has no use for are meant to be ones or zeros
    let (ones, zeros) = (|lo: u32| bits(op, lo, lo + 3) == 0xF, |lo: u32| bits(op, lo, lo + 3) == 0);
    match (bits(op, 4, 6), bits(op, 21, 22)) {
        (0b000, 0b00 | 0b10) if ones(16) && zeros(8) && zeros(0) => misc::read_status(cpu, op, d, bit(op, 22)),
        (0b000, 0b01 | 0b11) if ones(12) && zeros(8) && m != PC => {
            misc::write_status(cpu, op, cpu.regs[m], bits(op, 16, 19), bit(op, 22))
        }
        (0b001, 0b01) | (0b010, 0b01) | (0b011, 0b01) if bits(op, 8, 19) == 0xFFF => misc::branch_exchange(cpu, op),
        (0b001, 0b11) if ones(16) && ones(8) => {
            if d == PC || m == PC {
                return cpu.undefined(op);
            }
            cpu.regs[d] = cpu.regs[m].leading_zeros();
            None
        }
        (0b101, kind) if zeros(8) => {
            if [d, n, m].contains(&PC) {
                return cpu.undefined(op);
            }
            multiply::saturating(cpu, d, m, n, kind & 1 != 0, kind & 2 != 0);
            None
        }
        // BKPT has no condition
        (0b111, 0b01) if op >> 28 == 0xE => misc::breakpoint(cpu, bits(op, 8, 19) << 4 | (op & 0xF)),
        _ => cpu.undefined(op),
    }
}

/// A5.6, svc and the coprocessors, of which the VFP and NEON registers are
/// 10 and 11 and CP15 the only other one user mode can touch.
fn coprocessor<B: Bus>(cpu: &mut Cpu, bus: &mut B, op: u32) -> Option<Exit> {
    if bits(op, 24, 25) == 0b11 {
        return misc::supervisor_call(op & 0x00FF_FFFF);
    }
    let vfp = bits(op, 9, 11) == 0b101;
    match bits(op, 24, 25) {
        // loads and stores, and with bits 23:21 0b010 the transfers of two
        // registers
        0b00 | 0b01 if vfp => {
            if bits(op, 21, 24) == 0b0010 {
                crate::vfp::transfer_pair(cpu, op)
            } else if bits(op, 21, 24) == 0 {
                cpu.undefined(op)
            } else {
                crate::vfp::load_store(cpu, bus, op)
            }
        }
        0b10 if bit(op, 4) => misc::coprocessor_register(cpu, bus, op),
        0b10 if vfp => crate::vfp::data_processing(cpu, op),
        _ => cpu.undefined(op),
    }
}

/// A5.7, what cond 0b1111 encodes: NEON, the barriers, CLREX, the preload
/// hints and BLX with an immediate.
fn unconditional<B: Bus>(cpu: &mut Cpu, bus: &mut B, op: u32) -> Option<Exit> {
    if op & 0xFE00_0000 == 0xFA00_0000 {
        return misc::branch_link_exchange_immediate(cpu, op);
    }
    match bits(op, 20, 27) {
        // NEON data processing, 1111 001x
        0x20..=0x3F => crate::neon::data_processing(cpu, op),
        // NEON loads and stores, 1111 0100 xxx0
        0x40..=0x4F if !bit(op, 20) => crate::neon::load_store(cpu, bus, op),
        // CLREX, DSB, DMB and ISB
        0x57 => match bits(op, 4, 7) {
            0b0001 => {
                cpu.exclusive = None;
                None
            }
            0b0100..=0b0110 => {
                bus.barrier();
                None
            }
            _ => cpu.undefined(op),
        },
        // PLI and PLD, nothing to do
        0x41 | 0x45 | 0x49 | 0x4D | 0x51 | 0x55 | 0x59 | 0x5D | 0x61 | 0x65 | 0x69 | 0x6D | 0x71 | 0x75 | 0x79
        | 0x7D => None,
        // SETEND LE, which is what the Vita runs in, and CPS, which user mode
        // cannot change anything with
        0x10 if bit(op, 16) && bits(op, 4, 7) == 0 && !bit(op, 9) => None,
        0x10 if !bit(op, 16) => None,
        _ => cpu.undefined(op),
    }
}
