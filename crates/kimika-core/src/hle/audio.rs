//! SceAudio: ports the game writes samples to. no sound comes out yet,
//! but each output takes as long as its samples would to play, since
//! games pace themselves by it.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use super::{Call, Function};

pub const FUNCTIONS: &[(&str, Function)] = &[
    ("sceAudioOutOpenPort", open_port),
    ("sceAudioOutReleasePort", release_port),
    ("sceAudioOutOutput", output),
    ("sceAudioOutSetVolume", set_volume),
    ("sceAudioOutSetConfig", set_config),
    ("sceAudioOutGetConfig", get_config),
    ("sceAudioOutGetRestSample", get_rest_sample),
    ("sceAudioOutGetAdopt", get_adopt),
    ("sceAudioOutSetAlcMode", accepted),
    ("sceAudioOutSetEffectType", accepted),
    ("sceAudioOutGetPortVolume_forUser", accepted),
    ("sceAudioInOpenPort", no_microphone),
    ("sceAudioInOpenPortForDiag", no_microphone),
];

const NOT_OPENED: u32 = 0x8026_0001;
const INVALID_PORT: u32 = 0x8026_0003;
const PORT_FULL: u32 = 0x8026_0005;
const INVALID_SIZE: u32 = 0x8026_0006;
const INVALID_SAMPLE_FREQ: u32 = 0x8026_0008;
const INVALID_PORT_TYPE: u32 = 0x8026_000A;
const INVALID_CONF_TYPE: u32 = 0x8026_000C;
/// SCE_AUDIO_IN_ERROR_NOT_OPENED, there being no microphone.
const NO_MICROPHONE: u32 = 0x8026_0101;

/// SCE_AUDIO_MIN_LEN and SCE_AUDIO_MAX_LEN.
const MIN_LEN: u32 = 64;
const MAX_LEN: u32 = 65472;

pub struct Port {
    pub kind: u32,
    pub length: u32,
    pub frequency: u32,
    pub stereo: bool,
    pub volume: [u32; 2],
    /// when the samples given so far will have played.
    pub due: Instant,
}

#[derive(Default)]
pub struct Ports {
    pub open: HashMap<u32, Port>,
}

/// sceAudioOutOpenPort(type, samples per output, frequency, mode) -> port
fn open_port(call: &mut Call) {
    let (kind, length, frequency, mode) = (call.u32(), call.u32(), call.u32(), call.u32());
    if kind > 2 {
        return call.ret(INVALID_PORT_TYPE);
    }
    if !(MIN_LEN..=MAX_LEN).contains(&length) || length % MIN_LEN != 0 {
        return call.ret(INVALID_SIZE);
    }
    if ![8000, 11025, 12000, 16000, 22050, 24000, 32000, 44100, 48000].contains(&frequency) {
        return call.ret(INVALID_SAMPLE_FREQ);
    }
    let mut ports = call.shared.hle.audio.lock().unwrap();
    // the main port is one of a kind, as is the voice port
    if kind != 1 && ports.open.values().any(|port| port.kind == kind) {
        return call.ret(PORT_FULL);
    }
    let id = (0..8).map(|index| kind << 4 | index).find(|id| !ports.open.contains_key(id)).unwrap_or(0);
    ports.open.insert(id, Port { kind, length, frequency, stereo: mode == 1, volume: [32768; 2], due: Instant::now() });
    // a port is never 0, which reads as no port
    call.ret(id + 1);
}

fn port_index(port: u32) -> u32 {
    port.wrapping_sub(1)
}

/// sceAudioOutReleasePort(port)
fn release_port(call: &mut Call) {
    let port = call.u32();
    let removed = call.shared.hle.audio.lock().unwrap().open.remove(&port_index(port));
    call.ret(if removed.is_some() { 0 } else { INVALID_PORT });
}

/// sceAudioOutOutput(port, samples): once the samples given before have
/// played, these start to; NULL only waits for those.
fn output(call: &mut Call) {
    let (port, _samples) = (call.u32(), call.u32());
    let wait = {
        let mut ports = call.shared.hle.audio.lock().unwrap();
        let Some(port) = ports.open.get_mut(&port_index(port)) else {
            return call.ret(NOT_OPENED);
        };
        let now = Instant::now();
        let wait = port.due.saturating_duration_since(now);
        let length = Duration::from_secs_f64(port.length as f64 / port.frequency as f64);
        port.due = port.due.max(now) + length;
        wait
    };
    if !wait.is_zero() {
        Arc::clone(call.thread).nap(call.shared, wait);
    }
    call.ret(0u32);
}

/// sceAudioOutSetVolume(port, channels, volumes)
fn set_volume(call: &mut Call) {
    let (port, channels, volumes) = (call.u32(), call.u32(), call.u32());
    let memory = call.memory();
    let mut ports = call.shared.hle.audio.lock().unwrap();
    let Some(port) = ports.open.get_mut(&port_index(port)) else {
        return call.ret(NOT_OPENED);
    };
    for channel in 0..2 {
        if channels & (1 << channel) != 0 {
            port.volume[channel] = memory.read32(volumes + channel as u32 * 4);
        }
    }
    call.ret(0u32);
}

/// sceAudioOutSetConfig(port, samples per output, frequency, mode), a
/// negative value keeping what was set.
fn set_config(call: &mut Call) {
    let (port, length, frequency, mode) = (call.u32(), call.i32(), call.i32(), call.i32());
    let mut ports = call.shared.hle.audio.lock().unwrap();
    let Some(port) = ports.open.get_mut(&port_index(port)) else {
        return call.ret(NOT_OPENED);
    };
    if length > 0 {
        port.length = length as u32;
    }
    if frequency > 0 {
        port.frequency = frequency as u32;
    }
    if mode >= 0 {
        port.stereo = mode == 1;
    }
    call.ret(0u32);
}

/// sceAudioOutGetConfig(port, which): the frequency (0), the samples per
/// output (1) or the mode (2).
fn get_config(call: &mut Call) {
    let (port, which) = (call.u32(), call.u32());
    let ports = call.shared.hle.audio.lock().unwrap();
    let Some(port) = ports.open.get(&port_index(port)) else {
        return call.ret(NOT_OPENED);
    };
    let value = match which {
        0 => port.frequency,
        1 => port.length,
        2 => port.stereo as u32,
        _ => INVALID_CONF_TYPE,
    };
    call.ret(value);
}

/// sceAudioOutGetRestSample(port) -> samples still to play
fn get_rest_sample(call: &mut Call) {
    let port = call.u32();
    let ports = call.shared.hle.audio.lock().unwrap();
    let Some(port) = ports.open.get(&port_index(port)) else {
        return call.ret(NOT_OPENED);
    };
    let left = port.due.saturating_duration_since(Instant::now()).as_secs_f64() * port.frequency as f64;
    call.ret(left as u32);
}

/// sceAudioOutGetAdopt(type): whether a port of the type can be opened
fn get_adopt(call: &mut Call) {
    let kind = call.u32();
    call.ret((kind <= 2) as u32);
}

fn accepted(call: &mut Call) {
    call.ret(0u32);
}

fn no_microphone(call: &mut Call) {
    call.ret(NO_MICROPHONE);
}
