//! Thumb-2's 32-bit instructions, A6.3. the opcode has the first halfword
//! on top, so its bit numbers are the manual's, hw1 being bits 31:16.

use super::{last_in_it_block, load_into};
use crate::arm::alu::{self, Operation};
use crate::arm::loadstore::{load_exclusive, load_multiple, store, store_exclusive, store_multiple};
use crate::arm::media::{self, Extension, Flavor, Lanes};
use crate::arm::misc;
use crate::arm::multiply;
use crate::arm::shifter::{shift_by_immediate, shift_by_register, thumb_expand_immediate, ShiftType};
use crate::bits::{bit, bits, sign_extend};
use crate::psr::check_condition;
use crate::{Bus, Cpu, Exit, LR, PC, SP};

pub fn execute<B: Bus>(cpu: &mut Cpu, bus: &mut B, op: u32) -> Option<Exit> {
    let first = op >> 16;
    let op2 = bits(first, 4, 10);
    match bits(first, 11, 12) {
        0b01 if op2 & 0b1100100 == 0 => load_store_multiple(cpu, bus, op),
        0b01 if op2 & 0b1100100 == 0b0000100 => load_store_dual_exclusive_table(cpu, bus, op),
        0b01 if op2 & 0b1100000 == 0b0100000 => data_processing_shifted(cpu, op),
        0b01 => coprocessor(cpu, bus, op),
        0b10 if bit(op, 15) => branches_and_control(cpu, bus, op),
        0b10 if !bit(op2, 5) => data_processing_modified_immediate(cpu, op),
        0b10 => data_processing_plain_immediate(cpu, op),
        _ => match op2 {
            0b0000000..=0b0001111 if op2 & 0b1110001 == 0 => single(cpu, bus, op, false),
            0b0000000..=0b0011111 if op2 & 0b111 == 0b111 => cpu.undefined(op),
            0b0000000..=0b0011111 if op2 & 1 == 1 => single(cpu, bus, op, true),
            0b0010000..=0b0011111 if op2 & 1 == 0 => {
                // NEON element and structure loads and stores, 1111 1001 xxx0,
                // ARM's 1111 0100 xxx0
                crate::neon::load_store(cpu, bus, 0xF400_0000 | (op & 0x00FF_FFFF))
            }
            0b0100000..=0b0101111 => data_processing_register(cpu, op),
            0b0110000..=0b0110111 => multiply(cpu, op),
            0b0111000..=0b0111111 => long_multiply(cpu, op),
            0b1000000..=0b1111111 => coprocessor(cpu, bus, op),
            _ => cpu.undefined(op),
        },
    }
}

/// the registers in hw1's bits 3:0, hw2's 15:12, 11:8 and 3:0, the n, t,
/// d and m most instructions name.
fn registers(op: u32) -> (usize, usize, usize, usize) {
    (bits(op, 16, 19) as usize, bits(op, 12, 15) as usize, bits(op, 8, 11) as usize, (op & 0xF) as usize)
}

/// the shift amount spread over imm3 at 14:12 and imm2 at 7:6.
fn imm3_imm2(op: u32) -> u32 {
    bits(op, 12, 14) << 2 | bits(op, 6, 7)
}

/// A6.3.5, LDM, STM, PUSH and POP.
fn load_store_multiple<B: Bus>(cpu: &mut Cpu, bus: &mut B, op: u32) -> Option<Exit> {
    let n = bits(op, 16, 19) as usize;
    let list = op & 0xFFFF;
    let writeback = bit(op, 21);
    let loading = bit(op, 20);
    let increment = match bits(op, 23, 24) {
        0b01 => true,
        0b10 => false,
        // SRS and RFE
        _ => return cpu.undefined(op),
    };
    let unpredictable = n == PC
        || list.count_ones() < 2
        || list & (1 << SP) != 0
        || (writeback && list & (1 << n) != 0)
        || if loading {
            list & (1 << PC) != 0 && (list & (1 << LR) != 0 || (cpu.cpsr.in_it_block() && !last_in_it_block(cpu)))
        } else {
            list & (1 << PC) != 0
        };
    if unpredictable {
        return cpu.undefined(op);
    }
    let base = cpu.regs[n];
    let span = list.count_ones() * 4;
    let (lowest, written) =
        if increment { (base, base.wrapping_add(span)) } else { (base.wrapping_sub(span), base.wrapping_sub(span)) };
    // the checks above leave both of these nothing to refuse
    if loading {
        if writeback {
            cpu.regs[n] = written;
        }
        let _ = load_multiple(cpu, bus, list, lowest, false);
    } else {
        let _ = store_multiple(cpu, bus, list, lowest, None);
        if writeback {
            cpu.regs[n] = written;
        }
    }
    None
}

/// A6.3.6, LDRD, STRD, the exclusives and the table branches.
fn load_store_dual_exclusive_table<B: Bus>(cpu: &mut Cpu, bus: &mut B, op: u32) -> Option<Exit> {
    let (n, t, t2, m) = registers(op);
    let op1 = bits(op, 23, 24);
    let op2 = bits(op, 20, 21);
    let imm8 = (op & 0xFF) * 4;
    match (op1, op2) {
        // STREX, its status in Rd at 11:8
        (0b00, 0b00) => {
            let d = t2;
            if [d, t].iter().any(|&r| r == PC || r == SP) || n == PC || d == n || d == t {
                return cpu.undefined(op);
            }
            let address = cpu.regs[n].wrapping_add(imm8);
            cpu.regs[d] = store_exclusive(cpu, bus, address, 4, cpu.regs[t] as u64);
        }
        (0b00, 0b01) => {
            if t == PC || t == SP || n == PC {
                return cpu.undefined(op);
            }
            let address = cpu.regs[n].wrapping_add(imm8);
            cpu.regs[t] = load_exclusive(cpu, bus, address, 4) as u32;
        }
        (0b01, 0b00) => {
            let d = m;
            let size = match bits(op, 4, 7) {
                0b0100 => 1,
                0b0101 => 2,
                0b0111 => 8,
                _ => return cpu.undefined(op),
            };
            let pair = size == 8 && (t2 == PC || t2 == SP || d == t2);
            if d == PC || t == PC || n == PC || d == n || d == t || pair {
                return cpu.undefined(op);
            }
            let value = if size == 8 { cpu.regs[t] as u64 | (cpu.regs[t2] as u64) << 32 } else { cpu.regs[t] as u64 };
            cpu.regs[d] = store_exclusive(cpu, bus, cpu.regs[n], size, value);
        }
        (0b01, 0b01) => match bits(op, 4, 7) {
            // TBB and TBH, which may only end an IT block
            kind @ (0b0000 | 0b0001) => {
                if m == SP || m == PC || (cpu.cpsr.in_it_block() && !last_in_it_block(cpu)) {
                    return cpu.undefined(op);
                }
                let base = cpu.regs[n];
                let halfwords = if kind == 0 {
                    bus.read8(base.wrapping_add(cpu.regs[m])) as u32
                } else {
                    bus.read16(base.wrapping_add(cpu.regs[m] << 1)) as u32
                };
                cpu.branch(cpu.regs[PC].wrapping_add(halfwords * 2));
            }
            kind @ (0b0100 | 0b0101 | 0b0111) => {
                let size = match kind {
                    0b0100 => 1,
                    0b0101 => 2,
                    _ => 8,
                };
                if t == PC || t == SP || n == PC || (size == 8 && (t2 == PC || t2 == SP || t == t2)) {
                    return cpu.undefined(op);
                }
                let value = load_exclusive(cpu, bus, cpu.regs[n], size);
                cpu.regs[t] = value as u32;
                if size == 8 {
                    cpu.regs[t2] = (value >> 32) as u32;
                }
            }
            _ => return cpu.undefined(op),
        },
        // LDRD and STRD with an immediate, P at 24, U at 23 and W at 21
        _ => {
            let (index, add, writeback, loading) = (bit(op, 24), bit(op, 23), bit(op, 21), bit(op, 20));
            let bad = |r: usize| r == PC || r == SP;
            if bad(t) || bad(t2) || (loading && t == t2) || (writeback && (n == PC || n == t || n == t2)) {
                return cpu.undefined(op);
            }
            if !loading && n == PC {
                return cpu.undefined(op);
            }
            let base = if n == PC { cpu.aligned_pc() } else { cpu.regs[n] };
            let offset_address = if add { base.wrapping_add(imm8) } else { base.wrapping_sub(imm8) };
            let address = if index { offset_address } else { base };
            if loading {
                let (low, high) = (bus.read32(address), bus.read32(address.wrapping_add(4)));
                cpu.regs[t] = low;
                cpu.regs[t2] = high;
            } else {
                bus.write32(address, cpu.regs[t]);
                bus.write32(address.wrapping_add(4), cpu.regs[t2]);
            }
            if writeback {
                cpu.regs[n] = offset_address;
            }
        }
    }
    None
}

/// the sixteen operations in Thumb-2's numbering, from hw1's bits 8:5, with
/// Rn or Rd as 1111 turning some into others: MOV and MVN without an Rn,
/// the comparisons without an Rd.
fn operation(op: u32) -> Option<Operation> {
    let (n, d) = (bits(op, 16, 19), bits(op, 8, 11));
    let s = bit(op, 20);
    Some(match bits(op, 21, 24) {
        0b0000 if d == 15 && s => Operation::Tst,
        0b0000 => Operation::And,
        0b0001 => Operation::Bic,
        0b0010 if n == 15 => Operation::Mov,
        0b0010 => Operation::Orr,
        0b0011 if n == 15 => Operation::Mvn,
        0b0011 => Operation::Orn,
        0b0100 if d == 15 && s => Operation::Teq,
        0b0100 => Operation::Eor,
        0b1000 if d == 15 && s => Operation::Cmn,
        0b1000 => Operation::Add,
        0b1010 => Operation::Adc,
        0b1011 => Operation::Sbc,
        0b1101 if d == 15 && s => Operation::Cmp,
        0b1101 => Operation::Sub,
        0b1110 => Operation::Rsb,
        _ => return None,
    })
}

fn data_processing(cpu: &mut Cpu, op: u32, operation: Operation, operand: u32, carry: bool) -> Option<Exit> {
    let (n, _, d, _) = registers(op);
    // the PC is never a destination here, nor an operand but for ADR's
    if (!operation.is_compare() && d == PC) || (n == PC && !matches!(operation, Operation::Mov | Operation::Mvn)) {
        return cpu.undefined(op);
    }
    alu::data_processing(cpu, operation, bit(op, 20), d, cpu.regs[n], operand, carry);
    None
}

/// A6.3.11
fn data_processing_shifted(cpu: &mut Cpu, op: u32) -> Option<Exit> {
    let (n, _, d, m) = registers(op);
    if m == PC {
        return cpu.undefined(op);
    }
    let ty = ShiftType::from_bits(bits(op, 4, 5));
    let amount = imm3_imm2(op);
    if bits(op, 21, 24) == 0b0110 {
        // PKHBT and PKHTB
        if bit(op, 20) || bit(op, 4) || [n, d].contains(&PC) {
            return cpu.undefined(op);
        }
        media::pack(cpu, d, n, m, bit(op, 5), amount);
        return None;
    }
    let Some(operation) = operation(op) else { return cpu.undefined(op) };
    let (operand, carry) = shift_by_immediate(ty, cpu.regs[m], amount, cpu.cpsr.c);
    data_processing(cpu, op, operation, operand, carry)
}

/// A6.3.1
fn data_processing_modified_immediate(cpu: &mut Cpu, op: u32) -> Option<Exit> {
    let imm12 = (bit(op, 26) as u32) << 11 | bits(op, 12, 14) << 8 | (op & 0xFF);
    let Some((operand, carry)) = thumb_expand_immediate(imm12, cpu.cpsr.c) else { return cpu.undefined(op) };
    let Some(operation) = operation(op) else { return cpu.undefined(op) };
    data_processing(cpu, op, operation, operand, carry)
}

/// A6.3.3, ADDW, SUBW, MOVW, MOVT, the saturations and the bit fields.
fn data_processing_plain_immediate(cpu: &mut Cpu, op: u32) -> Option<Exit> {
    let (n, _, d, _) = registers(op);
    if d == PC {
        return cpu.undefined(op);
    }
    let imm12 = (bit(op, 26) as u32) << 11 | bits(op, 12, 14) << 8 | (op & 0xFF);
    let shift = imm3_imm2(op);
    let field = op & 0x1F;
    match bits(op, 20, 24) {
        // ADDW and SUBW, ADR from the PC
        kind @ (0b00000 | 0b01010) => {
            let base = if n == PC { cpu.aligned_pc() } else { cpu.regs[n] };
            cpu.regs[d] = if kind == 0 { base.wrapping_add(imm12) } else { base.wrapping_sub(imm12) };
        }
        kind @ (0b00100 | 0b01100) => {
            let value = bits(op, 16, 19) << 12 | imm12;
            cpu.regs[d] = if kind == 0b00100 { value } else { (cpu.regs[d] & 0xFFFF) | value << 16 };
        }
        // SSAT and USAT, the ones without a shift with the ASR bit being the
        // halfword forms
        kind @ (0b10000 | 0b10010 | 0b11000 | 0b11010) => {
            if n == PC {
                return cpu.undefined(op);
            }
            let unsigned = kind >= 0b11000;
            if kind & 0b10 != 0 && shift == 0 {
                let width = (op & 0xF) + if unsigned { 0 } else { 1 };
                media::saturate16(cpu, d, n, width, unsigned);
            } else {
                let ty = if kind & 0b10 != 0 { ShiftType::Asr } else { ShiftType::Lsl };
                media::saturate(cpu, d, n, field + if unsigned { 0 } else { 1 }, ty, shift, unsigned);
            }
        }
        kind @ (0b10100 | 0b11100) => {
            if n == PC || shift + field + 1 > 32 {
                return cpu.undefined(op);
            }
            media::extract(cpu, d, n, shift, field + 1, kind == 0b10100);
        }
        0b10110 => {
            let source = (n != PC).then_some(n);
            if media::insert(cpu, d, source, shift, field).is_none() {
                return cpu.undefined(op);
            }
        }
        _ => return cpu.undefined(op),
    }
    None
}

/// A6.3.4
fn branches_and_control<B: Bus>(cpu: &mut Cpu, bus: &mut B, op: u32) -> Option<Exit> {
    let op1 = bits(op, 12, 14);
    let kind = bits(op, 20, 26);
    let sign = bit(op, 26) as u32;
    let (j1, j2) = (bit(op, 13) as u32, bit(op, 11) as u32);
    // B, BL and BLX may only end an IT block
    let ends_block = !cpu.cpsr.in_it_block() || last_in_it_block(cpu);
    match op1 {
        0b000 | 0b010 if kind & 0b0111000 != 0b0111000 => {
            // B with a condition, which an IT block cannot have
            if cpu.cpsr.in_it_block() {
                return cpu.undefined(op);
            }
            let imm = sign << 20 | j2 << 19 | j1 << 18 | bits(op, 16, 21) << 12 | (op & 0x7FF) << 1;
            if check_condition(bits(op, 22, 25), &cpu.cpsr) {
                cpu.branch(cpu.regs[PC].wrapping_add(sign_extend(imm, 21) as u32));
            }
            None
        }
        0b000 | 0b010 => match kind {
            0b0111000 | 0b0111001 => {
                let n = bits(op, 16, 19) as usize;
                // the mask's bits 11:10 are nzcvq and g, which the ARM form has
                // as bits 3 and 2
                let mask = bits(op, 10, 11) << 2;
                if n == PC || n == SP || bits(op, 8, 9) != 0 {
                    return cpu.undefined(op);
                }
                misc::write_status(cpu, op, cpu.regs[n], mask, bit(op, 20))
            }
            // the hints, and CPS, which user mode cannot change anything with
            0b0111010 if bits(op, 8, 10) == 0 => misc::hint(op & 0xFF),
            0b0111010 => None,
            0b0111011 => match bits(op, 4, 7) {
                0b0010 => {
                    cpu.exclusive = None;
                    None
                }
                0b0100..=0b0110 => {
                    bus.barrier();
                    None
                }
                _ => cpu.undefined(op),
            },
            // BXJ, BX on a core without Jazelle
            0b0111100 => {
                let m = bits(op, 16, 19) as usize;
                if m == PC || m == SP || !ends_block {
                    return cpu.undefined(op);
                }
                cpu.branch_exchange(cpu.regs[m]);
                None
            }
            0b0111110 | 0b0111111 => {
                let d = bits(op, 8, 11) as usize;
                if d == SP {
                    return cpu.undefined(op);
                }
                misc::read_status(cpu, op, d, bit(op, 20))
            }
            // SUBS PC, LR, SMC and UDF
            _ => cpu.undefined(op),
        },
        _ => {
            if !ends_block {
                return cpu.undefined(op);
            }
            let i1 = !(j1 ^ sign) & 1;
            let i2 = !(j2 ^ sign) & 1;
            let imm = sign << 24 | i1 << 23 | i2 << 22 | bits(op, 16, 25) << 12 | (op & 0x7FF) << 1;
            let offset = sign_extend(imm, 25) as u32;
            match op1 {
                // B
                0b001 | 0b011 => cpu.branch(cpu.regs[PC].wrapping_add(offset)),
                // BL
                0b101 | 0b111 => {
                    cpu.regs[LR] = cpu.regs[PC] | 1;
                    cpu.branch(cpu.regs[PC].wrapping_add(offset));
                }
                // BLX to ARM, whose target is word aligned, as H must be 0
                _ => {
                    if op & 1 != 0 {
                        return cpu.undefined(op);
                    }
                    cpu.regs[LR] = cpu.regs[PC] | 1;
                    cpu.branch_exchange(cpu.aligned_pc().wrapping_add(offset) & !3);
                }
            }
            None
        }
    }
}

/// A6.3.7 to A6.3.10, the loads and stores of one register: hw1's bits 6:5
/// give the size and bit 8 the sign, bit 7 the 12-bit offset forms, Rn of
/// 1111 the literals, and hw2 the 8-bit offsets with P, U and W, or a
/// shifted register.
fn single<B: Bus>(cpu: &mut Cpu, bus: &mut B, op: u32, loading: bool) -> Option<Exit> {
    let (n, t, _, m) = registers(op);
    let size = 1 << bits(op, 21, 22);
    let signed = loading && bit(op, 24);
    if size == 8 || (signed && size == 4) || (!loading && bit(op, 24)) {
        return cpu.undefined(op);
    }
    let mut writeback = None;
    let address = if n == PC {
        if !loading {
            return cpu.undefined(op);
        }
        let imm12 = op & 0xFFF;
        if bit(op, 23) {
            cpu.aligned_pc().wrapping_add(imm12)
        } else {
            cpu.aligned_pc().wrapping_sub(imm12)
        }
    } else if bit(op, 23) {
        cpu.regs[n].wrapping_add(op & 0xFFF)
    } else if bit(op, 11) {
        let (index, add, write) = (bit(op, 10), bit(op, 9), bit(op, 8));
        if !index && !write {
            return cpu.undefined(op);
        }
        let imm8 = op & 0xFF;
        let base = cpu.regs[n];
        let offset_address = if add { base.wrapping_add(imm8) } else { base.wrapping_sub(imm8) };
        if write {
            if n == t {
                return cpu.undefined(op);
            }
            writeback = Some(offset_address);
        }
        if index {
            offset_address
        } else {
            base
        }
    } else if bits(op, 6, 11) == 0 {
        if m == PC || m == SP {
            return cpu.undefined(op);
        }
        cpu.regs[n].wrapping_add(cpu.regs[m] << bits(op, 4, 5))
    } else {
        return cpu.undefined(op);
    };

    if loading && t == PC && size < 4 {
        // PLD, PLI and the other preload hints, nothing to do, unless they
        // write back, which is unpredictable
        return if writeback.is_some() { cpu.undefined(op) } else { None };
    }
    if loading && t == SP && size < 4 {
        return cpu.undefined(op);
    }
    if !loading && (t == PC || (t == SP && size < 4)) {
        return cpu.undefined(op);
    }
    if let Some(written) = writeback {
        cpu.regs[n] = written;
    }
    if loading {
        load_into(cpu, bus, op, t, address, size, signed)
    } else {
        store(bus, address, size, cpu.regs[t]);
        None
    }
}

/// A6.3.12
fn data_processing_register(cpu: &mut Cpu, op: u32) -> Option<Exit> {
    let (n, _, d, m) = registers(op);
    let op1 = bits(op, 20, 23);
    let op2 = bits(op, 4, 7);
    if bits(op, 12, 15) != 0xF || d == PC || d == SP || m == PC || m == SP {
        return cpu.undefined(op);
    }
    match (op1, op2) {
        // LSL, LSR, ASR and ROR by a register
        (0b0000..=0b0111, 0b0000) => {
            if n == PC || n == SP {
                return cpu.undefined(op);
            }
            let ty = ShiftType::from_bits(op1 >> 1);
            let (result, carry) = shift_by_register(ty, cpu.regs[n], cpu.regs[m], cpu.cpsr.c);
            alu::data_processing(cpu, Operation::Mov, bit(op, 20), d, 0, result, carry);
        }
        (0b0000..=0b0101, 0b1000..=0b1011) => {
            let extension = match op1 {
                0b0000 => Extension::SignedHalfword,
                0b0001 => Extension::UnsignedHalfword,
                0b0010 => Extension::SignedBytes16,
                0b0011 => Extension::UnsignedBytes16,
                0b0100 => Extension::SignedByte,
                _ => Extension::UnsignedByte,
            };
            let accumulate = (n != PC).then_some(n);
            if n == SP {
                return cpu.undefined(op);
            }
            media::extend(cpu, d, accumulate, m, bits(op, 4, 5) * 8, extension);
        }
        (0b1000..=0b1111, 0b0000..=0b0110) if op2 & 0b11 != 0b11 => {
            let lanes = match op1 & 0b111 {
                0b001 => Lanes::Add16,
                0b010 => Lanes::Asx,
                0b110 => Lanes::Sax,
                0b101 => Lanes::Sub16,
                0b000 => Lanes::Add8,
                0b100 => Lanes::Sub8,
                _ => return cpu.undefined(op),
            };
            let flavor = match op2 & 0b11 {
                0b00 => Flavor::Plain,
                0b01 => Flavor::Saturating,
                _ => Flavor::Halving,
            };
            if n == PC || n == SP {
                return cpu.undefined(op);
            }
            media::parallel(cpu, d, n, m, lanes, op2 & 0b100 == 0, flavor);
        }
        (0b1000..=0b1011, 0b1000..=0b1011) => {
            if n == PC || n == SP {
                return cpu.undefined(op);
            }
            match (op1 & 0b11, op2 & 0b11) {
                (0b00, kind) => multiply::saturating(cpu, d, m, n, kind & 0b10 != 0, kind & 1 != 0),
                // REV, REV16, RBIT and REVSH name Rm twice, the second time
                // where Rn goes
                (0b01, kind) => {
                    if n != m {
                        return cpu.undefined(op);
                    }
                    let reversal = [0, 1, 3, 2][kind as usize];
                    cpu.regs[d] = media::reverse(cpu.regs[m], reversal);
                }
                (0b10, 0b00) => media::select(cpu, d, n, m),
                (0b11, 0b00) => {
                    if n != m {
                        return cpu.undefined(op);
                    }
                    cpu.regs[d] = cpu.regs[m].leading_zeros();
                }
                _ => return cpu.undefined(op),
            }
        }
        _ => return cpu.undefined(op),
    }
    None
}

/// A6.3.16
fn multiply(cpu: &mut Cpu, op: u32) -> Option<Exit> {
    let (n, a, d, m) = registers(op);
    if bits(op, 6, 7) != 0 || [d, n, m].iter().any(|&r| r == PC || r == SP) || a == SP {
        return cpu.undefined(op);
    }
    let accumulate = (a != PC).then_some(a);
    let (top_n, top_m) = (bit(op, 5), bit(op, 4));
    match (bits(op, 20, 22), bits(op, 4, 5)) {
        (0b000, 0b00) => multiply::multiply(cpu, d, n, m, accumulate, false),
        (0b000, 0b01) => {
            if a == PC {
                return cpu.undefined(op);
            }
            multiply::multiply_subtract(cpu, d, n, m, a)
        }
        (0b001, _) => multiply::halfword(cpu, d, n, m, accumulate, top_n, top_m),
        (0b010, 0b00 | 0b01) => multiply::dual(cpu, d, n, m, accumulate, top_m, false),
        (0b011, 0b00 | 0b01) => multiply::word_by_halfword(cpu, d, n, m, accumulate, top_m),
        (0b100, 0b00 | 0b01) => multiply::dual(cpu, d, n, m, accumulate, top_m, true),
        (0b101, 0b00 | 0b01) => multiply::most_significant(cpu, d, n, m, accumulate, false, top_m),
        (0b110, 0b00 | 0b01) => {
            if a == PC {
                return cpu.undefined(op);
            }
            multiply::most_significant(cpu, d, n, m, Some(a), true, top_m)
        }
        (0b111, 0b00) => media::sum_of_differences(cpu, d, n, m, accumulate),
        _ => return cpu.undefined(op),
    }
    None
}

/// A6.3.17, the long multiplies. SDIV and UDIV came after the Cortex-A9.
fn long_multiply(cpu: &mut Cpu, op: u32) -> Option<Exit> {
    let (n, low, high, m) = registers(op);
    if [low, high, n, m].iter().any(|&r| r == PC || r == SP) || low == high {
        return cpu.undefined(op);
    }
    match (bits(op, 20, 22), bits(op, 4, 7)) {
        (0b000, 0b0000) => multiply::long(cpu, low, high, n, m, true, false, false),
        (0b010, 0b0000) => multiply::long(cpu, low, high, n, m, false, false, false),
        (0b100, 0b0000) => multiply::long(cpu, low, high, n, m, true, true, false),
        (0b100, 0b1000..=0b1011) => multiply::halfword_long(cpu, low, high, n, m, bit(op, 5), bit(op, 4)),
        (0b100, 0b1100 | 0b1101) => multiply::dual_long(cpu, low, high, n, m, bit(op, 4), false),
        (0b101, 0b1100 | 0b1101) => multiply::dual_long(cpu, low, high, n, m, bit(op, 4), true),
        (0b110, 0b0000) => multiply::long(cpu, low, high, n, m, false, true, false),
        (0b110, 0b0110) => multiply::umaal(cpu, low, high, n, m),
        _ => return cpu.undefined(op),
    }
    None
}

/// A6.3.18. a Thumb coprocessor instruction starting 1110 is word for word
/// the ARM one with the condition always, and NEON's data processing moves
/// its U bit from 28 to 24.
fn coprocessor<B: Bus>(cpu: &mut Cpu, bus: &mut B, op: u32) -> Option<Exit> {
    if bits(op, 24, 27) == 0b1111 {
        let unsigned = bit(op, 28) as u32;
        return crate::neon::data_processing(cpu, 0xF200_0000 | unsigned << 24 | (op & 0x00FF_FFFF));
    }
    if op >> 28 == 0b1110 {
        return crate::arm::execute(cpu, bus, op);
    }
    // MCR2 and the other "2" forms, for coprocessors the Vita does not have
    cpu.undefined(op)
}
