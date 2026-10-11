//! SceCtrl, the buttons and sticks, as the frontend last handed them over.

use super::{Call, Function};

pub const FUNCTIONS: &[(&str, Function)] = &[
    ("sceCtrlSetSamplingMode", set_sampling_mode),
    ("sceCtrlSetSamplingModeExt", set_sampling_mode),
    ("sceCtrlGetSamplingMode", get_sampling_mode),
    ("sceCtrlPeekBufferPositive", peek_buffer_positive),
    ("sceCtrlPeekBufferPositiveExt2", peek_buffer_positive),
    ("sceCtrlReadBufferPositive", read_buffer_positive),
    ("sceCtrlReadBufferPositiveExt2", read_buffer_positive),
];

/// the controls at one moment: SceCtrlButtons bits, and the sticks from 0
/// to 255 with 128 at rest.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CtrlState {
    pub buttons: u32,
    pub lx: u8,
    pub ly: u8,
    pub rx: u8,
    pub ry: u8,
}

impl Default for CtrlState {
    fn default() -> Self {
        CtrlState { buttons: 0, lx: 128, ly: 128, rx: 128, ry: 128 }
    }
}

pub const SELECT: u32 = 0x0000_0001;
pub const START: u32 = 0x0000_0008;
pub const UP: u32 = 0x0000_0010;
pub const RIGHT: u32 = 0x0000_0020;
pub const DOWN: u32 = 0x0000_0040;
pub const LEFT: u32 = 0x0000_0080;
pub const L2: u32 = 0x0000_0100;
pub const R2: u32 = 0x0000_0200;
pub const L1: u32 = 0x0000_0400;
pub const R1: u32 = 0x0000_0800;
pub const TRIANGLE: u32 = 0x0000_1000;
pub const CIRCLE: u32 = 0x0000_2000;
pub const CROSS: u32 = 0x0000_4000;
pub const SQUARE: u32 = 0x0000_8000;

/// SCE_CTRL_MODE_DIGITAL, where the sticks are not sampled.
const DIGITAL: u32 = 0;

/// sceCtrlSetSamplingMode(mode) -> the mode before
fn set_sampling_mode(call: &mut Call) {
    let mode = call.u32();
    let before = std::mem::replace(&mut *call.shared.hle.sampling_mode.lock().unwrap(), mode);
    call.ret(before);
}

/// sceCtrlGetSamplingMode(&mode)
fn get_sampling_mode(call: &mut Call) {
    let pointer = call.u32();
    let mode = *call.shared.hle.sampling_mode.lock().unwrap();
    call.memory().write32(pointer, mode);
    call.ret(0u32);
}

/// writes count SceCtrlData of the present state: a timestamp in
/// microseconds, the buttons, the sticks, and the pressure of each pressure
/// sensitive button, all or nothing.
fn fill(call: &mut Call, data: u32, count: u32) -> u32 {
    let state = *call.shared.hle.ctrl.lock().unwrap();
    let digital = *call.shared.hle.sampling_mode.lock().unwrap() == DIGITAL;
    let sticks = if digital { [128; 4] } else { [state.lx, state.ly, state.rx, state.ry] };
    let pressed = |bit: u32| if state.buttons & bit != 0 { 255 } else { 0 };
    let pressures = [UP, RIGHT, DOWN, LEFT, L2, R2, L1, R1, TRIANGLE, CIRCLE, CROSS, SQUARE].map(pressed);
    let time = call.shared.started.elapsed().as_micros() as u64;
    let memory = call.memory();
    for i in 0..count.min(64) {
        let at = data + i * 0x20;
        memory.write32(at, time as u32);
        memory.write32(at + 4, (time >> 32) as u32);
        memory.write32(at + 8, state.buttons);
        memory.write_bytes(at + 0xC, &sticks);
        memory.write_bytes(at + 0x10, &pressures);
        memory.write32(at + 0x1C, 0);
    }
    count.min(64)
}

/// sceCtrlPeekBufferPositive(port, &data, count) -> count, at once.
fn peek_buffer_positive(call: &mut Call) {
    let (_port, data, count) = (call.u32(), call.u32(), call.u32());
    let filled = fill(call, data, count);
    call.ret(filled);
}

/// sceCtrlReadBufferPositive(port, &data, count) -> count, after the next
/// sample, which the console takes at each vertical blank.
fn read_buffer_positive(call: &mut Call) {
    let (_port, data, count) = (call.u32(), call.u32(), call.u32());
    let shared = call.shared;
    shared.kernel.cores.release();
    shared.display.wait_vblank(|| shared.is_stopping());
    shared.kernel.cores.acquire(shared);
    let filled = fill(call, data, count);
    call.ret(filled);
}
