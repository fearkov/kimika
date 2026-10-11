//! SceFiber: coroutines a thread runs on stacks of their own, switching
//! between them by hand. a switch saves the registers of what was running
//! and puts those of what runs next in the thread's CPU, so that when the
//! call returns it returns into the other one.

use std::collections::HashMap;

use kimika_cpu::{Cpu, Psr, Vfp, LR, PC, SP};

use super::{Call, Function};
use crate::kernel::Uid;

pub const FUNCTIONS: &[(&str, Function)] = &[
    ("_sceFiberInitializeImpl", initialize),
    ("sceFiberOptParamInitialize", opt_param_initialize),
    ("sceFiberFinalize", finalize),
    ("sceFiberRun", run),
    ("sceFiberSwitch", switch),
    ("sceFiberGetSelf", get_self),
    ("sceFiberReturnToThread", return_to_thread),
    ("sceFiberGetInfo", get_info),
];

const NULL: u32 = 0x8059_0001;
const ALIGNMENT: u32 = 0x8059_0002;
const RANGE: u32 = 0x8059_0003;
const INVALID: u32 = 0x8059_0004;
const PERMISSION: u32 = 0x8059_0005;
const BUSY: u32 = 0x8059_0007;

/// the registers a fiber or a thread left off with.
#[derive(Clone)]
struct Saved {
    regs: [u32; 16],
    cpsr: Psr,
    vfp: Vfp,
}

impl Saved {
    fn take(cpu: &Cpu) -> Saved {
        Saved { regs: cpu.regs, cpsr: cpu.cpsr, vfp: cpu.vfp.clone() }
    }

    fn restore(&self, cpu: &mut Cpu) {
        cpu.regs = self.regs;
        cpu.cpsr = self.cpsr;
        cpu.vfp = self.vfp.clone();
    }
}

struct Fiber {
    name: String,
    entry: u32,
    argument: u32,
    stack: u32,
    stack_size: u32,
    /// where it left off, none before it first runs and after it ends.
    saved: Option<Saved>,
    /// where the call it left off in wants the argument it is resumed
    /// with.
    resume_out: u32,
    running: bool,
}

/// a thread running fibers: the one it runs now, and where the thread
/// itself left off in sceFiberRun.
struct Running {
    fiber: u32,
    thread: Saved,
    return_out: u32,
}

#[derive(Default)]
pub struct Fibers {
    fibers: HashMap<u32, Fiber>,
    running: HashMap<Uid, Running>,
    /// the kernel's code a fiber returns to when its entry does.
    exit: u32,
}

/// _sceFiberInitializeImpl(fiber, name, entry, argument, stack, stack
/// size, options)
fn initialize(call: &mut Call) {
    let (fiber, name, entry, argument) = (call.u32(), call.string(), call.u32(), call.u32());
    let (stack, stack_size, _options) = (call.u32(), call.u32(), call.u32());
    if fiber == 0 || entry == 0 || stack == 0 {
        return call.ret(NULL);
    }
    if fiber % 8 != 0 || stack % 8 != 0 {
        return call.ret(ALIGNMENT);
    }
    if stack_size < 512 {
        return call.ret(RANGE);
    }
    let exit = {
        let fibers = call.shared.hle.fibers.lock().unwrap();
        fibers.exit
    };
    if exit == 0 {
        let Some(exit) = super::trampoline(call.shared, fiber_exit) else {
            return call.ret(INVALID);
        };
        call.shared.hle.fibers.lock().unwrap().exit = exit;
    }
    call.memory().fill(fiber, 0, 0x80);
    let state = Fiber {
        name: name.unwrap_or_default(),
        entry,
        argument,
        stack,
        stack_size,
        saved: None,
        resume_out: 0,
        running: false,
    };
    call.shared.hle.fibers.lock().unwrap().fibers.insert(fiber, state);
    call.ret(0u32);
}

/// sceFiberOptParamInitialize(options)
fn opt_param_initialize(call: &mut Call) {
    let options = call.u32();
    if options == 0 {
        return call.ret(NULL);
    }
    call.memory().fill(options, 0, 0x80);
    call.ret(0u32);
}

/// sceFiberFinalize(fiber), not while it runs.
fn finalize(call: &mut Call) {
    let fiber = call.u32();
    let mut fibers = call.shared.hle.fibers.lock().unwrap();
    match fibers.fibers.get(&fiber) {
        None => call.ret(INVALID),
        Some(state) if state.running => call.ret(BUSY),
        Some(_) => {
            fibers.fibers.remove(&fiber);
            call.ret(0u32);
        }
    }
}

/// puts a fiber's registers in the CPU: where it left off, the call it
/// left off in returning 0 with argument as what it is resumed with, or
/// its entry, with its own argument and this one, the first time.
fn enter(fibers: &mut Fibers, cpu: &mut Cpu, memory: &crate::memory::Memory, fiber: u32, argument: u32) {
    let exit = fibers.exit;
    let state = fibers.fibers.get_mut(&fiber).expect("a fiber checked to be there");
    state.running = true;
    match &state.saved {
        Some(saved) => {
            saved.restore(cpu);
            cpu.regs[0] = 0;
            if state.resume_out != 0 {
                memory.write32(state.resume_out, argument);
            }
        }
        None => {
            cpu.regs = [0; 16];
            cpu.regs[0] = state.argument;
            cpu.regs[1] = argument;
            cpu.regs[SP] = (state.stack + state.stack_size) & !7;
            cpu.regs[LR] = exit;
            cpu.regs[PC] = state.entry & !1;
            cpu.cpsr = Psr { thumb: state.entry & 1 != 0, ..Psr::default() };
        }
    }
}

/// sceFiberRun(fiber, argument, &returned): from a thread, into a fiber
/// until it gives the thread back.
fn run(call: &mut Call) {
    let (fiber, argument, returned) = (call.u32(), call.u32(), call.u32());
    let me = call.thread.uid;
    let memory = call.memory();
    let mut fibers = call.shared.hle.fibers.lock().unwrap();
    if fibers.running.contains_key(&me) {
        return call.ret(PERMISSION);
    }
    match fibers.fibers.get(&fiber) {
        None => return call.ret(INVALID),
        Some(state) if state.running => return call.ret(BUSY),
        Some(_) => {}
    }
    let mut thread = Saved::take(call.cpu);
    thread.regs[0] = 0;
    fibers.running.insert(me, Running { fiber, thread, return_out: returned });
    enter(&mut fibers, call.cpu, memory, fiber, argument);
}

/// sceFiberSwitch(fiber, argument, &resumed): from one fiber into another.
fn switch(call: &mut Call) {
    let (target, argument, resumed) = (call.u32(), call.u32(), call.u32());
    let me = call.thread.uid;
    let memory = call.memory();
    let mut fibers = call.shared.hle.fibers.lock().unwrap();
    let Some(current) = fibers.running.get(&me).map(|running| running.fiber) else {
        return call.ret(PERMISSION);
    };
    match fibers.fibers.get(&target) {
        None => return call.ret(INVALID),
        Some(state) if state.running && target != current => return call.ret(BUSY),
        Some(_) => {}
    }
    let saved = Saved::take(call.cpu);
    if let Some(state) = fibers.fibers.get_mut(&current) {
        state.saved = Some(saved);
        state.resume_out = resumed;
        state.running = false;
    }
    if let Some(running) = fibers.running.get_mut(&me) {
        running.fiber = target;
    }
    enter(&mut fibers, call.cpu, memory, target, argument);
}

/// gives the thread back its registers as sceFiberRun left them, the
/// argument it returns with written where it asked.
fn back_to_thread(fibers: &mut Fibers, call: &mut Call, argument: u32) {
    let me = call.thread.uid;
    if let Some(running) = fibers.running.remove(&me) {
        running.thread.restore(call.cpu);
        if running.return_out != 0 {
            call.memory().write32(running.return_out, argument);
        }
    }
}

/// sceFiberReturnToThread(argument, &resumed): from a fiber back to the
/// thread that ran it, to be resumed later where it is.
fn return_to_thread(call: &mut Call) {
    let (argument, resumed) = (call.u32(), call.u32());
    let me = call.thread.uid;
    let shared = call.shared;
    let mut fibers = shared.hle.fibers.lock().unwrap();
    let Some(current) = fibers.running.get(&me).map(|running| running.fiber) else {
        return call.ret(PERMISSION);
    };
    let saved = Saved::take(call.cpu);
    if let Some(state) = fibers.fibers.get_mut(&current) {
        state.saved = Some(saved);
        state.resume_out = resumed;
        state.running = false;
    }
    back_to_thread(&mut fibers, call, argument);
}

/// where a fiber's entry returns to: the fiber ends, and the thread goes
/// on from sceFiberRun.
fn fiber_exit(call: &mut Call) {
    let me = call.thread.uid;
    let shared = call.shared;
    let mut fibers = shared.hle.fibers.lock().unwrap();
    let Some(current) = fibers.running.get(&me).map(|running| running.fiber) else {
        return;
    };
    if let Some(state) = fibers.fibers.get_mut(&current) {
        state.saved = None;
        state.running = false;
    }
    back_to_thread(&mut fibers, call, 0);
}

/// sceFiberGetSelf(&fiber), the fiber the thread runs now.
fn get_self(call: &mut Call) {
    let out = call.u32();
    let current = call.shared.hle.fibers.lock().unwrap().running.get(&call.thread.uid).map(|running| running.fiber);
    match current {
        Some(fiber) => {
            if out != 0 {
                call.memory().write32(out, fiber);
            }
            call.ret(0u32);
        }
        None => call.ret(PERMISSION),
    }
}

/// sceFiberGetInfo(fiber, &info)
fn get_info(call: &mut Call) {
    let (fiber, info) = (call.u32(), call.u32());
    let fibers = call.shared.hle.fibers.lock().unwrap();
    let Some(state) = fibers.fibers.get(&fiber) else {
        return call.ret(INVALID);
    };
    let memory = call.memory();
    memory.fill(info, 0, 0x170);
    for (offset, value) in [(0, state.entry), (4, state.argument), (8, state.stack), (12, state.stack_size)] {
        memory.write32(info + offset, value);
    }
    let mut name = state.name.as_bytes().to_vec();
    name.truncate(31);
    memory.write_bytes(info + 16, &name);
    call.ret(0u32);
}
