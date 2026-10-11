//! SceThreadmgr and the parts of SceLibKernel built on it: threads, their
//! storage, sleeping, mutexes and lightweight mutexes, and leaving.

use std::sync::Arc;
use std::time::Duration;

use super::{Call, Function, Timeout};
use crate::kernel::memblock::MAIN;
use crate::kernel::sync::Mutex;
use crate::kernel::thread::{self, State, TLS_SIZE};
use crate::kernel::{errors, Thread};
use crate::Shared;

pub const FUNCTIONS: &[(&str, Function)] = &[
    ("sceKernelGetThreadId", get_thread_id),
    ("sceKernelGetProcessId", get_process_id),
    ("sceKernelDelayThread", delay_thread),
    ("sceKernelDelayThreadCB", delay_thread),
    ("sceKernelExitProcess", exit_process),
    ("sceKernelExitThread", exit_thread),
    ("sceKernelExitDeleteThread", exit_thread),
    ("sceKernelGetTLSAddr", get_tls_addr),
    ("sceKernelGetThreadTLSAddr", get_thread_tls_addr),
    ("sceKernelCreateThread", create_thread),
    ("sceKernelStartThread", start_thread),
    ("sceKernelWaitThreadEnd", wait_thread_end),
    ("sceKernelWaitThreadEndCB", wait_thread_end),
    ("sceKernelDeleteThread", delete_thread),
    ("sceKernelGetThreadInfo", get_thread_info),
    ("sceKernelGetThreadExitStatus", get_thread_exit_status),
    ("sceKernelChangeThreadPriority", change_thread_priority),
    ("sceKernelChangeThreadPriority2", change_thread_priority),
    ("sceKernelGetThreadCurrentPriority", get_thread_current_priority),
    ("sceKernelChangeThreadCpuAffinityMask", change_thread_affinity),
    ("sceKernelGetThreadCpuAffinityMask", get_thread_affinity),
    ("sceKernelGetThreadStackFreeSize", get_thread_stack_free_size),
    ("sceKernelCheckThreadStack", get_thread_stack_free_size),
    ("sceKernelGetCurrentThreadVfpException", vfp_exception),
    ("sceKernelClearThreadVfpException", vfp_exception),
    ("sceKernelYieldThread", yield_thread),
    ("sceKernelCreateMutex", create_mutex),
    ("sceKernelDeleteMutex", delete_mutex),
    ("sceKernelLockMutex", lock_mutex),
    ("sceKernelLockMutexCB", lock_mutex),
    ("sceKernelTryLockMutex", try_lock_mutex),
    ("sceKernelUnlockMutex", unlock_mutex),
    ("sceKernelCreateLwMutex", create_lw_mutex),
    ("sceKernelDeleteLwMutex", delete_lw_mutex),
    ("sceKernelLockLwMutex", lock_lw_mutex),
    ("sceKernelLockLwMutexCB", lock_lw_mutex),
    ("sceKernelTryLockLwMutex", try_lock_lw_mutex),
    ("sceKernelUnlockLwMutex", unlock_lw_mutex),
];

/// the priority a thread gets when it asks for the default one.
pub const DEFAULT_PRIORITY: u32 = 0x1000_0100;
/// the stack a thread gets when its creator gives no size.
pub const DEFAULT_STACK_SIZE: u32 = 0x4_0000;
/// the process's ID, the only one there is.
const PROCESS_ID: u32 = 0x0001_0001;

/// sceKernelGetThreadId() -> UID
fn get_thread_id(call: &mut Call) {
    call.ret(call.thread.uid);
}

/// sceKernelGetProcessId() -> UID
fn get_process_id(call: &mut Call) {
    call.ret(PROCESS_ID);
}

/// sceKernelDelayThread(microseconds), and its variant that would run
/// callbacks, of which there are none yet.
fn delay_thread(call: &mut Call) {
    let microseconds = call.u32();
    let thread = Arc::clone(call.thread);
    thread.nap(call.shared, Duration::from_micros(microseconds as u64));
    call.ret(0u32);
}

/// sceKernelExitProcess(status), which does not return.
fn exit_process(call: &mut Call) {
    let status = call.i32();
    call.exit_process(status);
}

/// sceKernelExitThread(status) and sceKernelExitDeleteThread(status).
fn exit_thread(call: &mut Call) {
    let status = call.i32();
    call.exit_thread(status);
}

/// sceKernelGetTLSAddr(key) -> address of the key's word in this thread's
/// storage, of which there are 0x100.
fn get_tls_addr(call: &mut Call) {
    let key = call.u32();
    let thread = call.thread;
    call.ret(if key < TLS_SIZE / 4 { thread.tls + 4 * key } else { 0 });
}

/// sceKernelGetThreadTLSAddr(thread, key), another thread's, 0 for this one.
fn get_thread_tls_addr(call: &mut Call) {
    let (uid, key) = (call.u32(), call.u32());
    let thread = if uid == 0 { Some(Arc::clone(call.thread)) } else { call.shared.kernel.thread(uid) };
    let address = match thread {
        Some(thread) if key < TLS_SIZE / 4 => thread.tls + 4 * key,
        _ => 0,
    };
    call.ret(address);
}

/// a thread's stack and storage, taken from main memory and committed, the
/// stack with an unmapped page below it to catch an overflow.
#[allow(clippy::too_many_arguments)]
pub fn make_thread(
    shared: &Arc<Shared>,
    name: &str,
    entry: u32,
    priority: u32,
    stack_size: u32,
    affinity: u32,
    attributes: u32,
    main: bool,
) -> Result<Arc<Thread>, u32> {
    let kernel = &shared.kernel;
    let stack_size = stack_size.next_multiple_of(0x1000);
    let guard = 0x1000;
    let (stack, tls) = {
        let mut blocks = kernel.blocks.lock().unwrap();
        let below = blocks.take(MAIN, guard + stack_size, 0x1000).ok_or(errors::NO_FREE_PHYSICAL_PAGE)?;
        let tls = match blocks.take(MAIN, 0x1000, 0x1000) {
            Some(tls) => tls,
            None => {
                blocks.untake(below);
                return Err(errors::NO_FREE_PHYSICAL_PAGE);
            }
        };
        (below + guard, tls)
    };
    if !shared.memory.commit(stack, stack_size) || !shared.memory.commit(tls, 0x1000) {
        return Err(errors::NO_FREE_PHYSICAL_PAGE);
    }
    let uid = kernel.uid();
    let thread =
        Arc::new(Thread::new(uid, name, entry, priority, affinity, attributes, (stack, stack_size), tls, main));
    kernel.threads.lock().unwrap().insert(uid, Arc::clone(&thread));
    log::debug!("thread {name} 0x{uid:08X}: stack 0x{stack:08X} 0x{stack_size:X} bytes, storage 0x{tls:08X}");
    Ok(thread)
}

/// sceKernelCreateThread(name, entry, priority, stack size, attributes,
/// affinity, option) -> UID, dormant until started.
fn create_thread(call: &mut Call) {
    let (name, entry, priority, stack_size) = (call.string(), call.u32(), call.u32(), call.u32());
    let (attributes, affinity, _option) = (call.u32(), call.u32(), call.u32());
    let name = name.unwrap_or_default();
    if stack_size != 0 && stack_size < 0x1000 {
        return call.ret(errors::ILLEGAL_STACK_SIZE);
    }
    let stack_size = if stack_size == 0 { DEFAULT_STACK_SIZE } else { stack_size };
    let made = make_thread(call.shared, &name, entry, priority, stack_size, affinity, attributes, false);
    call.result(made.map(|thread| thread.uid));
}

/// sceKernelStartThread(uid, argument size, arguments)
fn start_thread(call: &mut Call) {
    let (uid, size, pointer) = (call.u32(), call.u32(), call.u32());
    let Some(thread) = call.shared.kernel.thread(uid) else {
        return call.ret(errors::UNKNOWN_THREAD_ID);
    };
    let arguments = if size == 0 || pointer == 0 {
        Vec::new()
    } else {
        call.memory().bytes(pointer, size as usize).unwrap_or_default()
    };
    call.result(thread::start(call.shared, &thread, &arguments).map(|()| 0));
}

/// sceKernelWaitThreadEnd(uid, &status, &timeout)
fn wait_thread_end(call: &mut Call) {
    let (uid, status) = (call.u32(), call.u32());
    let timeout = Timeout::read(call);
    let Some(thread) = call.shared.kernel.thread(uid) else {
        return call.ret(errors::UNKNOWN_THREAD_ID);
    };
    let ended = thread.wait_end(call.shared, timeout.limit());
    timeout.finish(call);
    match ended {
        Some(ended) => {
            if status != 0 {
                call.memory().write32(status, ended as u32);
            }
            call.ret(0u32);
        }
        None if call.shared.is_stopping() => call.ret(errors::WAIT_DELETE),
        None => call.ret(errors::WAIT_TIMEOUT),
    }
}

/// the thread a UID names, the calling one for 0.
fn thread_or_me(call: &Call, uid: u32) -> Option<Arc<Thread>> {
    if uid == 0 || uid == call.thread.uid {
        Some(Arc::clone(call.thread))
    } else {
        call.shared.kernel.thread(uid)
    }
}

/// what SceKernelThreadInfo says of a thread's state: running, dormant or
/// ended, as SCE_THREAD_RUNNING, _DORMANT and _DEAD.
fn status(thread: &Thread) -> u32 {
    match thread.state() {
        State::Running => 0x1,
        State::Dormant => 0x10,
        State::Dead(_) => 0x40,
    }
}

/// sceKernelGetThreadInfo(UID, info), 0 for the calling thread.
fn get_thread_info(call: &mut Call) {
    let (uid, info) = (call.u32(), call.u32());
    let Some(thread) = thread_or_me(call, uid) else {
        return call.ret(errors::UNKNOWN_THREAD_ID);
    };
    let memory = call.memory();
    let size = memory.read32(info);
    if size < 0x80 {
        return call.ret(errors::ILLEGAL_SIZE);
    }
    memory.fill(info + 4, 0, 0x7C);
    memory.write32(info + 4, PROCESS_ID);
    let mut name = thread.name.as_bytes().to_vec();
    name.truncate(31);
    memory.write_bytes(info + 8, &name);
    let exit = match thread.state() {
        State::Dead(status) => status as u32,
        _ => 0,
    };
    let fields = [
        (0x28, thread.attributes),
        (0x2C, status(&thread)),
        (0x30, thread.entry),
        (0x34, thread.stack),
        (0x38, thread.stack_size),
        (0x3C, thread.priority),
        (0x40, thread.current_priority()),
        (0x44, thread.affinity),
        (0x48, thread.current_affinity()),
        (0x5C, exit),
    ];
    for (offset, value) in fields {
        memory.write32(info + offset, value);
    }
    call.ret(0u32);
}

/// sceKernelGetThreadExitStatus(UID, &status), refused while it runs.
fn get_thread_exit_status(call: &mut Call) {
    let (uid, out) = (call.u32(), call.u32());
    let Some(thread) = thread_or_me(call, uid) else {
        return call.ret(errors::UNKNOWN_THREAD_ID);
    };
    match thread.state() {
        State::Dead(status) => {
            if out != 0 {
                call.memory().write32(out, status as u32);
            }
            call.ret(0u32);
        }
        State::Dormant => call.ret(errors::DORMANT),
        State::Running => call.ret(errors::NOT_DORMANT),
    }
}

/// sceKernelChangeThreadPriority(UID, priority), 0 for the calling thread;
/// the host schedules the threads, so it is only kept.
fn change_thread_priority(call: &mut Call) {
    let (uid, priority) = (call.u32(), call.u32());
    let Some(thread) = thread_or_me(call, uid) else {
        return call.ret(errors::UNKNOWN_THREAD_ID);
    };
    thread.set_priority(if priority == 0 { DEFAULT_PRIORITY } else { priority });
    call.ret(0u32);
}

/// sceKernelGetThreadCurrentPriority()
fn get_thread_current_priority(call: &mut Call) {
    let priority = call.thread.current_priority();
    call.ret(priority);
}

/// sceKernelChangeThreadCpuAffinityMask(UID, mask), kept likewise.
fn change_thread_affinity(call: &mut Call) {
    let (uid, mask) = (call.u32(), call.u32());
    let Some(thread) = thread_or_me(call, uid) else {
        return call.ret(errors::UNKNOWN_THREAD_ID);
    };
    thread.set_affinity(mask);
    call.ret(0u32);
}

/// sceKernelGetThreadCpuAffinityMask(UID) -> mask
fn get_thread_affinity(call: &mut Call) {
    let uid = call.u32();
    let mask = thread_or_me(call, uid).map(|thread| thread.current_affinity());
    call.ret(mask.unwrap_or(errors::UNKNOWN_THREAD_ID));
}

/// sceKernelGetThreadStackFreeSize(UID) -> bytes below the stack pointer,
/// known for the calling thread and taken as the whole stack for another.
fn get_thread_stack_free_size(call: &mut Call) {
    let uid = call.u32();
    let Some(thread) = thread_or_me(call, uid) else {
        return call.ret(errors::UNKNOWN_THREAD_ID);
    };
    let free = if thread.uid == call.thread.uid {
        call.cpu.regs[kimika_cpu::SP].saturating_sub(thread.stack)
    } else {
        thread.stack_size
    };
    call.ret(free);
}

/// sceKernelGetCurrentThreadVfpException() and its clear: floating point
/// exceptions are never trapped here, so there are none.
fn vfp_exception(call: &mut Call) {
    call.ret(0u32);
}

/// sceKernelYieldThread(), the core given to another thread for a moment.
fn yield_thread(call: &mut Call) {
    call.shared.kernel.cores.yield_now(call.shared);
    call.ret(0u32);
}

/// sceKernelDeleteThread(uid), a thread that is dormant or has ended.
fn delete_thread(call: &mut Call) {
    let uid = call.u32();
    let removed = call.shared.kernel.threads.lock().unwrap().remove(&uid);
    call.ret(if removed.is_some() { 0 } else { errors::UNKNOWN_THREAD_ID });
}

// -- mutexes ---

/// sceKernelCreateMutex(name, attributes, initial count, option) -> UID,
/// held by its creator when the count is not zero.
fn create_mutex(call: &mut Call) {
    let (name, attributes, count) = (call.string(), call.u32(), call.u32());
    let kernel = &call.shared.kernel;
    let uid = kernel.uid();
    let mutex = Mutex::new(uid, &name.unwrap_or_default(), attributes, call.thread.uid, count);
    kernel.change(|objects| objects.mutexes.insert(uid, mutex));
    call.ret(uid);
}

/// sceKernelDeleteMutex(uid), waking whoever waits on it.
fn delete_mutex(call: &mut Call) {
    let uid = call.u32();
    let removed = call.shared.kernel.change(|objects| objects.mutexes.remove(&uid));
    call.ret(if removed.is_some() { 0 } else { errors::UNKNOWN_MUTEX_ID });
}

/// sceKernelLockMutex(uid, count, &timeout)
fn lock_mutex(call: &mut Call) {
    let (uid, count) = (call.u32(), call.u32());
    let timeout = Timeout::read(call);
    let me = call.thread.uid;
    let result =
        call.shared.kernel.wait_for(call.shared, timeout.limit(), |objects| match objects.mutexes.get_mut(&uid) {
            Some(mutex) => mutex.try_lock(me, count, errors::MUTEX_RECURSIVE),
            None => Some(Err(errors::UNKNOWN_MUTEX_ID)),
        });
    timeout.finish(call);
    call.result(result.map(|()| 0));
}

/// sceKernelTryLockMutex(uid, count)
fn try_lock_mutex(call: &mut Call) {
    let (uid, count) = (call.u32(), call.u32());
    let me = call.thread.uid;
    let result = call.shared.kernel.change(|objects| match objects.mutexes.get_mut(&uid) {
        Some(mutex) => mutex.try_lock(me, count, errors::MUTEX_RECURSIVE).unwrap_or(Err(errors::MUTEX_FAILED_TO_OWN)),
        None => Err(errors::UNKNOWN_MUTEX_ID),
    });
    call.result(result.map(|()| 0));
}

/// sceKernelUnlockMutex(uid, count)
fn unlock_mutex(call: &mut Call) {
    let (uid, count) = (call.u32(), call.u32());
    let me = call.thread.uid;
    let result = call.shared.kernel.change(|objects| match objects.mutexes.get_mut(&uid) {
        Some(mutex) => mutex.unlock(me, count, errors::MUTEX_NOT_OWNED, errors::MUTEX_UNLOCK_UDF),
        None => Err(errors::UNKNOWN_MUTEX_ID),
    });
    call.result(result.map(|()| 0));
}

// -- lightweight mutexes ---

/// sceKernelCreateLwMutex(work area, name, attributes, initial count,
/// option), the mutex kept here by its work area's address, which also gets
/// its UID at +0x10 and attributes at +0xC, as the console keeps them.
fn create_lw_mutex(call: &mut Call) {
    let (work, name, attributes, count) = (call.u32(), call.string(), call.u32(), call.u32());
    if work == 0 {
        return call.ret(errors::ILLEGAL_ADDR);
    }
    let kernel = &call.shared.kernel;
    let uid = kernel.uid();
    let mutex = Mutex::new(uid, &name.unwrap_or_default(), attributes, call.thread.uid, count);
    kernel.change(|objects| objects.lw_mutexes.insert(work, mutex));
    let memory = call.memory();
    memory.fill(work, 0, 0x20);
    memory.write32(work + 0xC, attributes);
    memory.write32(work + 0x10, uid);
    call.ret(0u32);
}

/// sceKernelDeleteLwMutex(work area)
fn delete_lw_mutex(call: &mut Call) {
    let work = call.u32();
    let removed = call.shared.kernel.change(|objects| objects.lw_mutexes.remove(&work));
    call.ret(if removed.is_some() { 0 } else { errors::UNKNOWN_LW_MUTEX_ID });
}

/// sceKernelLockLwMutex(work area, count, &timeout)
fn lock_lw_mutex(call: &mut Call) {
    let (work, count) = (call.u32(), call.u32());
    let timeout = Timeout::read(call);
    let me = call.thread.uid;
    let result =
        call.shared.kernel.wait_for(call.shared, timeout.limit(), |objects| match objects.lw_mutexes.get_mut(&work) {
            Some(mutex) => mutex.try_lock(me, count, errors::LW_MUTEX_RECURSIVE),
            None => Some(Err(errors::UNKNOWN_LW_MUTEX_ID)),
        });
    timeout.finish(call);
    call.result(result.map(|()| 0));
}

/// sceKernelTryLockLwMutex(work area, count)
fn try_lock_lw_mutex(call: &mut Call) {
    let (work, count) = (call.u32(), call.u32());
    let me = call.thread.uid;
    let result = call.shared.kernel.change(|objects| match objects.lw_mutexes.get_mut(&work) {
        Some(mutex) => {
            mutex.try_lock(me, count, errors::LW_MUTEX_RECURSIVE).unwrap_or(Err(errors::LW_MUTEX_FAILED_TO_OWN))
        }
        None => Err(errors::UNKNOWN_LW_MUTEX_ID),
    });
    call.result(result.map(|()| 0));
}

/// sceKernelUnlockLwMutex(work area, count)
fn unlock_lw_mutex(call: &mut Call) {
    let (work, count) = (call.u32(), call.u32());
    let me = call.thread.uid;
    let result = call.shared.kernel.change(|objects| match objects.lw_mutexes.get_mut(&work) {
        Some(mutex) => mutex.unlock(me, count, errors::LW_MUTEX_NOT_OWNED, errors::LW_MUTEX_UNLOCK_UDF),
        None => Err(errors::UNKNOWN_LW_MUTEX_ID),
    });
    call.result(result.map(|()| 0));
}
