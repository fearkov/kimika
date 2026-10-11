//! threads: each guest thread runs on a host thread of its own, holding one
//! of the console's three cores while it runs guest code and giving it up
//! while it waits in the kernel, so that no more of them run at once than
//! the console runs.

use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use kimika_cpu::{Cpu, Exit, LR, PC, SP};

use super::{errors, Uid};
use crate::hle::{self, Control};
use crate::memory::GuestBus;
use crate::{Outcome, Shared};

/// the block of thread local storage each thread has, which TPIDRURO
/// points just past.
pub const TLS_SIZE: u32 = 0x800;
/// a page of the kernel's own, where a thread that returns from its entry
/// lands, on an svc that ends it.
pub const RETURN_TRAP: u32 = 0x80FF_F000;
/// that svc's number, past any import's.
pub const RETURN_SVC: u32 = 0x00FF_FFFF;
/// the FPSCR a thread starts with, flush to zero and default NaN.
pub const FPSCR: u32 = 0x0300_0000;
/// how many instructions a thread runs before it lets another have its
/// core, when one is waiting.
const SLICE: u64 = 8192;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    /// made, not started.
    Dormant,
    Running,
    /// ended, with its status.
    Dead(i32),
}

pub struct Thread {
    pub uid: Uid,
    pub name: String,
    /// where it starts, odd for Thumb.
    pub entry: u32,
    pub priority: u32,
    pub affinity: u32,
    pub attributes: u32,
    /// the lowest address of its stack, and how big it is.
    pub stack: u32,
    pub stack_size: u32,
    /// its block of thread local storage.
    pub tls: u32,
    /// the process's first thread, whose end ends the process.
    pub main: bool,
    state: Mutex<State>,
    /// woken when it ends, and when a nap should end early.
    changed: Condvar,
}

impl Thread {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        uid: Uid,
        name: &str,
        entry: u32,
        priority: u32,
        affinity: u32,
        attributes: u32,
        (stack, stack_size): (u32, u32),
        tls: u32,
        main: bool,
    ) -> Thread {
        Thread {
            uid,
            name: name.to_owned(),
            entry,
            priority,
            affinity,
            attributes,
            stack,
            stack_size,
            tls,
            main,
            state: Mutex::new(State::Dormant),
            changed: Condvar::new(),
        }
    }

    /// what TPIDRURO holds for it.
    pub fn tls_pointer(&self) -> u32 {
        self.tls + TLS_SIZE
    }

    pub fn state(&self) -> State {
        *self.state.lock().unwrap()
    }

    pub fn is_dead(&self) -> bool {
        matches!(self.state(), State::Dead(_))
    }

    fn end(&self, status: i32) {
        *self.state.lock().unwrap() = State::Dead(status);
        self.changed.notify_all();
    }

    /// wakes it from a nap or a wait for another thread's end.
    pub fn wake(&self) {
        let _state = self.state.lock().unwrap();
        self.changed.notify_all();
    }

    /// sleeps for duration with its core given up, or less when the process
    /// ends.
    pub fn nap(&self, shared: &Shared, duration: Duration) {
        let deadline = Instant::now() + duration;
        shared.kernel.cores.release();
        let mut state = self.state.lock().unwrap();
        while !shared.is_stopping() {
            let now = Instant::now();
            if now >= deadline {
                break;
            }
            state = self.changed.wait_timeout(state, deadline - now).unwrap().0;
        }
        drop(state);
        shared.kernel.cores.acquire(shared);
    }

    /// waits for another thread to end, its core given up meanwhile, and
    /// gives its status, or none if the process ends first or timeout runs
    /// out.
    pub fn wait_end(&self, shared: &Shared, timeout: Option<Duration>) -> Option<i32> {
        let deadline = timeout.map(|timeout| Instant::now() + timeout);
        shared.kernel.cores.release();
        let mut state = self.state.lock().unwrap();
        let status = loop {
            if let State::Dead(status) = *state {
                break Some(status);
            }
            if shared.is_stopping() {
                break None;
            }
            match deadline {
                Some(deadline) => {
                    let now = Instant::now();
                    if now >= deadline {
                        break None;
                    }
                    state = self.changed.wait_timeout(state, deadline - now).unwrap().0;
                }
                None => state = self.changed.wait(state).unwrap(),
            }
        };
        drop(state);
        shared.kernel.cores.acquire(shared);
        status
    }
}

/// starts a dormant thread on a host thread of its own, with arguments
/// copied to the top of its stack, the way sceKernelStartThread does.
pub fn start(shared: &Arc<Shared>, thread: &Arc<Thread>, arguments: &[u8]) -> Result<(), u32> {
    start_after(shared, thread, arguments, Vec::new())
}

/// starts a thread as [start] does, calling the functions in first, with
/// no arguments, before it goes to its entry: the process's first thread
/// starts the modules loaded with the program this way.
pub fn start_after(shared: &Arc<Shared>, thread: &Arc<Thread>, arguments: &[u8], first: Vec<u32>) -> Result<(), u32> {
    {
        let mut state = thread.state.lock().unwrap();
        if *state != State::Dormant {
            return Err(errors::NOT_DORMANT);
        }
        *state = State::Running;
    }
    let memory = &shared.memory;
    // the thread's storage starts out naming it: its process, itself, its
    // stack, its priority and its cores
    memory.fill(thread.tls, 0, TLS_SIZE);
    let words = [
        (0x00, 0x0001_0001),
        (0x04, thread.uid),
        (0x08, thread.stack),
        (0x0C, thread.stack + thread.stack_size),
        (0x20, thread.priority),
        (0x24, thread.affinity),
    ];
    for (offset, value) in words {
        memory.write32(thread.tls + offset, value);
    }

    let top = thread.stack + thread.stack_size;
    let copied = (arguments.len() as u32).next_multiple_of(8);
    let sp = top - copied;
    memory.write_bytes(sp, arguments);
    let mut cpu = Cpu::new();
    cpu.reset_to(thread.entry, sp);
    cpu.regs[0] = arguments.len() as u32;
    cpu.regs[1] = if arguments.is_empty() { 0 } else { sp };
    cpu.regs[LR] = RETURN_TRAP;
    cpu.cp15.thread_id_ro = thread.tls_pointer();
    cpu.vfp.fpscr = FPSCR;

    let running = (Arc::clone(shared), Arc::clone(thread));
    let handle = std::thread::Builder::new()
        .name(thread.name.clone())
        .spawn(move || run(running.0, running.1, cpu, first))
        .map_err(|error| {
            log::error!("could not start a host thread, {error}");
            errors::ERROR
        })?;
    shared.kernel.keep_handle(handle);
    Ok(())
}

/// a thread's life: the functions it was given to call first, then guest
/// code from its entry for as long as it runs, HLE calls when it reaches an
/// import, until it or the process ends.
fn run(shared: Arc<Shared>, thread: Arc<Thread>, mut cpu: Cpu, first: Vec<u32>) {
    log::debug!("thread {} 0x{:08X} starts at 0x{:08X}", thread.name, thread.uid, thread.entry);
    shared.kernel.cores.acquire(&shared);
    let mut stop = None;
    for function in first {
        match call_guest(&shared, &thread, &mut cpu, function, &[0, 0]) {
            Ok(0) => {}
            Ok(result) => log::warn!("{}: 0x{function:08X} returned 0x{result:08X} before the entry", thread.name),
            Err(early) => {
                stop = Some(early);
                break;
            }
        }
    }
    let status = match stop.unwrap_or_else(|| execute(&shared, &thread, &mut cpu)) {
        Stop::Returned(status) => status as i32,
        Stop::ExitThread(status) => status,
        Stop::ExitProcess(status) => {
            shared.finish(Outcome::Exited(status));
            status
        }
        Stop::Stopped => 0,
    };
    shared.kernel.cores.release();
    thread.end(status);
    log::debug!("thread {} ends with {status}", thread.name);
    // the process lasts as long as its first thread
    if thread.main {
        shared.finish(Outcome::Exited(status));
    }
}

/// how guest code stopped running.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stop {
    /// the function it was in returned to the kernel's trap, with r0.
    Returned(u32),
    ExitThread(i32),
    ExitProcess(i32),
    /// the process is ending, or the thread did what ends it.
    Stopped,
}

/// runs guest code on cpu, as the thread, until the function it is in
/// returns to the kernel's trap or the thread or the process ends. the
/// thread holds a core throughout.
pub fn execute(shared: &Arc<Shared>, thread: &Arc<Thread>, cpu: &mut Cpu) -> Stop {
    let mut bus = GuestBus(&shared.memory);
    let mut undefined_seen = std::collections::HashSet::new();
    loop {
        if shared.is_stopping() {
            return Stop::Stopped;
        }
        match cpu.run(&mut bus, SLICE) {
            Exit::Timeout => shared.kernel.cores.checkpoint(shared),
            Exit::Yield => shared.kernel.cores.yield_now(shared),
            Exit::Supervisor(RETURN_SVC) => return Stop::Returned(cpu.regs[0]),
            Exit::Supervisor(index) => match hle::call(shared, thread, cpu, index) {
                Control::Continue => {}
                Control::ExitThread(status) => return Stop::ExitThread(status),
                Control::ExitProcess(status) => return Stop::ExitProcess(status),
            },
            // one bad decode should not end the session
            Exit::Undefined { pc, opcode } => {
                if undefined_seen.insert(pc) {
                    log::warn!("{}: undefined instruction 0x{opcode:08X} at 0x{pc:08X}, skipped", thread.name);
                }
            }
            Exit::Breakpoint { pc, imm } => {
                shared.finish(Outcome::Faulted(format!("{} reached bkpt #{imm} at 0x{pc:08X}", thread.name)));
                return Stop::Stopped;
            }
            Exit::Halted => return Stop::Stopped,
        }
    }
}

/// calls a guest function, odd for Thumb, from the kernel on the thread's
/// own cpu and stack, with arguments as the calling convention passes
/// them, and gives what it returns in r0. the thread's registers are as
/// they were afterwards. a function that ends the thread or the process
/// instead gives how.
pub fn call_guest(
    shared: &Arc<Shared>,
    thread: &Arc<Thread>,
    cpu: &mut Cpu,
    function: u32,
    arguments: &[u32],
) -> Result<u32, Stop> {
    let (registers, cpsr) = (cpu.regs, cpu.cpsr);
    // below whatever the caller has on its stack, eight byte aligned, with
    // the arguments past the fourth
    let extra = arguments.get(4..).unwrap_or_default();
    let sp = (cpu.regs[SP] & !7) - (extra.len() as u32 * 4).next_multiple_of(8);
    for (index, &argument) in extra.iter().enumerate() {
        shared.memory.write32(sp + index as u32 * 4, argument);
    }
    for (index, &argument) in arguments.iter().take(4).enumerate() {
        cpu.regs[index] = argument;
    }
    cpu.regs[SP] = sp;
    cpu.regs[LR] = RETURN_TRAP;
    cpu.regs[PC] = function & !1;
    cpu.cpsr.thumb = function & 1 != 0;
    cpu.cpsr.it = 0;
    let stop = execute(shared, thread, cpu);
    match stop {
        Stop::Returned(result) => {
            cpu.regs = registers;
            cpu.cpsr = cpsr;
            Ok(result)
        }
        _ => Err(stop),
    }
}

/// the cores guest threads take turns on.
pub struct Cores {
    state: Mutex<CoreState>,
    free: Condvar,
}

struct CoreState {
    free: usize,
    waiting: usize,
}

impl Cores {
    pub fn new(count: usize) -> Cores {
        Cores { state: Mutex::new(CoreState { free: count.max(1), waiting: 0 }), free: Condvar::new() }
    }

    /// waits for a core, or for the process to end.
    pub fn acquire(&self, shared: &Shared) {
        let mut state = self.state.lock().unwrap();
        state.waiting += 1;
        while state.free == 0 && !shared.is_stopping() {
            state = self.free.wait(state).unwrap();
        }
        state.waiting -= 1;
        // a thread let through by the end of the process takes no core
        if state.free > 0 {
            state.free -= 1;
        }
    }

    pub fn release(&self) {
        let mut state = self.state.lock().unwrap();
        state.free += 1;
        self.free.notify_one();
    }

    /// at the end of a slice: lets a thread waiting for a core have this
    /// one, if any is.
    pub fn checkpoint(&self, shared: &Shared) {
        if self.state.lock().unwrap().waiting == 0 {
            return;
        }
        self.yield_now(shared);
    }

    /// gives the core up and takes one again, for YIELD and WFE.
    pub fn yield_now(&self, shared: &Shared) {
        self.release();
        std::thread::yield_now();
        self.acquire(shared);
    }

    pub fn wake_everyone(&self) {
        let _state = self.state.lock().unwrap();
        self.free.notify_all();
    }
}
