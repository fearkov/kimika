//! branches, the status register, hints, barriers, coprocessor register
//! transfers, svc and bkpt, what both decoders share of them and ARM's
//! encodings of the rest.

use crate::bits::{bit, bits, sign_extend};
use crate::{Bus, Cpu, Exit, LR, PC};

/// B and BL.
pub fn branch(cpu: &mut Cpu, op: u32) -> Option<Exit> {
    let offset = sign_extend(op & 0x00FF_FFFF, 24) << 2;
    let target = cpu.regs[PC].wrapping_add(offset as u32);
    if bit(op, 24) {
        cpu.regs[LR] = cpu.regs[PC].wrapping_sub(4);
    }
    cpu.branch(target);
    None
}

/// BX, BLX and BXJ with a register, BXJ acting as BX as on any core
/// without Jazelle.
pub fn branch_exchange(cpu: &mut Cpu, op: u32) -> Option<Exit> {
    let target = cpu.regs[(op & 0xF) as usize];
    if bits(op, 4, 7) == 0b0011 {
        if op & 0xF == 15 {
            return cpu.undefined(op);
        }
        cpu.regs[LR] = cpu.regs[PC].wrapping_sub(4);
    }
    cpu.branch_exchange(target);
    None
}

/// BLX with an immediate, the unconditional form, which always goes to
/// Thumb.
pub fn branch_link_exchange_immediate(cpu: &mut Cpu, op: u32) -> Option<Exit> {
    let offset = (sign_extend(op & 0x00FF_FFFF, 24) << 2) | ((bit(op, 24) as i32) << 1);
    let target = cpu.regs[PC].wrapping_add(offset as u32);
    cpu.regs[LR] = cpu.regs[PC].wrapping_sub(4);
    cpu.branch_exchange(target | 1);
    None
}

/// MRS from the CPSR, which in user mode reads as the APSR. the SPSR
/// belongs to the privileged modes.
pub fn read_status(cpu: &mut Cpu, op: u32, d: usize, spsr: bool) -> Option<Exit> {
    if spsr || d == PC {
        return cpu.undefined(op);
    }
    // the execution state bits, T and IT, read as zero
    cpu.regs[d] = cpu.cpsr.to_bits() & !(1 << 5);
    None
}

/// MSR to the APSR, the flags with mask bit 3 and GE with bit 2, the other
/// fields being out of user mode's reach.
pub fn write_status(cpu: &mut Cpu, op: u32, value: u32, mask: u32, spsr: bool) -> Option<Exit> {
    if spsr || mask == 0 {
        return cpu.undefined(op);
    }
    cpu.cpsr.write_masked(value, mask);
    None
}

/// NOP, YIELD, WFE, WFI, SEV and DBG, the reserved numbers acting as NOP.
pub fn hint(hint: u32) -> Option<Exit> {
    match hint {
        1..=3 => Some(Exit::Yield),
        _ => None,
    }
}

/// MRC and MCR: CP15's thread ID registers and the barriers it kept, and
/// the VFP's system registers and single register transfers.
pub fn coprocessor_register<B: Bus>(cpu: &mut Cpu, bus: &mut B, op: u32) -> Option<Exit> {
    let coprocessor = bits(op, 8, 11);
    let opc1 = bits(op, 21, 23);
    let crn = bits(op, 16, 19);
    let t = bits(op, 12, 15) as usize;
    let crm = op & 0xF;
    let opc2 = bits(op, 5, 7);
    let reading = bit(op, 20);
    match coprocessor {
        15 if reading => {
            let Some(value) = cpu.cp15.read(opc1, crn, crm, opc2) else { return cpu.undefined(op) };
            if t == PC {
                // MRC into r15 sets the flags rather than branching
                cpu.cpsr.n = bit(value, 31);
                cpu.cpsr.z = bit(value, 30);
                cpu.cpsr.c = bit(value, 29);
                cpu.cpsr.v = bit(value, 28);
            } else {
                cpu.regs[t] = value;
            }
            None
        }
        15 => match (opc1, crn, crm, opc2) {
            // CP15ISB, CP15DSB and CP15DMB, deprecated but reachable
            (0, 7, 5, 4) | (0, 7, 10, 4) | (0, 7, 10, 5) => {
                bus.barrier();
                None
            }
            _ if t != PC && cpu.cp15.write(opc1, crn, crm, opc2, cpu.regs[t]) => None,
            _ => cpu.undefined(op),
        },
        10 | 11 => crate::vfp::register_transfer(cpu, op),
        _ => cpu.undefined(op),
    }
}

pub fn supervisor_call(imm: u32) -> Option<Exit> {
    // the fetch loop already points the PC at the next instruction
    Some(Exit::Supervisor(imm))
}

pub fn breakpoint(cpu: &Cpu, imm: u32) -> Option<Exit> {
    Some(Exit::Breakpoint { pc: cpu.current, imm })
}
