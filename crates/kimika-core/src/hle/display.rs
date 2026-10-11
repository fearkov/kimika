//! SceDisplay: the framebuffer a game shows and the vertical blanks it
//! paces itself by.

use super::{Call, Function};
use crate::display::{FrameBuffer, A2B10G10R10, A8B8G8R8};
use crate::kernel::errors;

pub const FUNCTIONS: &[(&str, Function)] = &[
    ("sceDisplaySetFrameBuf", set_frame_buf),
    ("sceDisplayGetFrameBuf", get_frame_buf),
    ("sceDisplayWaitVblankStart", wait_vblank_start),
    ("sceDisplayWaitVblankStartCB", wait_vblank_start),
    ("sceDisplayWaitVblankStartMulti", wait_vblank_start_multi),
    ("sceDisplayWaitVblankStartMultiCB", wait_vblank_start_multi),
    ("sceDisplayWaitSetFrameBuf", wait_set_frame_buf),
    ("sceDisplayWaitSetFrameBufCB", wait_set_frame_buf),
    ("sceDisplayGetRefreshRate", get_refresh_rate),
    ("sceDisplayGetVcount", get_vcount),
];

/// SCE_DISPLAY_SETBUF_NEXTFRAME, the framebuffer shown from the next blank.
const NEXT_FRAME: u32 = 1;
/// the resolutions the display has.
const RESOLUTIONS: [(u32, u32); 4] = [(960, 544), (720, 408), (640, 368), (480, 272)];

/// sceDisplaySetFrameBuf(&frame, sync), NULL to show nothing.
fn set_frame_buf(call: &mut Call) {
    let (pointer, sync) = (call.u32(), call.u32());
    if sync > NEXT_FRAME {
        return call.ret(errors::DISPLAY_INVALID_UPDATETIMING);
    }
    if pointer == 0 {
        call.shared.display.set(None, sync == NEXT_FRAME);
        return call.ret(0u32);
    }
    let memory = call.memory();
    let word = |offset: u32| memory.read32(pointer + offset);
    // the extended structure is 0x1C bytes and starts the same way
    if !matches!(word(0), 0x18 | 0x1C) {
        return call.ret(errors::DISPLAY_INVALID_VALUE);
    }
    let frame = FrameBuffer { base: word(4), pitch: word(8), format: word(0xC), width: word(0x10), height: word(0x14) };
    let error = if frame.base == 0 {
        Some(errors::DISPLAY_INVALID_ADDR)
    } else if !matches!(frame.format, A8B8G8R8 | A2B10G10R10) {
        Some(errors::DISPLAY_INVALID_PIXELFORMAT)
    } else if frame.pitch < frame.width {
        Some(errors::DISPLAY_INVALID_PITCH)
    } else if !RESOLUTIONS.contains(&(frame.width, frame.height)) {
        Some(errors::DISPLAY_INVALID_RESOLUTION)
    } else {
        None
    };
    if let Some(error) = error {
        log::warn!("{}: framebuffer {frame:X?} refused, 0x{error:08X}", call.thread.name);
        return call.ret(error);
    }
    if call.shared.display.latest() != Some(frame) {
        log::debug!("{}: framebuffer {frame:X?}", call.thread.name);
    }
    call.shared.display.set(Some(frame), sync == NEXT_FRAME);
    call.ret(0u32);
}

/// sceDisplayGetFrameBuf(&frame, sync), the one last set.
fn get_frame_buf(call: &mut Call) {
    let pointer = call.u32();
    let memory = call.memory();
    let frame = call.shared.display.latest();
    let fields = match frame {
        Some(frame) => [frame.base, frame.pitch, frame.format, frame.width, frame.height],
        None => [0; 5],
    };
    for (i, value) in fields.into_iter().enumerate() {
        memory.write32(pointer + 4 + 4 * i as u32, value);
    }
    call.ret(0u32);
}

/// waits for blanks with the core given up.
fn wait_blanks(call: &mut Call, count: u32) {
    let shared = call.shared;
    shared.kernel.cores.release();
    for _ in 0..count.max(1) {
        shared.display.wait_vblank(|| shared.is_stopping());
    }
    shared.kernel.cores.acquire(shared);
}

/// sceDisplayWaitVblankStart(), until the next blank starts.
fn wait_vblank_start(call: &mut Call) {
    wait_blanks(call, 1);
    call.ret(0u32);
}

/// sceDisplayWaitVblankStartMulti(count)
fn wait_vblank_start_multi(call: &mut Call) {
    let count = call.u32();
    wait_blanks(call, count);
    call.ret(0u32);
}

/// sceDisplayWaitSetFrameBuf(), until a framebuffer set for the next frame
/// shows.
fn wait_set_frame_buf(call: &mut Call) {
    let shared = call.shared;
    shared.kernel.cores.release();
    shared.display.wait_set(|| shared.is_stopping());
    shared.kernel.cores.acquire(shared);
    call.ret(0u32);
}

/// sceDisplayGetRefreshRate(&rate), 59.94 Hz.
fn get_refresh_rate(call: &mut Call) {
    let pointer = call.u32();
    call.memory().write32(pointer, (60000.0f32 / 1001.0).to_bits());
    call.ret(0u32);
}

/// sceDisplayGetVcount() -> blanks so far.
fn get_vcount(call: &mut Call) {
    call.ret(call.shared.display.vblanks() as u32);
}
