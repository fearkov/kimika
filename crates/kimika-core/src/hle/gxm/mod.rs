//! SceGxm, the GPU's library: memory the GPU may use, the display queue
//! that shows finished frames through a callback of the game's, sync
//! objects and notifications, contexts and the scenes drawn with them,
//! color and depth surfaces, shader programs and textures.
//!
//! the GPU here draws each scene as the game gives it, so by the time a
//! scene ends its work is done: sync objects have nothing to wait for and
//! notifications are written at once.

mod context;
mod surface;

use std::collections::{HashMap, VecDeque};

use super::{Call, Function};
use crate::hle::threads::make_thread;
use crate::kernel::{errors, thread};

pub use context::Context;

pub const FUNCTIONS: &[&[(&str, Function)]] = &[BASE, context::FUNCTIONS, surface::FUNCTIONS];

const BASE: &[(&str, Function)] = &[
    ("sceGxmInitialize", initialize),
    ("sceGxmVshInitialize", initialize),
    ("sceGxmTerminate", terminate),
    ("sceGxmMapMemory", map_memory),
    ("sceGxmUnmapMemory", unmap_memory),
    ("sceGxmMapVertexUsseMemory", map_usse_memory),
    ("sceGxmUnmapVertexUsseMemory", unmap_memory),
    ("sceGxmMapFragmentUsseMemory", map_usse_memory),
    ("sceGxmUnmapFragmentUsseMemory", unmap_memory),
    ("sceGxmDisplayQueueAddEntry", display_queue_add_entry),
    ("sceGxmDisplayQueueFinish", display_queue_finish),
    ("sceGxmSyncObjectCreate", sync_object_create),
    ("sceGxmSyncObjectDestroy", sync_object_destroy),
    ("sceGxmGetNotificationRegion", get_notification_region),
    ("sceGxmNotificationWait", notification_wait),
    ("sceGxmPadHeartbeat", done),
    ("sceGxmPadTriggerGpuPaTrace", done),
    ("sceGxmPushUserMarker", done),
    ("sceGxmPopUserMarker", done),
    ("sceGxmSetUserMarker", done),
    ("sceGxmSetValidationEnable", done),
    ("sceAppMgrGetBudgetInfo", get_budget_info),
];

pub const UNINITIALIZED: u32 = 0x805B_0000;
pub const ALREADY_INITIALIZED: u32 = 0x805B_0001;
pub const OUT_OF_MEMORY: u32 = 0x805B_0002;
pub const INVALID_VALUE: u32 = 0x805B_0003;
pub const INVALID_POINTER: u32 = 0x805B_0004;
pub const NOT_WITHIN_SCENE: u32 = 0x805B_0006;
pub const WITHIN_SCENE: u32 = 0x805B_0007;
pub const NULL_PROGRAM: u32 = 0x805B_0008;
pub const RESERVE_FAILED: u32 = 0x805B_000B;

/// how many notification words the region has.
const NOTIFICATIONS: u32 = 512;

/// what SceGxm keeps while the game runs.
#[derive(Default)]
pub struct State {
    pub initialized: bool,
    /// the display queue's callback, the size of the data each entry
    /// passes it and how many entries may wait.
    pub display_callback: u32,
    pub display_data_size: u32,
    pub display_max_pending: u32,
    pub notification_region: u32,
    /// the memory the game said the GPU may use, by base, with sizes.
    pub mapped: HashMap<u32, u32>,
    pub contexts: HashMap<u32, Context>,
    pub render_targets: HashMap<u32, RenderTarget>,
}

pub struct RenderTarget {
    pub width: u32,
    pub height: u32,
    pub multisample: u32,
}

/// what the display queue holds: the callback data of each frame waiting
/// to be shown, and whether one is being shown.
#[derive(Default)]
pub struct DisplayQueue {
    pub entries: VecDeque<Vec<u8>>,
    pub showing: bool,
    pub closed: bool,
}

/// a function that does nothing and says it did it.
fn done(call: &mut Call) {
    call.ret(0u32);
}

/// sceGxmInitialize(params): the display queue's callback and how many
/// frames it holds, and its thread started.
fn initialize(call: &mut Call) {
    let params = call.u32();
    let memory = call.memory();
    if params == 0 {
        return call.ret(INVALID_POINTER);
    }
    let (callback, data_size, max_pending) =
        (memory.read32(params + 8), memory.read32(params + 12), memory.read32(params + 4));
    {
        let mut gxm = call.shared.hle.gxm.lock().unwrap();
        if gxm.initialized {
            return call.ret(ALREADY_INITIALIZED);
        }
        let Some(region) = call.shared.kernel.allocate(memory, NOTIFICATIONS * 4, 0x100) else {
            return call.ret(OUT_OF_MEMORY);
        };
        *gxm = State {
            initialized: true,
            display_callback: callback,
            display_data_size: data_size,
            display_max_pending: max_pending.max(1),
            notification_region: region,
            ..State::default()
        };
    }
    call.shared.kernel.change(|objects| objects.display_queue = DisplayQueue::default());
    let shared = call.shared;
    let thread = super::trampoline(shared, display_queue_thread)
        .ok_or(OUT_OF_MEMORY)
        .and_then(|entry| make_thread(shared, "SceGxmDisplayQueue", entry, 0x40, 0x4000, 0, 0, false));
    match thread.and_then(|thread| thread::start(shared, &thread, &[])) {
        Ok(()) => call.ret(0u32),
        Err(error) => call.ret(error),
    }
}

/// the display queue's thread: shows each frame the game queues by calling
/// its callback with the entry's data, one at a time, until the library
/// ends.
fn display_queue_thread(call: &mut Call) {
    let (callback, size) = {
        let gxm = call.shared.hle.gxm.lock().unwrap();
        (gxm.display_callback, gxm.display_data_size)
    };
    let shared = call.shared;
    let Some(buffer) = shared.kernel.allocate(&shared.memory, size.max(4), 8) else {
        return call.ret(OUT_OF_MEMORY);
    };
    loop {
        let entry = shared.kernel.wait_for(shared, None, |objects| {
            let queue = &mut objects.display_queue;
            if queue.closed {
                return Some(Err(0));
            }
            let entry = queue.entries.pop_front()?;
            queue.showing = true;
            Some(Ok(entry))
        });
        let Ok(data) = entry else { break };
        shared.memory.write_bytes(buffer, &data);
        if call.call_guest(callback, &[buffer]).is_none() {
            return;
        }
        shared.kernel.change(|objects| objects.display_queue.showing = false);
    }
    call.ret(0u32);
}

/// sceGxmTerminate()
fn terminate(call: &mut Call) {
    let mut gxm = call.shared.hle.gxm.lock().unwrap();
    if !gxm.initialized {
        return call.ret(UNINITIALIZED);
    }
    gxm.initialized = false;
    drop(gxm);
    call.shared.kernel.change(|objects| objects.display_queue.closed = true);
    call.ret(0u32);
}

/// sceGxmMapMemory(base, size, attributes)
fn map_memory(call: &mut Call) {
    let (base, size) = (call.u32(), call.u32());
    if base == 0 {
        return call.ret(INVALID_POINTER);
    }
    call.shared.hle.gxm.lock().unwrap().mapped.insert(base, size);
    call.ret(0u32);
}

/// sceGxmUnmapMemory(base) and the USSE kinds.
fn unmap_memory(call: &mut Call) {
    let base = call.u32();
    let removed = call.shared.hle.gxm.lock().unwrap().mapped.remove(&base);
    call.ret(if removed.is_some() { 0 } else { INVALID_VALUE });
}

/// sceGxmMapVertexUsseMemory(base, size, &offset) and the fragment kind:
/// the offset USSE code there is reached at, the address itself here.
fn map_usse_memory(call: &mut Call) {
    let (base, size, offset) = (call.u32(), call.u32(), call.u32());
    if base == 0 || offset == 0 {
        return call.ret(INVALID_POINTER);
    }
    call.shared.hle.gxm.lock().unwrap().mapped.insert(base, size);
    call.memory().write32(offset, base);
    call.ret(0u32);
}

/// sceGxmDisplayQueueAddEntry(old sync, new sync, callback data): the
/// frame queued once there is room for it.
fn display_queue_add_entry(call: &mut Call) {
    let (_old, _new, data) = (call.u32(), call.u32(), call.u32());
    let (size, max_pending) = {
        let gxm = call.shared.hle.gxm.lock().unwrap();
        if !gxm.initialized {
            return call.ret(UNINITIALIZED);
        }
        (gxm.display_data_size, gxm.display_max_pending)
    };
    let bytes = call.memory().bytes(data, size as usize).unwrap_or_else(|| vec![0; size as usize]);
    let mut entry = Some(bytes);
    let result = call.shared.kernel.wait_for(call.shared, None, |objects| {
        let queue = &mut objects.display_queue;
        let pending = queue.entries.len() as u32 + queue.showing as u32;
        (pending < max_pending).then(|| {
            queue.entries.extend(entry.take());
            Ok(0)
        })
    });
    call.result(result);
}

/// sceGxmDisplayQueueFinish(), once every frame queued has been shown.
fn display_queue_finish(call: &mut Call) {
    let result = call.shared.kernel.wait_for(call.shared, None, |objects| {
        let queue = &objects.display_queue;
        (queue.entries.is_empty() && !queue.showing).then_some(Ok(0))
    });
    call.result(result);
}

/// sceGxmSyncObjectCreate(&sync)
fn sync_object_create(call: &mut Call) {
    let out = call.u32();
    if out == 0 {
        return call.ret(INVALID_POINTER);
    }
    match call.shared.kernel.allocate(call.memory(), 0x10, 0x10) {
        Some(sync) => {
            call.memory().write32(out, sync);
            call.ret(0u32);
        }
        None => call.ret(OUT_OF_MEMORY),
    }
}

/// sceGxmSyncObjectDestroy(sync)
fn sync_object_destroy(call: &mut Call) {
    let sync = call.u32();
    call.shared.kernel.free(sync);
    call.ret(0u32);
}

/// sceGxmGetNotificationRegion() -> the words notifications are written
/// to
fn get_notification_region(call: &mut Call) {
    let region = call.shared.hle.gxm.lock().unwrap().notification_region;
    call.ret(region);
}

/// sceGxmNotificationWait(notification): already written, the work it
/// waits for being done when it is given.
fn notification_wait(call: &mut Call) {
    let notification = call.u32();
    if notification == 0 {
        return call.ret(INVALID_POINTER);
    }
    call.ret(0u32);
}

/// writes a SceGxmNotification's value to its address, if there is one.
pub fn notify(call: &Call, notification: u32) {
    if notification == 0 {
        return;
    }
    let memory = call.memory();
    let (address, value) = (memory.read32(notification), memory.read32(notification + 4));
    if address != 0 {
        memory.write32(address, value);
    }
}

/// sceAppMgrGetBudgetInfo(info): only the system's own applications have a
/// budget, so a game is refused, as it would be.
fn get_budget_info(call: &mut Call) {
    call.ret(errors::ERROR);
}
