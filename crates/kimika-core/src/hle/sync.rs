//! SceThreadmgr's other objects: semaphores, event flags, condition
//! variables and their lightweight kind, message pipes and callbacks.
//!
//! a wait gives up the thread's core and sleeps until a change to any
//! object lets it go, as [crate::kernel::Kernel::wait_for] does, and a
//! timeout is written back with what is left of it.

use super::{Call, Function, Timeout};
use crate::kernel::errors;
use crate::kernel::sync::{
    Callback, Cond, EventFlag, Objects, Pipe, Semaphore, Signal, WAIT_CLEAR, WAIT_CLEAR_PATTERN, WAIT_MULTIPLE, WAIT_OR,
};

pub const FUNCTIONS: &[(&str, Function)] = &[
    ("sceKernelCreateSema", create_semaphore),
    ("sceKernelDeleteSema", delete_semaphore),
    ("sceKernelSignalSema", signal_semaphore),
    ("sceKernelWaitSema", wait_semaphore),
    ("sceKernelWaitSemaCB", wait_semaphore),
    ("sceKernelPollSema", poll_semaphore),
    ("sceKernelCancelSema", cancel_semaphore),
    ("sceKernelGetSemaInfo", get_semaphore_info),
    ("sceKernelCreateEventFlag", create_event_flag),
    ("sceKernelDeleteEventFlag", delete_event_flag),
    ("sceKernelSetEventFlag", set_event_flag),
    ("sceKernelClearEventFlag", clear_event_flag),
    ("sceKernelWaitEventFlag", wait_event_flag),
    ("sceKernelWaitEventFlagCB", wait_event_flag),
    ("sceKernelPollEventFlag", poll_event_flag),
    ("sceKernelCancelEventFlag", cancel_event_flag),
    ("sceKernelGetEventFlagInfo", get_event_flag_info),
    ("sceKernelCreateCond", create_cond),
    ("sceKernelDeleteCond", delete_cond),
    ("sceKernelWaitCond", wait_cond),
    ("sceKernelWaitCondCB", wait_cond),
    ("sceKernelSignalCond", signal_cond),
    ("sceKernelSignalCondAll", signal_cond_all),
    ("sceKernelSignalCondTo", signal_cond_to),
    ("sceKernelCreateLwCond", create_lw_cond),
    ("sceKernelDeleteLwCond", delete_lw_cond),
    ("sceKernelWaitLwCond", wait_lw_cond),
    ("sceKernelWaitLwCondCB", wait_lw_cond),
    ("sceKernelSignalLwCond", signal_lw_cond),
    ("sceKernelSignalLwCondAll", signal_lw_cond_all),
    ("sceKernelSignalLwCondTo", signal_lw_cond_to),
    ("sceKernelCreateMsgPipe", create_pipe),
    ("sceKernelDeleteMsgPipe", delete_pipe),
    ("sceKernelSendMsgPipe", send_pipe),
    ("sceKernelSendMsgPipeCB", send_pipe),
    ("sceKernelTrySendMsgPipe", try_send_pipe),
    ("sceKernelReceiveMsgPipe", receive_pipe),
    ("sceKernelReceiveMsgPipeCB", receive_pipe),
    ("sceKernelTryReceiveMsgPipe", try_receive_pipe),
    ("sceKernelCreateCallback", create_callback),
    ("sceKernelDeleteCallback", delete_callback),
    ("sceKernelNotifyCallback", notify_callback),
    ("sceKernelCancelCallback", cancel_callback),
    ("sceKernelGetCallbackCount", get_callback_count),
    ("sceKernelCheckCallback", check_callback),
];

/// writes a name into a structure, NUL-terminated in 32 bytes.
fn write_name(call: &Call, address: u32, name: &str) {
    let mut bytes = name.as_bytes().to_vec();
    bytes.truncate(31);
    bytes.resize(32, 0);
    call.memory().write_bytes(address, &bytes);
}

// -- semaphores ---

/// sceKernelCreateSema(name, attributes, initial, max, option) -> UID
fn create_semaphore(call: &mut Call) {
    let (name, attributes, initial, max) = (call.string(), call.u32(), call.i32(), call.i32());
    if max <= 0 || initial < 0 || initial > max {
        return call.ret(errors::ILLEGAL_COUNT);
    }
    let kernel = &call.shared.kernel;
    let uid = kernel.uid();
    let semaphore = Semaphore { name: name.unwrap_or_default(), attributes, initial, count: initial, max, waiting: 0 };
    kernel.change(|objects| objects.semaphores.insert(uid, semaphore));
    call.ret(uid);
}

/// sceKernelDeleteSema(UID), waking whoever waits on it.
fn delete_semaphore(call: &mut Call) {
    let uid = call.u32();
    let removed = call.shared.kernel.change(|objects| objects.semaphores.remove(&uid));
    call.ret(if removed.is_some() { 0 } else { errors::UNKNOWN_SEMA_ID });
}

/// sceKernelSignalSema(UID, count)
fn signal_semaphore(call: &mut Call) {
    let (uid, count) = (call.u32(), call.i32());
    let result = call.shared.kernel.change(|objects| {
        let semaphore = objects.semaphores.get_mut(&uid).ok_or(errors::UNKNOWN_SEMA_ID)?;
        if count <= 0 {
            return Err(errors::ILLEGAL_COUNT);
        }
        match semaphore.count.checked_add(count).filter(|&total| total <= semaphore.max) {
            Some(total) => {
                semaphore.count = total;
                Ok(0)
            }
            None => Err(errors::SEMA_OVF),
        }
    });
    call.result(result);
}

/// sceKernelWaitSema(UID, count, timeout)
fn wait_semaphore(call: &mut Call) {
    let (uid, count) = (call.u32(), call.i32());
    let timeout = Timeout::read(call);
    let kernel = &call.shared.kernel;
    let known = kernel.change(|objects| objects.semaphores.get_mut(&uid).map(|semaphore| semaphore.waiting += 1));
    if known.is_none() {
        return call.ret(errors::UNKNOWN_SEMA_ID);
    }
    let result = kernel.wait_for(call.shared, timeout.limit(), |objects| {
        let Some(semaphore) = objects.semaphores.get_mut(&uid) else {
            return Some(Err(errors::WAIT_DELETE));
        };
        if count <= 0 || count > semaphore.max {
            return Some(Err(errors::ILLEGAL_COUNT));
        }
        (semaphore.count >= count).then(|| {
            semaphore.count -= count;
            Ok(0)
        })
    });
    kernel.change(|objects| objects.semaphores.get_mut(&uid).map(|semaphore| semaphore.waiting -= 1));
    timeout.finish(call);
    call.result(result);
}

/// sceKernelPollSema(UID, count)
fn poll_semaphore(call: &mut Call) {
    let (uid, count) = (call.u32(), call.i32());
    let result = call.shared.kernel.change(|objects| {
        let semaphore = objects.semaphores.get_mut(&uid).ok_or(errors::UNKNOWN_SEMA_ID)?;
        if count <= 0 {
            return Err(errors::ILLEGAL_COUNT);
        }
        if semaphore.count < count {
            return Err(errors::SEMA_ZERO);
        }
        semaphore.count -= count;
        Ok(0)
    });
    call.result(result);
}

/// sceKernelCancelSema(UID, count, &waiting): sets the count, -1 for the
/// initial one.
fn cancel_semaphore(call: &mut Call) {
    let (uid, count, waiting) = (call.u32(), call.i32(), call.u32());
    let result = call.shared.kernel.change(|objects| {
        let semaphore = objects.semaphores.get_mut(&uid).ok_or(errors::UNKNOWN_SEMA_ID)?;
        if count > semaphore.max {
            return Err(errors::ILLEGAL_COUNT);
        }
        semaphore.count = if count < 0 { semaphore.initial } else { count };
        Ok(semaphore.waiting)
    });
    if let (Ok(count), true) = (result, waiting != 0) {
        call.memory().write32(waiting, count);
    }
    call.result(result.map(|_| 0));
}

/// sceKernelGetSemaInfo(UID, info)
fn get_semaphore_info(call: &mut Call) {
    let (uid, info) = (call.u32(), call.u32());
    let found = call.shared.kernel.change(|objects| {
        objects.semaphores.get(&uid).map(|semaphore| {
            let numbers = [semaphore.attributes, semaphore.initial as u32, semaphore.count as u32];
            (semaphore.name.clone(), numbers, semaphore.max as u32, semaphore.waiting)
        })
    });
    let Some((name, [attributes, initial, count], max, waiting)) = found else {
        return call.ret(errors::UNKNOWN_SEMA_ID);
    };
    let memory = call.memory();
    memory.write32(info + 4, uid);
    write_name(call, info + 8, &name);
    for (offset, value) in [(0x28, attributes), (0x2C, initial), (0x30, count), (0x34, max), (0x38, waiting)] {
        memory.write32(info + offset, value);
    }
    call.ret(0u32);
}

// -- event flags ---

/// sceKernelCreateEventFlag(name, attributes, pattern, option) -> UID
fn create_event_flag(call: &mut Call) {
    let (name, attributes, pattern) = (call.string(), call.u32(), call.u32());
    let kernel = &call.shared.kernel;
    let uid = kernel.uid();
    let flag = EventFlag { name: name.unwrap_or_default(), attributes, initial: pattern, pattern, waiting: 0 };
    kernel.change(|objects| objects.event_flags.insert(uid, flag));
    call.ret(uid);
}

/// sceKernelDeleteEventFlag(UID), waking whoever waits on it.
fn delete_event_flag(call: &mut Call) {
    let uid = call.u32();
    let removed = call.shared.kernel.change(|objects| objects.event_flags.remove(&uid));
    call.ret(if removed.is_some() { 0 } else { errors::UNKNOWN_EVF_ID });
}

/// sceKernelSetEventFlag(UID, bits), or'd into the pattern.
fn set_event_flag(call: &mut Call) {
    let (uid, bits) = (call.u32(), call.u32());
    let found = call.shared.kernel.change(|objects| objects.event_flags.get_mut(&uid).map(|flag| flag.pattern |= bits));
    call.ret(if found.is_some() { 0 } else { errors::UNKNOWN_EVF_ID });
}

/// sceKernelClearEventFlag(UID, bits), and'd into the pattern: the bits
/// given stay.
fn clear_event_flag(call: &mut Call) {
    let (uid, bits) = (call.u32(), call.u32());
    let found = call.shared.kernel.change(|objects| objects.event_flags.get_mut(&uid).map(|flag| flag.pattern &= bits));
    call.ret(if found.is_some() { 0 } else { errors::UNKNOWN_EVF_ID });
}

fn check_mode(bits: u32, mode: u32) -> Result<(), u32> {
    if bits == 0 {
        return Err(errors::ILLEGAL_PATTERN);
    }
    if mode & !(WAIT_OR | WAIT_CLEAR | WAIT_CLEAR_PATTERN) != 0 {
        return Err(errors::ILLEGAL_MODE);
    }
    Ok(())
}

/// sceKernelWaitEventFlag(UID, bits, mode, &pattern, timeout)
fn wait_event_flag(call: &mut Call) {
    let (uid, bits, mode, out) = (call.u32(), call.u32(), call.u32(), call.u32());
    let timeout = Timeout::read(call);
    if let Err(error) = check_mode(bits, mode) {
        return call.ret(error);
    }
    let kernel = &call.shared.kernel;
    let joined = kernel.change(|objects| {
        let flag = objects.event_flags.get_mut(&uid).ok_or(errors::UNKNOWN_EVF_ID)?;
        if flag.waiting > 0 && flag.attributes & WAIT_MULTIPLE == 0 {
            return Err(errors::EVF_MULTI);
        }
        flag.waiting += 1;
        Ok(())
    });
    if let Err(error) = joined {
        return call.ret(error);
    }
    let result = kernel.wait_for(call.shared, timeout.limit(), |objects| match objects.event_flags.get_mut(&uid) {
        Some(flag) => flag.take(bits, mode).map(Ok),
        None => Some(Err(errors::WAIT_DELETE)),
    });
    let pattern = kernel.change(|objects| {
        objects.event_flags.get_mut(&uid).map(|flag| {
            flag.waiting -= 1;
            flag.pattern
        })
    });
    if out != 0 {
        call.memory().write32(out, result.unwrap_or(pattern.unwrap_or(0)));
    }
    timeout.finish(call);
    call.result(result.map(|_| 0));
}

/// sceKernelPollEventFlag(UID, bits, mode, &pattern)
fn poll_event_flag(call: &mut Call) {
    let (uid, bits, mode, out) = (call.u32(), call.u32(), call.u32(), call.u32());
    if let Err(error) = check_mode(bits, mode) {
        return call.ret(error);
    }
    let result = call.shared.kernel.change(|objects| {
        let flag = objects.event_flags.get_mut(&uid).ok_or(errors::UNKNOWN_EVF_ID)?;
        flag.take(bits, mode).ok_or(errors::EVF_COND)
    });
    if let (Ok(pattern), true) = (result, out != 0) {
        call.memory().write32(out, pattern);
    }
    call.result(result.map(|_| 0));
}

/// sceKernelCancelEventFlag(UID, pattern, &waiting)
fn cancel_event_flag(call: &mut Call) {
    let (uid, pattern, waiting) = (call.u32(), call.u32(), call.u32());
    let result = call.shared.kernel.change(|objects| {
        let flag = objects.event_flags.get_mut(&uid).ok_or(errors::UNKNOWN_EVF_ID)?;
        flag.pattern = pattern;
        Ok(flag.waiting)
    });
    if let (Ok(count), true) = (result, waiting != 0) {
        call.memory().write32(waiting, count);
    }
    call.result(result.map(|_| 0));
}

/// sceKernelGetEventFlagInfo(UID, info)
fn get_event_flag_info(call: &mut Call) {
    let (uid, info) = (call.u32(), call.u32());
    let found = call.shared.kernel.change(|objects| {
        objects
            .event_flags
            .get(&uid)
            .map(|flag| (flag.name.clone(), [flag.attributes, flag.initial, flag.pattern, flag.waiting]))
    });
    let Some((name, [attributes, initial, pattern, waiting])) = found else {
        return call.ret(errors::UNKNOWN_EVF_ID);
    };
    let memory = call.memory();
    memory.write32(info + 4, uid);
    write_name(call, info + 8, &name);
    for (offset, value) in [(0x28, attributes), (0x2C, initial), (0x30, pattern), (0x34, waiting)] {
        memory.write32(info + offset, value);
    }
    call.ret(0u32);
}

// -- condition variables ---

/// which kind of condition variable, and so of mutex.
#[derive(Clone, Copy)]
enum Kind {
    /// by UID, with a mutex by UID.
    Plain,
    /// by work area, with a lightweight mutex by work area.
    Light,
}

impl Kind {
    fn conds(self, objects: &mut Objects) -> &mut std::collections::HashMap<u32, Cond> {
        match self {
            Kind::Plain => &mut objects.conds,
            Kind::Light => &mut objects.lw_conds,
        }
    }

    fn unknown(self) -> u32 {
        match self {
            Kind::Plain => errors::UNKNOWN_COND_ID,
            Kind::Light => errors::UNKNOWN_LW_COND_ID,
        }
    }

    fn not_owned(self) -> u32 {
        match self {
            Kind::Plain => errors::MUTEX_NOT_OWNED,
            Kind::Light => errors::LW_MUTEX_NOT_OWNED,
        }
    }

    fn recursive(self) -> u32 {
        match self {
            Kind::Plain => errors::MUTEX_RECURSIVE,
            Kind::Light => errors::LW_MUTEX_RECURSIVE,
        }
    }
}

/// a cond and its mutex, both borrowed from the objects.
fn cond_and_mutex(
    objects: &mut Objects,
    kind: Kind,
    key: u32,
) -> Option<(&mut Cond, Option<&mut crate::kernel::sync::Mutex>)> {
    let (conds, mutexes) = match kind {
        Kind::Plain => (&mut objects.conds, &mut objects.mutexes),
        Kind::Light => (&mut objects.lw_conds, &mut objects.lw_mutexes),
    };
    let cond = conds.get_mut(&key)?;
    let mutex = mutexes.get_mut(&cond.mutex);
    Some((cond, mutex))
}

/// waits on a condition variable: lets its mutex go, which the thread has
/// to hold, sleeps until signalled, and takes the mutex back, as many
/// times as it held it, before it goes on, whether it was signalled or
/// its wait ran out.
fn wait_on(call: &mut Call, kind: Kind, key: u32) {
    let timeout = Timeout::read(call);
    let me = call.thread.uid;
    let kernel = &call.shared.kernel;
    let entered = kernel.change(|objects| {
        let (cond, mutex) = cond_and_mutex(objects, kind, key).ok_or(kind.unknown())?;
        let mutex = mutex.ok_or(kind.unknown())?;
        if mutex.owner != Some(me) {
            return Err(kind.not_owned());
        }
        let count = mutex.count;
        mutex.owner = None;
        mutex.count = 0;
        Ok((cond.enqueue(me), count))
    });
    let (ticket, count) = match entered {
        Ok(entered) => entered,
        Err(error) => return call.ret(error),
    };
    let relock = |mutex: Option<&mut crate::kernel::sync::Mutex>| match mutex {
        Some(mutex) => mutex.try_lock(me, count, kind.recursive()),
        None => Some(Err(kind.unknown())),
    };
    let mut result = kernel.wait_for(call.shared, timeout.limit(), |objects| {
        let Some((cond, mutex)) = cond_and_mutex(objects, kind, key) else {
            return Some(Err(errors::WAIT_DELETE));
        };
        if !cond.signalled.contains(&ticket) {
            return None;
        }
        let locked = relock(mutex)?;
        cond.signalled.remove(&ticket);
        Some(locked)
    });
    if result == Err(errors::WAIT_TIMEOUT) {
        // out of time, it still has to have the mutex back
        kernel.change(|objects| {
            if let Some((cond, _)) = cond_and_mutex(objects, kind, key) {
                cond.forget(ticket);
            }
        });
        let relocked = kernel.wait_for(call.shared, None, |objects| match cond_and_mutex(objects, kind, key) {
            Some((_, mutex)) => relock(mutex),
            None => Some(Err(errors::WAIT_DELETE)),
        });
        result = relocked.and(Err(errors::WAIT_TIMEOUT));
    }
    timeout.finish(call);
    call.result(result.map(|()| 0));
}

fn signal(call: &mut Call, kind: Kind, key: u32, which: Signal) {
    let found = call.shared.kernel.change(|objects| kind.conds(objects).get_mut(&key).map(|cond| cond.signal(which)));
    call.ret(match found {
        Some(_) => 0,
        None => kind.unknown(),
    });
}

/// sceKernelCreateCond(name, attributes, mutex, option) -> UID
fn create_cond(call: &mut Call) {
    let (name, attributes, mutex) = (call.string(), call.u32(), call.u32());
    let kernel = &call.shared.kernel;
    let uid = kernel.uid();
    let made = kernel.change(|objects| {
        if !objects.mutexes.contains_key(&mutex) {
            return Err(errors::UNKNOWN_MUTEX_ID);
        }
        objects.conds.insert(uid, Cond::new(&name.unwrap_or_default(), attributes, mutex));
        Ok(uid)
    });
    call.result(made);
}

/// sceKernelDeleteCond(UID)
fn delete_cond(call: &mut Call) {
    let uid = call.u32();
    let removed = call.shared.kernel.change(|objects| objects.conds.remove(&uid));
    call.ret(if removed.is_some() { 0 } else { errors::UNKNOWN_COND_ID });
}

/// sceKernelWaitCond(UID, timeout)
fn wait_cond(call: &mut Call) {
    let uid = call.u32();
    wait_on(call, Kind::Plain, uid);
}

/// sceKernelSignalCond(UID), the first thread waiting.
fn signal_cond(call: &mut Call) {
    let uid = call.u32();
    signal(call, Kind::Plain, uid, Signal::One);
}

/// sceKernelSignalCondAll(UID)
fn signal_cond_all(call: &mut Call) {
    let uid = call.u32();
    signal(call, Kind::Plain, uid, Signal::All);
}

/// sceKernelSignalCondTo(UID, thread)
fn signal_cond_to(call: &mut Call) {
    let (uid, thread) = (call.u32(), call.u32());
    signal(call, Kind::Plain, uid, Signal::To(thread));
}

/// sceKernelCreateLwCond(work area, name, attributes, lightweight mutex
/// work area, option), the condition variable kept here by its work area,
/// which gets its UID at +0x10 as a lightweight mutex's does.
fn create_lw_cond(call: &mut Call) {
    let (work, name, attributes, mutex) = (call.u32(), call.string(), call.u32(), call.u32());
    if work == 0 {
        return call.ret(errors::ILLEGAL_ADDR);
    }
    let kernel = &call.shared.kernel;
    let uid = kernel.uid();
    let made = kernel.change(|objects| {
        if !objects.lw_mutexes.contains_key(&mutex) {
            return Err(errors::UNKNOWN_LW_MUTEX_ID);
        }
        objects.lw_conds.insert(work, Cond::new(&name.unwrap_or_default(), attributes, mutex));
        Ok(0)
    });
    if made.is_ok() {
        let memory = call.memory();
        memory.fill(work, 0, 0x20);
        memory.write32(work + 4, mutex);
        memory.write32(work + 0x10, uid);
    }
    call.result(made);
}

/// sceKernelDeleteLwCond(work area)
fn delete_lw_cond(call: &mut Call) {
    let work = call.u32();
    let removed = call.shared.kernel.change(|objects| objects.lw_conds.remove(&work));
    call.ret(if removed.is_some() { 0 } else { errors::UNKNOWN_LW_COND_ID });
}

/// sceKernelWaitLwCond(work area, timeout)
fn wait_lw_cond(call: &mut Call) {
    let work = call.u32();
    wait_on(call, Kind::Light, work);
}

/// sceKernelSignalLwCond(work area)
fn signal_lw_cond(call: &mut Call) {
    let work = call.u32();
    signal(call, Kind::Light, work, Signal::One);
}

/// sceKernelSignalLwCondAll(work area)
fn signal_lw_cond_all(call: &mut Call) {
    let work = call.u32();
    signal(call, Kind::Light, work, Signal::All);
}

/// sceKernelSignalLwCondTo(work area, thread)
fn signal_lw_cond_to(call: &mut Call) {
    let (work, thread) = (call.u32(), call.u32());
    signal(call, Kind::Light, work, Signal::To(thread));
}

// -- message pipes ---

/// SCE_KERNEL_MSG_PIPE_MODE_FULL, the whole message or nothing, rather than
/// as much as there is room or data for; _DONT_WAIT, failing rather than
/// waiting; and _DONT_REMOVE, a receive that leaves what it reads.
const PIPE_FULL: u32 = 0x1;
const PIPE_DONT_WAIT: u32 = 0x10;
const PIPE_DONT_REMOVE: u32 = 0x100;

/// sceKernelCreateMsgPipe(name, type, attributes, buffer size, option)
fn create_pipe(call: &mut Call) {
    let (name, _kind, attributes, size) = (call.string(), call.u32(), call.u32(), call.u32());
    let kernel = &call.shared.kernel;
    let uid = kernel.uid();
    if size == 0 {
        log::warn!("{}: a message pipe with no buffer gets one of a page", call.thread.name);
    }
    let capacity = if size == 0 { 0x1000 } else { size as usize };
    let pipe = Pipe { name: name.unwrap_or_default(), attributes, capacity, buffer: Default::default() };
    kernel.change(|objects| objects.pipes.insert(uid, pipe));
    call.ret(uid);
}

/// sceKernelDeleteMsgPipe(UID)
fn delete_pipe(call: &mut Call) {
    let uid = call.u32();
    let removed = call.shared.kernel.change(|objects| objects.pipes.remove(&uid));
    call.ret(if removed.is_some() { 0 } else { errors::UNKNOWN_MSG_PIPE_ID });
}

/// sends a message into a pipe, waiting for room unless told not to, and
/// gives how much went.
fn send(call: &mut Call, uid: u32, message: u32, size: u32, mode: u32, sent: u32, timeout: Option<Timeout>) {
    let data = call.memory().bytes(message, size as usize).unwrap_or_default();
    let attempt = |objects: &mut Objects| -> Option<Result<u32, u32>> {
        let Some(pipe) = objects.pipes.get_mut(&uid) else {
            return Some(Err(errors::UNKNOWN_MSG_PIPE_ID));
        };
        let room = pipe.capacity - pipe.buffer.len();
        if mode & PIPE_FULL != 0 && data.len() > pipe.capacity {
            return Some(Err(errors::ILLEGAL_SIZE));
        }
        let count = if mode & PIPE_FULL != 0 {
            if room < data.len() {
                return None;
            }
            data.len()
        } else {
            if room == 0 && !data.is_empty() {
                return None;
            }
            room.min(data.len())
        };
        pipe.buffer.extend(&data[..count]);
        Some(Ok(count as u32))
    };
    let kernel = &call.shared.kernel;
    let result = match &timeout {
        Some(timeout) if mode & PIPE_DONT_WAIT == 0 => kernel.wait_for(call.shared, timeout.limit(), attempt),
        _ => kernel.change(|objects| attempt(objects).unwrap_or(Err(errors::MSG_PIPE_FULL))),
    };
    if let (Ok(count), true) = (result, sent != 0) {
        call.memory().write32(sent, count);
    }
    if let Some(timeout) = timeout {
        timeout.finish(call);
    }
    call.result(result.map(|_| 0));
}

/// receives a message from a pipe, waiting for one unless told not to, and
/// gives how much came.
fn receive(call: &mut Call, uid: u32, message: u32, size: u32, mode: u32, received: u32, timeout: Option<Timeout>) {
    let size = size as usize;
    let attempt = |objects: &mut Objects| -> Option<Result<Vec<u8>, u32>> {
        let Some(pipe) = objects.pipes.get_mut(&uid) else {
            return Some(Err(errors::UNKNOWN_MSG_PIPE_ID));
        };
        let count = if mode & PIPE_FULL != 0 {
            if pipe.buffer.len() < size {
                return None;
            }
            size
        } else {
            if pipe.buffer.is_empty() && size > 0 {
                return None;
            }
            pipe.buffer.len().min(size)
        };
        let data: Vec<u8> = if mode & PIPE_DONT_REMOVE != 0 {
            pipe.buffer.iter().take(count).copied().collect()
        } else {
            pipe.buffer.drain(..count).collect()
        };
        Some(Ok(data))
    };
    let kernel = &call.shared.kernel;
    let result = match &timeout {
        Some(timeout) if mode & PIPE_DONT_WAIT == 0 => kernel.wait_for(call.shared, timeout.limit(), attempt),
        _ => kernel.change(|objects| attempt(objects).unwrap_or(Err(errors::MSG_PIPE_EMPTY))),
    };
    if let Ok(data) = &result {
        call.memory().write_bytes(message, data);
        if received != 0 {
            call.memory().write32(received, data.len() as u32);
        }
    }
    if let Some(timeout) = timeout {
        timeout.finish(call);
    }
    call.result(result.map(|_| 0));
}

/// sceKernelSendMsgPipe(UID, message, size, mode, &sent, timeout)
fn send_pipe(call: &mut Call) {
    let (uid, message, size, mode, sent) = (call.u32(), call.u32(), call.u32(), call.u32(), call.u32());
    let timeout = Timeout::read(call);
    send(call, uid, message, size, mode, sent, Some(timeout));
}

/// sceKernelTrySendMsgPipe(UID, message, size, mode, &sent)
fn try_send_pipe(call: &mut Call) {
    let (uid, message, size, mode, sent) = (call.u32(), call.u32(), call.u32(), call.u32(), call.u32());
    send(call, uid, message, size, mode | PIPE_DONT_WAIT, sent, None);
}

/// sceKernelReceiveMsgPipe(UID, message, size, mode, &received, timeout)
fn receive_pipe(call: &mut Call) {
    let (uid, message, size, mode, received) = (call.u32(), call.u32(), call.u32(), call.u32(), call.u32());
    let timeout = Timeout::read(call);
    receive(call, uid, message, size, mode, received, Some(timeout));
}

/// sceKernelTryReceiveMsgPipe(UID, message, size, mode, &received)
fn try_receive_pipe(call: &mut Call) {
    let (uid, message, size, mode, received) = (call.u32(), call.u32(), call.u32(), call.u32(), call.u32());
    receive(call, uid, message, size, mode | PIPE_DONT_WAIT, received, None);
}

// -- callbacks ---

/// sceKernelCreateCallback(name, attributes, function, argument) -> UID,
/// run by the thread that made it.
fn create_callback(call: &mut Call) {
    let (name, _attributes, function, argument) = (call.string(), call.u32(), call.u32(), call.u32());
    let kernel = &call.shared.kernel;
    let uid = kernel.uid();
    let callback = Callback {
        name: name.unwrap_or_default(),
        thread: call.thread.uid,
        function,
        argument,
        notified: 0,
        notify_argument: 0,
    };
    kernel.change(|objects| objects.callbacks.insert(uid, callback));
    call.ret(uid);
}

/// sceKernelDeleteCallback(UID)
fn delete_callback(call: &mut Call) {
    let uid = call.u32();
    let removed = call.shared.kernel.change(|objects| objects.callbacks.remove(&uid));
    call.ret(if removed.is_some() { 0 } else { errors::UNKNOWN_CALLBACK_ID });
}

/// sceKernelNotifyCallback(UID, argument)
fn notify_callback(call: &mut Call) {
    let (uid, argument) = (call.u32(), call.u32());
    let found = call.shared.kernel.change(|objects| {
        objects.callbacks.get_mut(&uid).map(|callback| {
            callback.notified += 1;
            callback.notify_argument = argument;
        })
    });
    call.ret(if found.is_some() { 0 } else { errors::UNKNOWN_CALLBACK_ID });
}

/// sceKernelCancelCallback(UID), forgetting it was told anything.
fn cancel_callback(call: &mut Call) {
    let uid = call.u32();
    let found =
        call.shared.kernel.change(|objects| objects.callbacks.get_mut(&uid).map(|callback| callback.notified = 0));
    call.ret(if found.is_some() { 0 } else { errors::UNKNOWN_CALLBACK_ID });
}

/// sceKernelGetCallbackCount(UID) -> how many times it was told
fn get_callback_count(call: &mut Call) {
    let uid = call.u32();
    let count = call.shared.kernel.change(|objects| objects.callbacks.get(&uid).map(|callback| callback.notified));
    call.ret(count.unwrap_or(errors::UNKNOWN_CALLBACK_ID));
}

/// sceKernelCheckCallback() -> how many ran: every callback of this thread
/// that was told something runs, as callback(UID, count, argument, its
/// own argument), and goes if it returns other than 0.
fn check_callback(call: &mut Call) {
    let me = call.thread.uid;
    let due: Vec<(u32, u32, u32, u32, u32)> = call.shared.kernel.change(|objects| {
        objects
            .callbacks
            .iter_mut()
            .filter(|(_, callback)| callback.thread == me && callback.notified > 0)
            .map(|(&uid, callback)| {
                let count = std::mem::take(&mut callback.notified);
                (uid, callback.function, count, callback.notify_argument, callback.argument)
            })
            .collect()
    });
    let mut ran = 0u32;
    for (uid, function, count, notify_argument, argument) in due {
        let Some(result) = call.call_guest(function, &[uid, count, notify_argument, argument]) else {
            return;
        };
        ran += 1;
        if result != 0 {
            call.shared.kernel.change(|objects| objects.callbacks.remove(&uid));
        }
    }
    call.ret(ran);
}
