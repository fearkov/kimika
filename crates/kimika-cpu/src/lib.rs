//! the PlayStation Vita's CPU as games see it: a Cortex-A9 in user mode,
//! ARMv7 in ARM and Thumb state, VFPv3 with 32 double registers, and NEON.
//! it is the reference recompiled code is checked against and runs what
//! that code leaves out, one instruction at a time.
//!
//! several of these run at once, one for each guest thread on a host thread
//! of its own, so memory is reached through a Bus each of them has, and the
//! exclusive monitor stores with a compare and swap rather than trusting
//! that nothing else ran in between.

pub mod arm;
pub mod bits;
pub mod cp15;
pub mod fp;
pub mod neon;
pub mod psr;
pub mod thumb;
pub mod vfp;

pub use cp15::Cp15;
pub use psr::{check_condition, Psr};
pub use vfp::Vfp;

/// everything the CPU can reach outside itself.
pub trait Bus {
    fn read8(&mut self, addr: u32) -> u8;
    fn read16(&mut self, addr: u32) -> u16;
    fn read32(&mut self, addr: u32) -> u32;

    fn write8(&mut self, addr: u32, value: u8);
    fn write16(&mut self, addr: u32, value: u16);
    fn write32(&mut self, addr: u32, value: u32);

    /// instruction fetch.
    #[inline(always)]
    fn fetch16(&mut self, addr: u32) -> u16 {
        self.read16(addr)
    }

    #[inline(always)]
    fn fetch32(&mut self, addr: u32) -> u32 {
        self.read32(addr)
    }

    /// stores value at addr if the size bytes there still hold expected, as
    /// one step no other core can come in between, and says whether it
    /// stored. STREX is this, against what LDREX read. the default is only
    /// right where one core runs.
    fn compare_exchange(&mut self, addr: u32, size: u32, expected: u64, value: u64) -> bool {
        let now = match size {
            1 => self.read8(addr) as u64,
            2 => self.read16(addr) as u64,
            4 => self.read32(addr) as u64,
            _ => self.read32(addr) as u64 | (self.read32(addr.wrapping_add(4)) as u64) << 32,
        };
        if now != expected {
            return false;
        }
        match size {
            1 => self.write8(addr, value as u8),
            2 => self.write16(addr, value as u16),
            4 => self.write32(addr, value as u32),
            _ => {
                self.write32(addr, value as u32);
                self.write32(addr.wrapping_add(4), (value >> 32) as u32);
            }
        }
        true
    }

    /// DMB, DSB and ISB: what came before is seen by other cores before
    /// what comes after.
    #[inline(always)]
    fn barrier(&mut self) {
        std::sync::atomic::fence(std::sync::atomic::Ordering::SeqCst);
    }
}

/// why [Cpu::run] or [Cpu::step] stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Exit {
    /// the budget ran out, nothing else happened.
    Timeout,
    /// an svc ran, its immediate here and the pc already past it.
    Supervisor(u32),
    /// an instruction the CPU does not have, or that does nothing defined,
    /// with the opcode as fetched: a Thumb instruction of 32 bits has its
    /// first halfword on top.
    Undefined {
        pc: u32,
        opcode: u32,
    },
    Breakpoint {
        pc: u32,
        imm: u32,
    },
    /// [Cpu::halt] was called from inside an instruction handler.
    Halted,
    /// WFE, WFI or YIELD, a thread with nothing to do until another core
    /// does something, which a scheduler can take as a hint.
    Yield,
}

pub const PC: usize = 15;
pub const LR: usize = 14;
pub const SP: usize = 13;

/// what LDREX marked: where, how many bytes and what they held.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Reservation {
    pub address: u32,
    pub size: u32,
    pub value: u64,
}

#[derive(Clone)]
pub struct Cpu {
    /// r0-r15. while an instruction runs, r15 reads as its address plus 8 in
    /// ARM state and plus 4 in Thumb, and between instructions it is the
    /// address of the next one.
    pub regs: [u32; 16],
    pub cpsr: Psr,
    pub vfp: Vfp,
    pub cp15: Cp15,
    pub exclusive: Option<Reservation>,
    /// the address of the instruction running, or that ran last.
    pub current: u32,
    /// set by a handler when it writes r15, so the fetch loop knows not to
    /// move the PC on itself.
    branched: bool,
    halted: bool,
    /// instructions run so far.
    pub cycles: u64,
}

impl Default for Cpu {
    fn default() -> Self {
        Self::new()
    }
}

impl Cpu {
    pub fn new() -> Cpu {
        Cpu {
            regs: [0; 16],
            cpsr: Psr::default(),
            vfp: Vfp::new(),
            cp15: Cp15::default(),
            exclusive: None,
            current: 0,
            branched: false,
            halted: false,
            cycles: 0,
        }
    }

    #[inline(always)]
    pub fn reg(&self, index: usize) -> u32 {
        self.regs[index]
    }

    /// writes a register the way data processing does, r15 through
    /// [Cpu::alu_write_pc], so every instruction that can write the PC does
    /// the right thing with it.
    #[inline(always)]
    pub fn set_reg(&mut self, index: usize, value: u32) {
        if index == PC {
            self.alu_write_pc(value);
        } else {
            self.regs[index] = value;
        }
    }

    /// a branch that stays in the current state, B and BL, and data
    /// processing in Thumb state.
    #[inline(always)]
    pub fn branch(&mut self, addr: u32) {
        self.regs[PC] = if self.cpsr.thumb { addr & !1 } else { addr & !3 };
        self.branched = true;
    }

    /// an interworking branch, bit 0 of addr selecting Thumb: BX, BLX, and
    /// every load of the PC.
    #[inline(always)]
    pub fn branch_exchange(&mut self, addr: u32) {
        self.cpsr.thumb = addr & 1 != 0;
        self.regs[PC] = if self.cpsr.thumb { addr & !1 } else { addr & !3 };
        self.branched = true;
    }

    /// a write of the PC by data processing, which interworks in ARM state
    /// since ARMv7 and does not in Thumb.
    #[inline(always)]
    pub fn alu_write_pc(&mut self, addr: u32) {
        if self.cpsr.thumb {
            self.branch(addr);
        } else {
            self.branch_exchange(addr);
        }
    }

    /// the address of the instruction running.
    #[inline(always)]
    pub fn current_pc(&self) -> u32 {
        self.current
    }

    /// the PC as an instruction reads it, its address plus 4 in Thumb state
    /// and 8 in ARM, word aligned for the literal loads and ADR.
    #[inline(always)]
    pub fn aligned_pc(&self) -> u32 {
        self.regs[PC] & !3
    }

    /// the instruction running cannot run: undefined, or unpredictable,
    /// which the CPU treats the same so that recompiled code and the
    /// interpreter never pick different meanings for it.
    #[cold]
    pub fn undefined(&self, opcode: u32) -> Option<Exit> {
        Some(Exit::Undefined { pc: self.current, opcode })
    }

    pub fn halt(&mut self) {
        self.halted = true;
    }

    pub fn is_halted(&self) -> bool {
        self.halted
    }

    pub fn resume(&mut self) {
        self.halted = false;
    }

    /// starts over at entry, odd for Thumb, with the stack at sp, as a new
    /// thread does.
    pub fn reset_to(&mut self, entry: u32, sp: u32) {
        self.regs = [0; 16];
        self.regs[SP] = sp;
        self.cpsr = Psr { thumb: entry & 1 != 0, ..Psr::default() };
        self.regs[PC] = entry & !1;
        self.current = entry & !1;
        self.exclusive = None;
        self.branched = false;
        self.halted = false;
    }

    /// runs up to budget instructions, stopping early for anything the
    /// caller has to deal with.
    pub fn run<B: Bus>(&mut self, bus: &mut B, budget: u64) -> Exit {
        let end = self.cycles.wrapping_add(budget);
        while self.cycles < end {
            if self.halted {
                return Exit::Halted;
            }
            if let Some(exit) = self.step(bus) {
                return exit;
            }
        }
        Exit::Timeout
    }

    /// runs exactly one instruction.
    #[inline]
    pub fn step<B: Bus>(&mut self, bus: &mut B) -> Option<Exit> {
        self.cycles += 1;
        let addr = self.regs[PC];
        self.current = addr;
        if self.cpsr.thumb {
            let first = bus.fetch16(addr) as u32;
            // 0b11101, 0b11110 and 0b11111 on top start an instruction of
            // two halfwords
            let wide = first >= 0xE800;
            let opcode = if wide { first << 16 | bus.fetch16(addr.wrapping_add(2)) as u32 } else { first };
            self.regs[PC] = addr.wrapping_add(4);
            // inside an IT block every instruction has the block's
            // condition, and the block moves on whether it passed or not
            let in_block = self.cpsr.in_it_block();
            let exit = if !in_block || check_condition(self.cpsr.it_condition(), &self.cpsr) {
                thumb::execute(self, bus, opcode, wide)
            } else {
                None
            };
            if in_block {
                self.cpsr.advance_it();
            }
            if self.branched {
                self.branched = false;
            } else {
                self.regs[PC] = addr.wrapping_add(if wide { 4 } else { 2 });
            }
            exit
        } else {
            let opcode = bus.fetch32(addr);
            self.regs[PC] = addr.wrapping_add(8);
            let exit = arm::execute(self, bus, opcode);
            if self.branched {
                self.branched = false;
            } else {
                self.regs[PC] = addr.wrapping_add(4);
            }
            exit
        }
    }

    #[inline(always)]
    pub fn set_nz(&mut self, value: u32) {
        self.cpsr.n = (value as i32) < 0;
        self.cpsr.z = value == 0;
    }
}

impl std::fmt::Debug for Cpu {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        writeln!(f, "CPU state (pc=0x{:08X}):", self.current)?;
        for row in 0..4 {
            for col in 0..4 {
                let i = row * 4 + col;
                write!(f, "  r{i:<2}=0x{:08X}", self.regs[i])?;
            }
            writeln!(f)?;
        }
        write!(
            f,
            "  cpsr=0x{:08X} [{}{}{}{}{}] {} it=0x{:02X}",
            self.cpsr.to_bits(),
            if self.cpsr.n { 'N' } else { '-' },
            if self.cpsr.z { 'Z' } else { '-' },
            if self.cpsr.c { 'C' } else { '-' },
            if self.cpsr.v { 'V' } else { '-' },
            if self.cpsr.q { 'Q' } else { '-' },
            if self.cpsr.thumb { "Thumb" } else { "ARM" },
            self.cpsr.it,
        )
    }
}
