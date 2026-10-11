//! A7.7, the element and structure loads and stores, VLD1-4 and VST1-4.
//! a structure is one element from each of up to four registers, laid out
//! one after another in memory. alignment is not checked: the hardware's
//! faults are something no game counts on.

use super::{lane, registers, set_lane, undefined};
use crate::bits::{bit, bits};
use crate::{Bus, Cpu, Exit, PC, SP};

/// how a load or store moves its elements: structures of so many elements,
/// the registers they go to so far apart, and so many registers deep for
/// the multiple forms; or one lane, or every lane, of each register.
struct Plan {
    structure: usize,
    spacing: usize,
    depth: usize,
    ebytes: u32,
    shape: Shape,
}

enum Shape {
    Multiple,
    Lane(usize),
    AllLanes,
}

pub fn load_store<B: Bus>(cpu: &mut Cpu, bus: &mut B, op: u32) -> Option<Exit> {
    let loading = bit(op, 21);
    let (n, m) = (bits(op, 16, 19) as usize, (op & 0xF) as usize);
    let d = registers(op).0;
    let Some(plan) = plan(op, loading) else {
        return undefined(cpu, op);
    };
    // the last register any structure reaches
    let last = d + (plan.structure - 1) * plan.spacing + plan.depth - 1;
    if n == PC || last > 31 {
        return undefined(cpu, op);
    }
    let start = cpu.regs[n];
    let mut address = start;
    let size = plan.ebytes * 8;
    match plan.shape {
        Shape::Multiple => {
            for r in 0..plan.depth {
                for e in 0..(8 / plan.ebytes) as usize {
                    for k in 0..plan.structure {
                        element(cpu, bus, loading, d + k * plan.spacing + r, e, size, address);
                        address = address.wrapping_add(plan.ebytes);
                    }
                }
            }
        }
        Shape::Lane(index) => {
            for k in 0..plan.structure {
                element(cpu, bus, loading, d + k * plan.spacing, index, size, address);
                address = address.wrapping_add(plan.ebytes);
            }
        }
        Shape::AllLanes => {
            // VLD1 can fill two registers with the one element, the others
            // one register for each element of the structure
            for k in 0..plan.structure {
                let value = read(bus, address, size);
                for r in 0..plan.depth {
                    cpu.vfp.set_d(d + k * plan.spacing + r, super::replicate(value, size));
                }
                address = address.wrapping_add(plan.ebytes);
            }
        }
    }
    // Rm the PC means no writeback, SP the size moved, else Rm added
    if m != PC {
        let step = if m == SP { address.wrapping_sub(start) } else { cpu.regs[m] };
        cpu.regs[n] = start.wrapping_add(step);
    }
    None
}

/// the encoding's plan, or None if it is undefined.
fn plan(op: u32, loading: bool) -> Option<Plan> {
    if !bit(op, 23) {
        // multiple structures: the type at 11:8 gives the structure's size,
        // how many registers deep, and the spacing
        let (kind, size, align) = (bits(op, 8, 11), bits(op, 6, 7), bits(op, 4, 5));
        let (structure, depth, spacing) = match kind {
            0b0111 if align & 2 == 0 => (1, 1, 1),
            0b1010 if align != 3 => (1, 2, 1),
            0b0110 if align & 2 == 0 => (1, 3, 1),
            0b0010 => (1, 4, 1),
            0b1000 | 0b1001 if size != 3 && align != 3 => (2, 1, (kind & 1) as usize + 1),
            0b0011 if size != 3 => (2, 2, 2),
            0b0100 | 0b0101 if size != 3 && align & 2 == 0 => (3, 1, (kind & 1) as usize + 1),
            0b0000 | 0b0001 if size != 3 => (4, 1, (kind & 1) as usize + 1),
            _ => return None,
        };
        return Some(Plan { structure, spacing, depth, ebytes: 1 << size, shape: Shape::Multiple });
    }
    let structure = bits(op, 8, 9) as usize + 1;
    let size = bits(op, 10, 11);
    if size == 3 {
        // a structure to all lanes, which only loads have
        let (size, t, a) = (bits(op, 6, 7), bit(op, 5), bit(op, 4));
        let refused = match structure {
            1 => size == 3 || (size == 0 && a),
            2 => size == 3,
            3 => size == 3 || a,
            _ => size == 3 && !a,
        };
        if !loading || refused {
            return None;
        }
        // VLD4's size 3 is words aligned to 16 bytes
        let ebytes = if size == 3 { 4 } else { 1 << size };
        let (depth, spacing) = if structure == 1 { (t as usize + 1, 1) } else { (1, t as usize + 1) };
        return Some(Plan { structure, spacing, depth, ebytes, shape: Shape::AllLanes });
    }
    // a structure to or from one lane: index_align at 7:4 holds the index
    // above the bits that say how far apart the registers are and the
    // alignment
    let ia = bits(op, 4, 7);
    let index = (ia >> (size + 1)) as usize;
    let apart = match size {
        0 => false,
        1 => bit(ia, 1),
        _ => bit(ia, 2),
    };
    let refused = match (structure, size) {
        (1, 0) => ia & 1 != 0,
        (1, 1) => ia & 2 != 0,
        (1, _) => ia & 4 != 0 || (ia & 3 != 0 && ia & 3 != 3),
        (2, 2) => ia & 2 != 0,
        (2, _) => false,
        (3, 2) => ia & 3 != 0,
        (3, _) => ia & 1 != 0,
        (_, 2) => ia & 3 == 3,
        _ => false,
    };
    if refused {
        return None;
    }
    let spacing = if structure > 1 && apart { 2 } else { 1 };
    Some(Plan { structure, spacing, depth: 1, ebytes: 1 << size, shape: Shape::Lane(index) })
}

fn read<B: Bus>(bus: &mut B, address: u32, size: u32) -> u64 {
    match size {
        8 => bus.read8(address) as u64,
        16 => bus.read16(address) as u64,
        32 => bus.read32(address) as u64,
        _ => bus.read32(address) as u64 | (bus.read32(address.wrapping_add(4)) as u64) << 32,
    }
}

fn element<B: Bus>(cpu: &mut Cpu, bus: &mut B, loading: bool, register: usize, e: usize, size: u32, address: u32) {
    let mut word = [cpu.vfp.d(register)];
    if loading {
        set_lane(&mut word, e, size, read(bus, address, size));
        cpu.vfp.set_d(register, word[0]);
    } else {
        let value = lane(&word, e, size);
        match size {
            8 => bus.write8(address, value as u8),
            16 => bus.write16(address, value as u16),
            32 => bus.write32(address, value as u32),
            _ => {
                bus.write32(address, value as u32);
                bus.write32(address.wrapping_add(4), (value >> 32) as u32);
            }
        }
    }
}
