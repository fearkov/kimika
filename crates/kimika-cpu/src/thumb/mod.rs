//! Thumb state, decoded the way the ARMv7 manual lays it out, chapter A6:
//! the 16-bit instructions here, Thumb-2's 32-bit ones in wide.
//!
//! inside an IT block the 16-bit instructions that would set flags don't,
//! and the conditions come from the block, which the fetch loop checks.

mod wide;

use crate::arm::alu::{self, Operation};
use crate::arm::loadstore::{load, load_multiple, load_register, store, store_multiple};
use crate::arm::media::{self, Extension};
use crate::arm::misc;
use crate::arm::multiply;
use crate::arm::shifter::{shift_by_immediate, shift_by_register, ShiftType};
use crate::bits::{bit, bits, sign_extend};
use crate::psr::check_condition;
use crate::{Bus, Cpu, Exit, LR, PC, SP};

pub fn execute<B: Bus>(cpu: &mut Cpu, bus: &mut B, op: u32, is_wide: bool) -> Option<Exit> {
    if is_wide {
        return wide::execute(cpu, bus, op);
    }
    match bits(op, 10, 15) {
        0b000000..=0b001111 => shift_add_subtract_move_compare(cpu, op),
        0b010000 => data_processing(cpu, op),
        0b010001 => special_data_and_branch(cpu, op),
        0b010010 | 0b010011 => {
            // LDR from a literal, word aligned from the PC
            let address = cpu.aligned_pc().wrapping_add((op & 0xFF) * 4);
            cpu.regs[bits(op, 8, 10) as usize] = bus.read32(address);
            None
        }
        0b010100..=0b100111 => load_store_single(cpu, bus, op),
        0b101000 | 0b101001 => {
            // ADR
            cpu.regs[bits(op, 8, 10) as usize] = cpu.aligned_pc().wrapping_add((op & 0xFF) * 4);
            None
        }
        0b101010 | 0b101011 => {
            // ADD from SP
            cpu.regs[bits(op, 8, 10) as usize] = cpu.regs[SP].wrapping_add((op & 0xFF) * 4);
            None
        }
        0b101100..=0b101111 => miscellaneous(cpu, bus, op),
        0b110000 | 0b110001 => {
            // STM, always writing back
            let n = bits(op, 8, 10) as usize;
            let list = op & 0xFF;
            let written = cpu.regs[n].wrapping_add(list.count_ones() * 4);
            if store_multiple(cpu, bus, list, cpu.regs[n], Some((n, written))).is_none() {
                return cpu.undefined(op);
            }
            cpu.regs[n] = written;
            None
        }
        0b110010 | 0b110011 => {
            // LDM, writing back unless the base is loaded
            let n = bits(op, 8, 10) as usize;
            let list = op & 0xFF;
            let base = cpu.regs[n];
            if list & (1 << n) == 0 {
                cpu.regs[n] = base.wrapping_add(list.count_ones() * 4);
            }
            load_multiple(cpu, bus, list, base, false).map_or_else(|| cpu.undefined(op), |()| None)
        }
        0b110100..=0b110111 => conditional_branch_and_svc(cpu, op),
        0b111000 | 0b111001 => {
            // B, which may only end an IT block
            if cpu.cpsr.in_it_block() && !last_in_it_block(cpu) {
                return cpu.undefined(op);
            }
            let offset = sign_extend(op & 0x7FF, 11) << 1;
            cpu.branch(cpu.regs[PC].wrapping_add(offset as u32));
            None
        }
        _ => cpu.undefined(op),
    }
}

/// whether the instruction running is the last of its IT block.
fn last_in_it_block(cpu: &Cpu) -> bool {
    cpu.cpsr.it & 0xF == 0b1000
}

/// the 16-bit instructions that set flags do it only outside IT blocks.
fn sets_flags(cpu: &Cpu) -> bool {
    !cpu.cpsr.in_it_block()
}

/// A6.2.1
fn shift_add_subtract_move_compare(cpu: &mut Cpu, op: u32) -> Option<Exit> {
    let set_flags = sets_flags(cpu);
    let d = (op & 7) as usize;
    let m = bits(op, 3, 5) as usize;
    match bits(op, 9, 13) {
        // LSL, LSR and ASR by an immediate, LSL #0 being MOVS
        0b00000..=0b01011 => {
            let ty = ShiftType::from_bits(bits(op, 11, 12));
            let (result, carry) = shift_by_immediate(ty, cpu.regs[m], bits(op, 6, 10), cpu.cpsr.c);
            alu::data_processing(cpu, Operation::Mov, set_flags, d, 0, result, carry);
        }
        0b01100..=0b01111 => {
            let operand = if bit(op, 10) { bits(op, 6, 8) } else { cpu.regs[bits(op, 6, 8) as usize] };
            let operation = if bit(op, 9) { Operation::Sub } else { Operation::Add };
            alu::data_processing(cpu, operation, set_flags, d, cpu.regs[m], operand, cpu.cpsr.c);
        }
        opcode => {
            let n = bits(op, 8, 10) as usize;
            let imm = op & 0xFF;
            let carry = cpu.cpsr.c;
            match opcode >> 2 {
                0b100 => alu::data_processing(cpu, Operation::Mov, set_flags, n, 0, imm, carry),
                0b101 => alu::data_processing(cpu, Operation::Cmp, true, n, cpu.regs[n], imm, carry),
                0b110 => alu::data_processing(cpu, Operation::Add, set_flags, n, cpu.regs[n], imm, carry),
                _ => alu::data_processing(cpu, Operation::Sub, set_flags, n, cpu.regs[n], imm, carry),
            }
        }
    }
    None
}

/// A6.2.2, the low register data processing, Rdn at 2:0 and Rm at 5:3.
fn data_processing(cpu: &mut Cpu, op: u32) -> Option<Exit> {
    let set_flags = sets_flags(cpu);
    let d = (op & 7) as usize;
    let (a, b) = (cpu.regs[d], cpu.regs[bits(op, 3, 5) as usize]);
    let carry = cpu.cpsr.c;
    let shift = |cpu: &mut Cpu, ty: ShiftType| {
        let (result, carry) = shift_by_register(ty, a, b, carry);
        alu::data_processing(cpu, Operation::Mov, set_flags, d, 0, result, carry);
    };
    match bits(op, 6, 9) {
        0b0000 => alu::data_processing(cpu, Operation::And, set_flags, d, a, b, carry),
        0b0001 => alu::data_processing(cpu, Operation::Eor, set_flags, d, a, b, carry),
        0b0010 => shift(cpu, ShiftType::Lsl),
        0b0011 => shift(cpu, ShiftType::Lsr),
        0b0100 => shift(cpu, ShiftType::Asr),
        0b0101 => alu::data_processing(cpu, Operation::Adc, set_flags, d, a, b, carry),
        0b0110 => alu::data_processing(cpu, Operation::Sbc, set_flags, d, a, b, carry),
        0b0111 => shift(cpu, ShiftType::Ror),
        0b1000 => alu::data_processing(cpu, Operation::Tst, true, d, a, b, carry),
        // RSB from zero, NEG
        0b1001 => alu::data_processing(cpu, Operation::Rsb, set_flags, d, b, 0, carry),
        0b1010 => alu::data_processing(cpu, Operation::Cmp, true, d, a, b, carry),
        0b1011 => alu::data_processing(cpu, Operation::Cmn, true, d, a, b, carry),
        0b1100 => alu::data_processing(cpu, Operation::Orr, set_flags, d, a, b, carry),
        0b1101 => {
            let n = bits(op, 3, 5) as usize;
            multiply::multiply(cpu, d, n, d, None, set_flags);
        }
        0b1110 => alu::data_processing(cpu, Operation::Bic, set_flags, d, a, b, carry),
        _ => alu::data_processing(cpu, Operation::Mvn, set_flags, d, a, b, carry),
    }
    None
}

/// A6.2.3, ADD, CMP and MOV with the high registers, and BX and BLX.
fn special_data_and_branch(cpu: &mut Cpu, op: u32) -> Option<Exit> {
    let d = ((op >> 4) & 8 | op & 7) as usize;
    let m = bits(op, 3, 6) as usize;
    // the PC reads as the instruction plus 4 here
    let value = |cpu: &Cpu, r: usize| if r == PC { cpu.regs[PC] } else { cpu.regs[r] };
    match bits(op, 6, 9) {
        0b0000..=0b0011 => {
            let sum = value(cpu, d).wrapping_add(value(cpu, m));
            cpu.set_reg(d, sum);
        }
        0b0101..=0b0111 => {
            if d < 8 && m < 8 || d == PC || m == PC {
                return cpu.undefined(op);
            }
            alu::data_processing(cpu, Operation::Cmp, true, d, cpu.regs[d], cpu.regs[m], cpu.cpsr.c);
        }
        0b1000..=0b1011 => {
            let source = value(cpu, m);
            cpu.set_reg(d, source);
        }
        0b1100..=0b1111 => {
            if op & 7 != 0 || (cpu.cpsr.in_it_block() && !last_in_it_block(cpu)) {
                return cpu.undefined(op);
            }
            let target = value(cpu, m);
            if bit(op, 7) {
                if m == PC {
                    return cpu.undefined(op);
                }
                cpu.regs[LR] = cpu.regs[PC].wrapping_sub(2) | 1;
            }
            cpu.branch_exchange(target);
        }
        _ => return cpu.undefined(op),
    }
    None
}

/// A6.2.4, the loads and stores of one register.
fn load_store_single<B: Bus>(cpu: &mut Cpu, bus: &mut B, op: u32) -> Option<Exit> {
    let t = (op & 7) as usize;
    let n = bits(op, 3, 5) as usize;
    let imm5 = bits(op, 6, 10);
    let (address, size, signed, loading) = match bits(op, 12, 15) {
        0b0101 => {
            let address = cpu.regs[n].wrapping_add(cpu.regs[bits(op, 6, 8) as usize]);
            let (size, signed, loading) = match bits(op, 9, 11) {
                0b000 => (4, false, false),
                0b001 => (2, false, false),
                0b010 => (1, false, false),
                0b011 => (1, true, true),
                0b100 => (4, false, true),
                0b101 => (2, false, true),
                0b110 => (1, false, true),
                _ => (2, true, true),
            };
            (address, size, signed, loading)
        }
        0b0110 => (cpu.regs[n].wrapping_add(imm5 * 4), 4, false, bit(op, 11)),
        0b0111 => (cpu.regs[n].wrapping_add(imm5), 1, false, bit(op, 11)),
        0b1000 => (cpu.regs[n].wrapping_add(imm5 * 2), 2, false, bit(op, 11)),
        _ => {
            // SP relative, Rt at 10:8
            let t = bits(op, 8, 10) as usize;
            let address = cpu.regs[SP].wrapping_add((op & 0xFF) * 4);
            if bit(op, 11) {
                cpu.regs[t] = bus.read32(address);
            } else {
                bus.write32(address, cpu.regs[t]);
            }
            return None;
        }
    };
    if loading {
        cpu.regs[t] = load(bus, address, size, signed);
    } else {
        store(bus, address, size, cpu.regs[t]);
    }
    None
}

/// A6.2.5
fn miscellaneous<B: Bus>(cpu: &mut Cpu, bus: &mut B, op: u32) -> Option<Exit> {
    let d = (op & 7) as usize;
    let m = bits(op, 3, 5) as usize;
    match bits(op, 5, 11) {
        0b0000000..=0b0000011 => cpu.regs[SP] = cpu.regs[SP].wrapping_add((op & 0x7F) * 4),
        0b0000100..=0b0000111 => cpu.regs[SP] = cpu.regs[SP].wrapping_sub((op & 0x7F) * 4),
        0b0001000..=0b0001111 | 0b0011000..=0b0011111 | 0b1001000..=0b1001111 | 0b1011000..=0b1011111 => {
            // CBZ and CBNZ, which can't be in an IT block
            if cpu.cpsr.in_it_block() {
                return cpu.undefined(op);
            }
            let zero = cpu.regs[d] == 0;
            if zero != bit(op, 11) {
                let offset = bit(op, 9) as u32 * 64 + bits(op, 3, 7) * 2;
                cpu.branch(cpu.regs[PC].wrapping_add(offset));
            }
        }
        0b0010000 | 0b0010001 => media::extend(cpu, d, None, m, 0, Extension::SignedHalfword),
        0b0010010 | 0b0010011 => media::extend(cpu, d, None, m, 0, Extension::SignedByte),
        0b0010100 | 0b0010101 => media::extend(cpu, d, None, m, 0, Extension::UnsignedHalfword),
        0b0010110 | 0b0010111 => media::extend(cpu, d, None, m, 0, Extension::UnsignedByte),
        0b0100000..=0b0101111 => {
            // PUSH, with LR when bit 8 is set
            let list = (op & 0xFF) | (bit(op, 8) as u32) << LR;
            let lowest = cpu.regs[SP].wrapping_sub(list.count_ones() * 4);
            if store_multiple(cpu, bus, list, lowest, None).is_none() {
                return cpu.undefined(op);
            }
            cpu.regs[SP] = lowest;
        }
        // CPS, which user mode can't change anything with
        0b0110011 => {}
        0b1010000 | 0b1010001 => cpu.regs[d] = media::reverse(cpu.regs[m], 0),
        0b1010010 | 0b1010011 => cpu.regs[d] = media::reverse(cpu.regs[m], 1),
        0b1010110 | 0b1010111 => cpu.regs[d] = media::reverse(cpu.regs[m], 2),
        0b1100000..=0b1101111 => {
            // POP, with PC when bit 8 is set, which may only end an IT block
            let list = (op & 0xFF) | (bit(op, 8) as u32) << PC;
            if bit(op, 8) && cpu.cpsr.in_it_block() && !last_in_it_block(cpu) {
                return cpu.undefined(op);
            }
            let base = cpu.regs[SP];
            cpu.regs[SP] = base.wrapping_add(list.count_ones() * 4);
            if load_multiple(cpu, bus, list, base, false).is_none() {
                return cpu.undefined(op);
            }
        }
        0b1110000..=0b1110111 => return misc::breakpoint(cpu, op & 0xFF),
        0b1111000..=0b1111111 => {
            if op & 0xF == 0 {
                return misc::hint(bits(op, 4, 7));
            }
            return if_then(cpu, op);
        }
        _ => return cpu.undefined(op),
    }
    None
}

/// IT, which puts its condition and mask in ITSTATE for the instructions
/// after it.
fn if_then(cpu: &mut Cpu, op: u32) -> Option<Exit> {
    let condition = bits(op, 4, 7);
    let mask = op & 0xF;
    // IT inside a block, with the never condition, or with an always
    // condition that has an else is unpredictable
    if cpu.cpsr.in_it_block() || condition == 0xF || (condition == 0xE && mask.count_ones() != 1) {
        return cpu.undefined(op);
    }
    cpu.cpsr.it = (op & 0xFF) as u8;
    None
}

/// A6.2.6
fn conditional_branch_and_svc(cpu: &mut Cpu, op: u32) -> Option<Exit> {
    match bits(op, 8, 11) {
        // UDF, permanently undefined
        0b1110 => cpu.undefined(op),
        0b1111 => misc::supervisor_call(op & 0xFF),
        condition => {
            if cpu.cpsr.in_it_block() {
                return cpu.undefined(op);
            }
            if check_condition(condition, &cpu.cpsr) {
                let offset = sign_extend(op & 0xFF, 8) << 1;
                cpu.branch(cpu.regs[PC].wrapping_add(offset as u32));
            }
            None
        }
    }
}

/// the load of a register by Thumb-2, where the PC is an interworking
/// branch that may only end an IT block.
fn load_into<B: Bus>(
    cpu: &mut Cpu,
    bus: &mut B,
    op: u32,
    t: usize,
    address: u32,
    size: u32,
    signed: bool,
) -> Option<Exit> {
    if t == PC && cpu.cpsr.in_it_block() && !last_in_it_block(cpu) {
        return cpu.undefined(op);
    }
    let value = load(bus, address, size, signed);
    load_register(cpu, t, value);
    None
}
