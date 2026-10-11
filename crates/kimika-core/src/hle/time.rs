//! the clocks: the process's own, counted from its start, and the date,
//! from the host's.

use std::time::{SystemTime, UNIX_EPOCH};

use super::{Call, Function};

pub const FUNCTIONS: &[(&str, Function)] = &[
    ("sceKernelGetProcessTimeLow", get_process_time_low),
    ("sceKernelGetProcessTimeWide", get_process_time_wide),
    ("sceKernelGetProcessTime", get_process_time),
    ("sceKernelGetSystemTimeLow", get_process_time_low),
    ("sceKernelGetSystemTimeWide", get_process_time_wide),
    ("sceKernelLibcGettimeofday", gettimeofday),
    ("sceKernelLibcTime", time),
    ("sceRtcGetCurrentTick", get_current_tick),
    ("sceRtcGetTickResolution", get_tick_resolution),
];

/// microseconds from the year 1 to 1970, where the RTC's ticks start.
const RTC_EPOCH: u64 = 62_135_596_800_000_000;

fn microseconds(call: &Call) -> u64 {
    call.shared.started.elapsed().as_micros() as u64
}

fn since_1970() -> std::time::Duration {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default()
}

/// sceKernelGetProcessTimeLow() -> microseconds, the low word.
fn get_process_time_low(call: &mut Call) {
    call.ret(microseconds(call) as u32);
}

/// sceKernelGetProcessTimeWide() -> microseconds
fn get_process_time_wide(call: &mut Call) {
    call.ret(microseconds(call));
}

/// sceKernelGetProcessTime(&clock)
fn get_process_time(call: &mut Call) {
    let pointer = call.u32();
    let now = microseconds(call);
    call.memory().write32(pointer, now as u32);
    call.memory().write32(pointer + 4, (now >> 32) as u32);
    call.ret(0u32);
}

/// sceKernelLibcGettimeofday(&timeval, &timezone), seconds and
/// microseconds since 1970, in UTC.
fn gettimeofday(call: &mut Call) {
    let (timeval, timezone) = (call.u32(), call.u32());
    let now = since_1970();
    let memory = call.memory();
    if timeval != 0 {
        memory.write32(timeval, now.as_secs() as u32);
        memory.write32(timeval + 4, now.subsec_micros());
    }
    if timezone != 0 {
        memory.write32(timezone, 0);
        memory.write32(timezone + 4, 0);
    }
    call.ret(0u32);
}

/// sceKernelLibcTime(&time) -> seconds since 1970
fn time(call: &mut Call) {
    let pointer = call.u32();
    let seconds = since_1970().as_secs() as u32;
    if pointer != 0 {
        call.memory().write32(pointer, seconds);
    }
    call.ret(seconds);
}

/// sceRtcGetCurrentTick(&tick), microseconds since the year 1, in UTC.
fn get_current_tick(call: &mut Call) {
    let pointer = call.u32();
    let tick = RTC_EPOCH + since_1970().as_micros() as u64;
    call.memory().write32(pointer, tick as u32);
    call.memory().write32(pointer + 4, (tick >> 32) as u32);
    call.ret(0u32);
}

/// sceRtcGetTickResolution() -> ticks a second
fn get_tick_resolution(call: &mut Call) {
    call.ret(1_000_000u32);
}
