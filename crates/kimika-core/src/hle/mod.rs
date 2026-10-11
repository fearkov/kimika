//! the system libraries a game imports, done here in Rust. the loader
//! writes each import's stub as `svc #index; bx lr`, so a call lands in
//! [call] with the import's number, and the function is the one named, in
//! the NID database, as the import is: one table serves every library that
//! exports a function.
//!
//! arguments come the way the ARM hard-float calling convention passes
//! them, integers and pointers in r0-r3 and then on the stack, floats in
//! s0-s15 and d0-d7, and [Call] reads them in that order.

pub mod clib;
pub mod ctrl;
pub mod display;
pub mod io;
pub mod modules;
pub mod sync;
pub mod sysmem;
pub mod threads;
pub mod time;

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock, RwLock};
use std::time::{Duration, Instant};

use kimika_cpu::Cpu;

use crate::kernel::thread::{self, Stop};
use crate::kernel::Thread;
use crate::memory::Memory;
use crate::Shared;

/// a function done here, reading its arguments from the call and leaving
/// its result there.
pub type Function = fn(&mut Call);

/// every function done here, by its name in the NID database.
fn functions() -> &'static HashMap<&'static str, Function> {
    static TABLE: OnceLock<HashMap<&'static str, Function>> = OnceLock::new();
    TABLE.get_or_init(|| {
        [
            clib::FUNCTIONS,
            ctrl::FUNCTIONS,
            display::FUNCTIONS,
            io::FUNCTIONS,
            modules::FUNCTIONS,
            sync::FUNCTIONS,
            sysmem::FUNCTIONS,
            threads::FUNCTIONS,
            time::FUNCTIONS,
        ]
        .into_iter()
        .flatten()
        .copied()
        .collect()
    })
}

/// the function done here for an import of a library, none if there is not
/// one, or the database has no name for it.
pub fn find(library: u32, nid: u32) -> Option<Function> {
    functions().get(vita_module::nid::name(library, nid)?).copied()
}

/// an import the loader linked to an svc.
pub struct Import {
    pub library: u32,
    pub nid: u32,
    pub function: Option<Function>,
}

/// what the libraries done here keep while the game runs.
#[derive(Default)]
pub struct State {
    imports: RwLock<Vec<Import>>,
    unimplemented: Mutex<HashMap<(u32, u32), u64>>,
    pub ctrl: Mutex<ctrl::CtrlState>,
    pub sampling_mode: Mutex<u32>,
    pub files: Mutex<io::Files>,
}

impl State {
    /// the number of the svc an import's stub calls.
    pub fn add_import(&self, library: u32, nid: u32) -> u32 {
        let mut imports = self.imports.write().unwrap();
        imports.push(Import { library, nid, function: find(library, nid) });
        imports.len() as u32 - 1
    }

    /// the calls made to functions not done here, by library and NID, with
    /// how many times each, the most called first.
    pub fn unimplemented(&self) -> Vec<(u32, u32, u64)> {
        let mut calls: Vec<_> =
            self.unimplemented.lock().unwrap().iter().map(|(&(library, nid), &count)| (library, nid, count)).collect();
        calls.sort_by_key(|&(library, nid, count)| (std::cmp::Reverse(count), library, nid));
        calls
    }
}

/// what a call leaves the thread that made it to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Control {
    Continue,
    ExitThread(i32),
    ExitProcess(i32),
}

/// runs the function an import's svc names.
pub fn call(shared: &Arc<Shared>, thread: &Arc<Thread>, cpu: &mut Cpu, index: u32) -> Control {
    let found = {
        let imports = shared.hle.imports.read().unwrap();
        imports.get(index as usize).map(|import| (import.library, import.nid, import.function))
    };
    let Some((library, nid, function)) = found else {
        log::warn!("svc #0x{index:X} at 0x{:08X} names no import", cpu.current);
        return Control::Continue;
    };
    let mut call = Call::new(cpu, shared, thread);
    match function {
        Some(function) => {
            if log::log_enabled!(log::Level::Trace) {
                log::trace!(
                    "{}: {}(0x{:08X}, 0x{:08X}, 0x{:08X}, 0x{:08X}) from 0x{:08X}",
                    thread.name,
                    vita_module::nid::name(library, nid).unwrap_or("?"),
                    call.cpu.regs[0],
                    call.cpu.regs[1],
                    call.cpu.regs[2],
                    call.cpu.regs[3],
                    call.cpu.regs[kimika_cpu::LR],
                );
            }
            function(&mut call);
        }
        None => unimplemented(&mut call, library, nid),
    }
    call.control
}

/// a function not done here: warned about the first time, counted, and
/// answered with 0.
fn unimplemented(call: &mut Call, library: u32, nid: u32) {
    let first = {
        let mut seen = call.shared.hle.unimplemented.lock().unwrap();
        let count = seen.entry((library, nid)).or_insert(0);
        *count += 1;
        *count == 1
    };
    if first {
        let regs = &call.cpu.regs;
        log::warn!(
            "{} calls {}::{} (0x{library:08X} 0x{nid:08X}), not done yet, with 0x{:08X}, 0x{:08X}, 0x{:08X}, 0x{:08X} from 0x{:08X}",
            call.thread.name,
            vita_module::nid::library(library).unwrap_or("?"),
            vita_module::nid::name(library, nid).unwrap_or("?"),
            regs[0],
            regs[1],
            regs[2],
            regs[3],
            regs[kimika_cpu::LR],
        );
    }
    call.cpu.regs[0] = 0;
}

/// a call in progress: the caller's registers, which hold its arguments and
/// get its result, and what every thread shares.
pub struct Call<'a> {
    pub cpu: &'a mut Cpu,
    pub shared: &'a Arc<Shared>,
    pub thread: &'a Arc<Thread>,
    /// the next core register an argument takes, 4 once they are used up.
    next_register: usize,
    /// where the next argument on the stack is.
    next_stack: u32,
    /// which of s0-s15 floating-point arguments took.
    singles: u16,
    /// set once a double found no register, after which every float goes on
    /// the stack.
    vfp_closed: bool,
    control: Control,
}

impl<'a> Call<'a> {
    pub fn new(cpu: &'a mut Cpu, shared: &'a Arc<Shared>, thread: &'a Arc<Thread>) -> Call<'a> {
        let sp = cpu.regs[kimika_cpu::SP];
        Call {
            cpu,
            shared,
            thread,
            next_register: 0,
            next_stack: sp,
            singles: 0,
            vfp_closed: false,
            control: Control::Continue,
        }
    }

    pub fn memory(&self) -> &'a Memory {
        &self.shared.memory
    }

    // -- arguments ---

    pub fn u32(&mut self) -> u32 {
        if self.next_register < 4 {
            let value = self.cpu.regs[self.next_register];
            self.next_register += 1;
            value
        } else {
            let value = self.memory().read32(self.next_stack);
            self.next_stack = self.next_stack.wrapping_add(4);
            value
        }
    }

    pub fn i32(&mut self) -> i32 {
        self.u32() as i32
    }

    /// a 64-bit integer: an even pair of registers, r0:r1 or r2:r3, or eight
    /// aligned bytes on the stack, after which no argument takes a register.
    pub fn u64(&mut self) -> u64 {
        self.next_register = self.next_register.next_multiple_of(2);
        if self.next_register < 4 {
            let (low, high) = (self.cpu.regs[self.next_register], self.cpu.regs[self.next_register + 1]);
            self.next_register += 2;
            return low as u64 | (high as u64) << 32;
        }
        self.next_register = 4;
        self.next_stack = self.next_stack.next_multiple_of(8);
        let (low, high) = (self.memory().read32(self.next_stack), self.memory().read32(self.next_stack + 4));
        self.next_stack = self.next_stack.wrapping_add(8);
        low as u64 | (high as u64) << 32
    }

    /// a float: the lowest free single register, one a double's alignment
    /// left behind included, or the stack.
    pub fn f32(&mut self) -> f32 {
        if !self.vfp_closed {
            if let Some(index) = (0..16).find(|&i| self.singles & (1 << i) == 0) {
                self.singles |= 1 << index;
                return f32::from_bits(self.cpu.vfp.s(index));
            }
        }
        f32::from_bits(self.u32_from_stack())
    }

    /// a double: the lowest free one of d0-d7, or the stack, after which
    /// floats go on the stack too.
    pub fn f64(&mut self) -> f64 {
        if !self.vfp_closed {
            if let Some(index) = (0..8).find(|&i| self.singles & (0b11 << (2 * i)) == 0) {
                self.singles |= 0b11 << (2 * index);
                return f64::from_bits(self.cpu.vfp.d(index));
            }
            self.vfp_closed = true;
        }
        self.next_stack = self.next_stack.next_multiple_of(8);
        let (low, high) = (self.u32_from_stack(), self.u32_from_stack());
        f64::from_bits(low as u64 | (high as u64) << 32)
    }

    fn u32_from_stack(&mut self) -> u32 {
        let value = self.memory().read32(self.next_stack);
        self.next_stack = self.next_stack.wrapping_add(4);
        value
    }

    /// a pointer argument to a NUL-terminated string, none for NULL or one
    /// that runs into unmapped memory.
    pub fn string(&mut self) -> Option<String> {
        match self.u32() {
            0 => None,
            address => self.memory().read_cstring(address, 4096),
        }
    }

    // -- results ---

    pub fn ret(&mut self, value: impl Return) {
        value.store(self.cpu);
    }

    /// a result that is a value or an error code, both in r0.
    pub fn result(&mut self, result: Result<u32, u32>) {
        self.cpu.regs[0] = result.unwrap_or_else(|error| error);
    }

    // -- calling back ---

    /// calls a guest function on the calling thread, its stack and its
    /// registers, which are as they were afterwards, and gives what it
    /// returned; none if it ended the thread or the process instead, which
    /// then ends once this call returns. read every argument before.
    pub fn call_guest(&mut self, function: u32, arguments: &[u32]) -> Option<u32> {
        match thread::call_guest(self.shared, self.thread, self.cpu, function, arguments) {
            Ok(result) => Some(result),
            Err(Stop::ExitThread(status)) => {
                self.control = Control::ExitThread(status);
                None
            }
            Err(Stop::ExitProcess(status)) => {
                self.control = Control::ExitProcess(status);
                None
            }
            Err(Stop::Returned(_) | Stop::Stopped) => None,
        }
    }

    /// ends the calling thread once the function returns.
    pub fn exit_thread(&mut self, status: i32) {
        self.control = Control::ExitThread(status);
    }

    /// ends the process once the function returns.
    pub fn exit_process(&mut self, status: i32) {
        self.control = Control::ExitProcess(status);
    }
}

/// a timeout argument, a pointer to the microseconds a call may wait,
/// none for NULL, which the call writes back with what is left once it is
/// done.
pub struct Timeout {
    pointer: u32,
    limit: Option<Duration>,
    start: Instant,
}

impl Timeout {
    pub fn read(call: &mut Call) -> Timeout {
        let pointer = call.u32();
        let limit = (pointer != 0).then(|| Duration::from_micros(call.memory().read32(pointer) as u64));
        Timeout { pointer, limit, start: Instant::now() }
    }

    pub fn limit(&self) -> Option<Duration> {
        self.limit
    }

    pub fn finish(&self, call: &Call) {
        if let Some(limit) = self.limit {
            let left = limit.saturating_sub(self.start.elapsed()).as_micros().min(u32::MAX as u128);
            call.memory().write32(self.pointer, left as u32);
        }
    }
}

/// a value a function returns, where the calling convention puts it.
pub trait Return {
    fn store(self, cpu: &mut Cpu);
}

impl Return for u32 {
    fn store(self, cpu: &mut Cpu) {
        cpu.regs[0] = self;
    }
}

impl Return for i32 {
    fn store(self, cpu: &mut Cpu) {
        cpu.regs[0] = self as u32;
    }
}

impl Return for u64 {
    fn store(self, cpu: &mut Cpu) {
        cpu.regs[0] = self as u32;
        cpu.regs[1] = (self >> 32) as u32;
    }
}

impl Return for f32 {
    fn store(self, cpu: &mut Cpu) {
        cpu.vfp.set_s(0, self.to_bits());
    }
}

impl Return for f64 {
    fn store(self, cpu: &mut Cpu) {
        cpu.vfp.set_d(0, self.to_bits());
    }
}
