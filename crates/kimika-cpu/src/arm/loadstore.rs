//! loads and stores: single words, bytes and halfwords, doublewords, blocks
//! of registers and the exclusives, shared by both decoders, then ARM's
//! encodings of them.

use super::shifter::{shift_by_immediate, ShiftType};
use crate::bits::{bit, bits};
use crate::{Bus, Cpu, Exit, Reservation, PC, SP};

/// size bytes from addr, sign-extended when signed.
#[inline(always)]
pub fn load<B: Bus>(bus: &mut B, addr: u32, size: u32, signed: bool) -> u32 {
    match (size, signed) {
        (1, false) => bus.read8(addr) as u32,
        (1, true) => bus.read8(addr) as i8 as i32 as u32,
        (2, false) => bus.read16(addr) as u32,
        (2, true) => bus.read16(addr) as i16 as i32 as u32,
        _ => bus.read32(addr),
    }
}

#[inline(always)]
pub fn store<B: Bus>(bus: &mut B, addr: u32, size: u32, value: u32) {
    match size {
        1 => bus.write8(addr, value as u8),
        2 => bus.write16(addr, value as u16),
        _ => bus.write32(addr, value),
    }
}

/// a register loaded from memory, which for the PC is an interworking
/// branch.
#[inline(always)]
pub fn load_register(cpu: &mut Cpu, t: usize, value: u32) {
    if t == PC {
        cpu.branch_exchange(value);
    } else {
        cpu.regs[t] = value;
    }
}

/// LDREX and its byte, halfword and doubleword forms: the load, and a mark
/// of what it read for STREX to store against.
pub fn load_exclusive<B: Bus>(cpu: &mut Cpu, bus: &mut B, address: u32, size: u32) -> u64 {
    let value = match size {
        8 => bus.read32(address) as u64 | (bus.read32(address.wrapping_add(4)) as u64) << 32,
        _ => load(bus, address, size, false) as u64,
    };
    cpu.exclusive = Some(Reservation { address, size, value });
    value
}

/// STREX and its forms: a store, if the mark LDREX left is for this address
/// and memory still holds what it read then, made as one step against the
/// other cores. any STREX ends the mark. 0 when it stored, 1 when not.
pub fn store_exclusive<B: Bus>(cpu: &mut Cpu, bus: &mut B, address: u32, size: u32, value: u64) -> u32 {
    let stored = match cpu.exclusive.take() {
        Some(mark) if mark.address == address && mark.size == size => {
            bus.compare_exchange(address, size, mark.value, value)
        }
        _ => false,
    };
    !stored as u32
}

/// SWP and SWPB, which ARMv7 keeps, as one exchange.
fn swap<B: Bus>(bus: &mut B, address: u32, size: u32, value: u32) -> u32 {
    loop {
        let old = load(bus, address, size, false);
        if bus.compare_exchange(address, size, old as u64, value as u64) {
            return old;
        }
    }
}

/// LDM and POP from the lowest address up, a loaded PC branching. none
/// for an empty list, or one with SP where only ARM may load it.
pub fn load_multiple<B: Bus>(cpu: &mut Cpu, bus: &mut B, list: u32, lowest: u32, sp_allowed: bool) -> Option<()> {
    if list == 0 || (!sp_allowed && list & (1 << SP) != 0) {
        return None;
    }
    let mut address = lowest;
    let mut pc = None;
    for i in 0..16 {
        if list & (1 << i) != 0 {
            let value = bus.read32(address);
            address = address.wrapping_add(4);
            if i == PC {
                pc = Some(value);
            } else {
                cpu.regs[i] = value;
            }
        }
    }
    if let Some(value) = pc {
        cpu.branch_exchange(value);
    }
    Some(())
}

/// STM and PUSH, with base the value the base register gets when it is
/// stored but not first in the list, which ARMv7 leaves unknown and the
/// ARM11 made the written back one.
pub fn store_multiple<B: Bus>(
    cpu: &Cpu,
    bus: &mut B,
    list: u32,
    lowest: u32,
    base: Option<(usize, u32)>,
) -> Option<()> {
    if list == 0 {
        return None;
    }
    let mut address = lowest;
    for i in 0..16 {
        if list & (1 << i) != 0 {
            let value = match base {
                Some((n, written)) if n == i && list & ((1 << i) - 1) != 0 => written,
                _ => cpu.regs[i],
            };
            bus.write32(address, value);
            address = address.wrapping_add(4);
        }
    }
    Some(())
}

// ARM's encodings.

/// LDR, STR, LDRB and STRB, and the T forms, which in user mode are the same.
pub fn single<B: Bus>(cpu: &mut Cpu, bus: &mut B, op: u32) -> Option<Exit> {
    let n = bits(op, 16, 19) as usize;
    let t = bits(op, 12, 15) as usize;
    let pre_index = bit(op, 24);
    let add = bit(op, 23);
    let byte = bit(op, 22);
    let writeback = !pre_index || bit(op, 21);
    let loading = bit(op, 20);
    if writeback && (n == PC || n == t) {
        return cpu.undefined(op);
    }

    let offset = if bit(op, 25) {
        let m = (op & 0xF) as usize;
        let ty = ShiftType::from_bits(bits(op, 5, 6));
        shift_by_immediate(ty, cpu.regs[m], bits(op, 7, 11), cpu.cpsr.c).0
    } else {
        op & 0xFFF
    };
    // the PC as a base is word aligned, for literal loads
    let base = if n == PC { cpu.aligned_pc() } else { cpu.regs[n] };
    let offset_address = if add { base.wrapping_add(offset) } else { base.wrapping_sub(offset) };
    let address = if pre_index { offset_address } else { base };
    let size = if byte { 1 } else { 4 };

    if loading {
        let value = load(bus, address, size, false);
        if writeback {
            cpu.regs[n] = offset_address;
        }
        load_register(cpu, t, value);
    } else {
        // storing r15 stores the PC as read, the address plus 8
        store(bus, address, size, cpu.regs[t]);
        if writeback {
            cpu.regs[n] = offset_address;
        }
    }
    None
}

/// LDRH, STRH, LDRSB, LDRSH, LDRD and STRD, and the T forms of the first
/// four.
pub fn extra<B: Bus>(cpu: &mut Cpu, bus: &mut B, op: u32) -> Option<Exit> {
    let n = bits(op, 16, 19) as usize;
    let t = bits(op, 12, 15) as usize;
    let pre_index = bit(op, 24);
    let add = bit(op, 23);
    let writeback = !pre_index || bit(op, 21);
    let loading = bit(op, 20);
    let offset = if bit(op, 22) { (bits(op, 8, 11) << 4) | (op & 0xF) } else { cpu.regs[(op & 0xF) as usize] };
    let base = if n == PC { cpu.aligned_pc() } else { cpu.regs[n] };
    let offset_address = if add { base.wrapping_add(offset) } else { base.wrapping_sub(offset) };
    let address = if pre_index { offset_address } else { base };

    // bits 6:5 say which: with L clear 01 STRH, 10 LDRD and 11 STRD, with
    // L set 01 LDRH, 10 LDRSB and 11 LDRSH
    let sh = bits(op, 5, 6);
    if !loading && sh != 0b01 {
        // the doublewords take Rt and the next register, Rt even and not r14
        let clash = writeback && (n == PC || n == t || n == t + 1);
        if t & 1 != 0 || t == 14 || clash {
            return cpu.undefined(op);
        }
        if sh == 0b10 {
            let (low, high) = (bus.read32(address), bus.read32(address.wrapping_add(4)));
            cpu.regs[t] = low;
            cpu.regs[t + 1] = high;
        } else {
            bus.write32(address, cpu.regs[t]);
            bus.write32(address.wrapping_add(4), cpu.regs[t + 1]);
        }
    } else {
        if writeback && (n == PC || n == t) {
            return cpu.undefined(op);
        }
        if loading {
            let (size, signed) = match sh {
                0b01 => (2, false),
                0b10 => (1, true),
                _ => (2, true),
            };
            let value = load(bus, address, size, signed);
            if writeback {
                cpu.regs[n] = offset_address;
            }
            load_register(cpu, t, value);
            return None;
        }
        store(bus, address, 2, cpu.regs[t]);
    }
    if writeback {
        cpu.regs[n] = offset_address;
    }
    None
}

/// LDM and STM in the four orders. the forms that reach the user bank or
/// return from an exception are for privileged code.
pub fn block<B: Bus>(cpu: &mut Cpu, bus: &mut B, op: u32) -> Option<Exit> {
    let n = bits(op, 16, 19) as usize;
    let before = bit(op, 24);
    let increment = bit(op, 23);
    let writeback = bit(op, 21);
    let loading = bit(op, 20);
    let list = op & 0xFFFF;
    if bit(op, 22) || n == PC {
        return cpu.undefined(op);
    }
    let base = cpu.regs[n];
    let span = list.count_ones() * 4;
    let lowest = match (increment, before) {
        (true, false) => base,
        (true, true) => base.wrapping_add(4),
        (false, false) => base.wrapping_sub(span).wrapping_add(4),
        (false, true) => base.wrapping_sub(span),
    };
    let written = if increment { base.wrapping_add(span) } else { base.wrapping_sub(span) };
    if loading {
        // writing back a base that is also loaded is unpredictable
        if writeback && list & (1 << n) != 0 {
            return cpu.undefined(op);
        }
        if writeback {
            cpu.regs[n] = written;
        }
        if load_multiple(cpu, bus, list, lowest, true).is_none() {
            return cpu.undefined(op);
        }
    } else {
        if store_multiple(cpu, bus, list, lowest, writeback.then_some((n, written))).is_none() {
            return cpu.undefined(op);
        }
        if writeback {
            cpu.regs[n] = written;
        }
    }
    None
}

/// the synchronization primitives: SWP and SWPB, and the exclusives.
pub fn synchronization<B: Bus>(cpu: &mut Cpu, bus: &mut B, op: u32) -> Option<Exit> {
    let n = bits(op, 16, 19) as usize;
    let t = bits(op, 12, 15) as usize;
    let address = cpu.regs[n];
    if !bit(op, 23) {
        // SWP and SWPB
        let m = (op & 0xF) as usize;
        if bits(op, 20, 21) != 0 || [n, t, m].contains(&PC) || n == t || n == m {
            return cpu.undefined(op);
        }
        cpu.regs[t] = swap(bus, address, if bit(op, 22) { 1 } else { 4 }, cpu.regs[m]);
        return None;
    }
    let size = match bits(op, 21, 22) {
        0b00 => 4,
        0b01 => 8,
        0b10 => 1,
        _ => 2,
    };
    if n == PC {
        return cpu.undefined(op);
    }
    if bit(op, 20) {
        // LDREX, the doubleword form into an even register and the next
        if t == PC || (size == 8 && (t & 1 != 0 || t == 14)) {
            return cpu.undefined(op);
        }
        let value = load_exclusive(cpu, bus, address, size);
        cpu.regs[t] = value as u32;
        if size == 8 {
            cpu.regs[t + 1] = (value >> 32) as u32;
        }
    } else {
        // STREX puts its status in Rd, at 15:12, and stores Rt, at 3:0
        let (d, t) = (t, (op & 0xF) as usize);
        let pair = size == 8 && (t & 1 != 0 || t == 14);
        if d == PC || t == PC || pair || d == n || d == t || (size == 8 && d == t + 1) {
            return cpu.undefined(op);
        }
        let value = if size == 8 { cpu.regs[t] as u64 | (cpu.regs[t + 1] as u64) << 32 } else { cpu.regs[t] as u64 };
        cpu.regs[d] = store_exclusive(cpu, bus, address, size, value);
    }
    None
}
