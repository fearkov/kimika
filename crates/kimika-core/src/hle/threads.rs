//! SceThreadmgr and the parts of SceLibKernel built on it: threads, their
//! storage, sleeping, mutexes and lightweight mutexes, and leaving.

use std::sync::Arc;
use std::time::Duration;

use super::{Call, Function};
use crate::kernel::memblock::MAIN;
use crate::kernel::sync::Mutex;
use crate::kernel::thread::{self, TLS_SIZE};
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
    let (uid, status, timeout) = (call.u32(), call.u32(), call.u32());
    let Some(thread) = call.shared.kernel.thread(uid) else {
        return call.ret(errors::UNKNOWN_THREAD_ID);
    };
    let limit = (timeout != 0).then(|| Duration::from_micros(call.memory().read32(timeout) as u64));
    match thread.wait_end(call.shared, limit) {
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

/// sceKernelDeleteThread(uid), a thread that is dormant or has ended.
fn delete_thread(call: &mut Call) {
    let uid = call.u32();
    let removed = call.shared.kernel.threads.lock().unwrap().remove(&uid);
    call.ret(if removed.is_some() { 0 } else { errors::UNKNOWN_THREAD_ID });
}

/// a timeout argument, a pointer to microseconds, none for NULL.
fn timeout(call: &mut Call) -> Option<Duration> {
    match call.u32() {
        0 => None,
        pointer => Some(Duration::from_micros(call.memory().read32(pointer) as u64)),
    }
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
    let limit = timeout(call);
    let me = call.thread.uid;
    let result = call.shared.kernel.wait_for(call.shared, limit, |objects| match objects.mutexes.get_mut(&uid) {
        Some(mutex) => mutex.try_lock(me, count, errors::MUTEX_RECURSIVE),
        None => Some(Err(errors::UNKNOWN_MUTEX_ID)),
    });
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
    let limit = timeout(call);
    let me = call.thread.uid;
    let result = call.shared.kernel.wait_for(call.shared, limit, |objects| match objects.lw_mutexes.get_mut(&work) {
        Some(mutex) => mutex.try_lock(me, count, errors::LW_MUTEX_RECURSIVE),
        None => Some(Err(errors::UNKNOWN_LW_MUTEX_ID)),
    });
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
