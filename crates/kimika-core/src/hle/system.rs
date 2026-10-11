//! the small libraries every game asks something of on its way up: the
//! system's settings through SceAppUtil, the battery and the clocks
//! through ScePower, and the touch panels and the motion sensors, which
//! nobody is touching or moving.

use super::{Call, Function};

pub const FUNCTIONS: &[(&str, Function)] = &[
    ("sceAppUtilInit", app_util_init),
    ("sceAppUtilShutdown", accepted),
    ("sceAppUtilSystemParamGetInt", system_param_get_int),
    ("sceAppUtilSystemParamGetString", system_param_get_string),
    ("sceAppUtilAppParamGetInt", app_param_get_int),
    ("sceAppUtilPhotoMount", accepted),
    ("sceAppUtilPhotoUmount", accepted),
    ("sceAppUtilMusicMount", accepted),
    ("sceAppUtilMusicUmount", accepted),
    ("sceAppUtilAppEventParseLiveArea", accepted),
    ("sceAppUtilReceiveAppEvent", accepted),
    ("sceAppUtilSaveDataMount", accepted),
    ("sceAppUtilSaveDataUmount", accepted),
    ("scePowerGetBatteryLifePercent", battery_percent),
    ("scePowerIsPowerOnline", yes),
    ("scePowerIsBatteryCharging", no),
    ("scePowerIsLowBattery", no),
    ("scePowerIsSuspendRequired", no),
    ("scePowerGetBatteryLifeTime", battery_minutes),
    ("scePowerGetBatteryRemainCapacity", battery_capacity),
    ("scePowerGetBatteryFullCapacity", battery_capacity),
    ("scePowerRegisterCallback", accepted),
    ("scePowerUnregisterCallback", accepted),
    ("scePowerSetArmClockFrequency", accepted),
    ("scePowerSetBusClockFrequency", accepted),
    ("scePowerSetGpuClockFrequency", accepted),
    ("scePowerSetGpuXbarClockFrequency", accepted),
    ("scePowerGetArmClockFrequency", arm_clock),
    ("scePowerGetBusClockFrequency", bus_clock),
    ("scePowerGetGpuClockFrequency", gpu_clock),
    ("scePowerGetGpuXbarClockFrequency", gpu_xbar_clock),
    ("scePowerRequestDisplayOff", accepted),
    ("scePowerRequestDisplayOn", accepted),
    ("sceTouchSetSamplingState", accepted),
    ("sceTouchGetSamplingState", touch_get_sampling_state),
    ("sceTouchPeek", touch_read),
    ("sceTouchRead", touch_read),
    ("sceTouchGetPanelInfo", touch_get_panel_info),
    ("sceTouchEnableTouchForce", accepted),
    ("sceTouchDisableTouchForce", accepted),
    ("sceMotionStartSampling", accepted),
    ("sceMotionStopSampling", accepted),
    ("sceMotionGetState", motion_get_state),
    ("sceMotionGetSensorState", motion_get_sensor_state),
    ("sceMotionReset", accepted),
    ("sceMotionMagnetometerOn", accepted),
    ("sceMotionMagnetometerOff", accepted),
    ("sceMotionSetDeadband", accepted),
    ("sceMotionSetTiltCorrection", accepted),
    ("sceMotionSetAngleThreshold", accepted),
];

fn accepted(call: &mut Call) {
    call.ret(0u32);
}

fn yes(call: &mut Call) {
    call.ret(1u32);
}

fn no(call: &mut Call) {
    call.ret(0u32);
}

/// sceAppUtilInit(init params, boot params): an application started as
/// usual.
fn app_util_init(call: &mut Call) {
    let (_init, boot) = (call.u32(), call.u32());
    if boot != 0 {
        call.memory().fill(boot, 0, 0x28);
    }
    call.ret(0u32);
}

/// the settings the console reports: English, cross to confirm, the dates
/// written year first, a 24 hour clock, in UTC with no daylight savings.
fn system_param(id: u32) -> Option<u32> {
    match id {
        1 => Some(1),
        2 => Some(1),
        4 => Some(0),
        5 => Some(1),
        6 => Some(0),
        7 => Some(0),
        _ => None,
    }
}

/// SCE_APPUTIL_ERROR_PARAMETER
const APPUTIL_PARAMETER: u32 = 0x8010_0600;

/// sceAppUtilSystemParamGetInt(id, &value)
fn system_param_get_int(call: &mut Call) {
    let (id, out) = (call.u32(), call.u32());
    match system_param(id) {
        Some(value) if out != 0 => {
            call.memory().write32(out, value);
            call.ret(0u32);
        }
        _ => call.ret(APPUTIL_PARAMETER),
    }
}

/// sceAppUtilSystemParamGetString(id, buffer, size): the user's name.
fn system_param_get_string(call: &mut Call) {
    let (id, buffer, size) = (call.u32(), call.u32(), call.u32());
    if id != 3 || buffer == 0 || size == 0 {
        return call.ret(APPUTIL_PARAMETER);
    }
    let mut name = b"Kimika".to_vec();
    name.truncate(size as usize - 1);
    name.push(0);
    call.memory().write_bytes(buffer, &name);
    call.ret(0u32);
}

/// sceAppUtilAppParamGetInt(id, &value): the game's SKU, the full one.
fn app_param_get_int(call: &mut Call) {
    let (id, out) = (call.u32(), call.u32());
    if id != 0 || out == 0 {
        return call.ret(APPUTIL_PARAMETER);
    }
    call.memory().write32(out, 3);
    call.ret(0u32);
}

fn battery_percent(call: &mut Call) {
    call.ret(100u32);
}

fn battery_minutes(call: &mut Call) {
    call.ret(300u32);
}

fn battery_capacity(call: &mut Call) {
    call.ret(2210u32);
}

/// the clocks in MHz, as a game finds them by default.
fn arm_clock(call: &mut Call) {
    call.ret(333u32);
}

fn bus_clock(call: &mut Call) {
    call.ret(222u32);
}

fn gpu_clock(call: &mut Call) {
    call.ret(111u32);
}

fn gpu_xbar_clock(call: &mut Call) {
    call.ret(111u32);
}

/// sceTouchGetSamplingState(port, &state), sampling either way.
fn touch_get_sampling_state(call: &mut Call) {
    let (_port, out) = (call.u32(), call.u32());
    if out != 0 {
        call.memory().write32(out, 1);
    }
    call.ret(0u32);
}

/// how big a SceTouchData is.
const TOUCH_DATA_SIZE: u32 = 0x90;

/// sceTouchPeek(port, data, count) and sceTouchRead: buffers with no
/// touches in them.
fn touch_read(call: &mut Call) {
    let (_port, data, count) = (call.u32(), call.u32(), call.u32());
    if data == 0 {
        return call.ret(0x8070_0001u32);
    }
    let stamp = call.shared.started.elapsed().as_micros() as u64;
    let memory = call.memory();
    for index in 0..count.min(64) {
        let at = data + index * TOUCH_DATA_SIZE;
        memory.fill(at, 0, TOUCH_DATA_SIZE);
        memory.write32(at, stamp as u32);
        memory.write32(at + 4, (stamp >> 32) as u32);
    }
    call.ret(count.min(64));
}

/// sceTouchGetPanelInfo(port, &info): the front panel and the back one,
/// both mapped to the 1920x1088 the reports use.
fn touch_get_panel_info(call: &mut Call) {
    let (port, info) = (call.u32(), call.u32());
    if info == 0 {
        return call.ret(0x8070_0001u32);
    }
    let memory = call.memory();
    memory.fill(info, 0, 0x30);
    let (max_x, max_y) = if port == 0 { (1919u16, 1087u16) } else { (1919, 889) };
    let halves = [0, 0, max_x, max_y, 0, 0, 1919, 1087];
    for (index, half) in halves.into_iter().enumerate() {
        memory.write16(info + index as u32 * 2, half);
    }
    memory.write8(info + 16, 1);
    memory.write8(info + 17, 128);
    call.ret(0u32);
}

/// how big a SceMotionState is.
const MOTION_STATE_SIZE: u32 = 0xF8;

/// the identity matrix's floats, row by row.
const IDENTITY: [f32; 16] = [1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0];

/// sceMotionGetState(&state): a console lying still face up.
fn motion_get_state(call: &mut Call) {
    let state = call.u32();
    if state == 0 {
        return call.ret(0x8036_0201u32);
    }
    let stamp = call.shared.started.elapsed().as_micros() as u64;
    let memory = call.memory();
    memory.fill(state, 0, MOTION_STATE_SIZE);
    memory.write32(state, stamp as u32);
    let float = |offset: u32, value: f32| memory.write32(state + offset, value.to_bits());
    // gravity, down through the back of the console
    float(0x0C, -1.0);
    // the orientation quaternion, x y z w
    float(0x34, 1.0);
    for (index, value) in IDENTITY.iter().enumerate() {
        float(0x38 + index as u32 * 4, *value);
        float(0x78 + index as u32 * 4, *value);
    }
    float(0xC4, -1.0);
    memory.write32(state + 0xC8, stamp as u32);
    memory.write32(state + 0xCC, (stamp >> 32) as u32);
    call.ret(0u32);
}

/// sceMotionGetSensorState(states, count): the sensors' readings, still.
fn motion_get_sensor_state(call: &mut Call) {
    let (states, count) = (call.u32(), call.u32());
    if states == 0 {
        return call.ret(0x8036_0201u32);
    }
    let memory = call.memory();
    for index in 0..count.min(64) {
        let at = states + index * 0x40;
        memory.fill(at, 0, 0x40);
        memory.write32(at + 8, (-1.0f32).to_bits());
    }
    call.ret(0u32);
}
