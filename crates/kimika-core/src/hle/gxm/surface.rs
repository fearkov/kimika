//! color and depth/stencil surfaces: what a scene draws into. both
//! structures are opaque to the game, so they hold here what this library
//! needs to know of them, laid out as it likes.
//!
//! a SceGxmColorSurface, 0x30 bytes: the format at 0x00, the memory layout
//! at 0x04, the scale, gamma and dither modes or'd together at 0x08, the
//! width, height and stride in pixels at 0x0C, 0x10 and 0x14, the data at
//! 0x18, the output register size where the SDK has it at 0x1C, the clip
//! rectangle's corners as halfwords at 0x20 and 0x24, and 1 at 0x28 when
//! enabled.
//!
//! a SceGxmDepthStencilSurface, 0x14 bytes, keeps its SDK fields: the
//! layout, force load and force store bits and 1 << 31 when enabled in the
//! control word, the depth and stencil data, the background depth, and the
//! background stencil in the low byte of the background control word, with
//! the stride in samples above it and the format's index on top.

use super::super::{Call, Function};
use crate::memory::Memory;

pub const FUNCTIONS: &[(&str, Function)] = &[
    ("sceGxmColorSurfaceInit", color_init),
    ("sceGxmColorSurfaceInitDisabled", color_init_disabled),
    ("sceGxmColorSurfaceIsEnabled", color_is_enabled),
    ("sceGxmColorSurfaceGetClip", color_get_clip),
    ("sceGxmColorSurfaceSetClip", color_set_clip),
    ("sceGxmColorSurfaceGetScaleMode", color_get_scale_mode),
    ("sceGxmColorSurfaceSetScaleMode", color_set_scale_mode),
    ("sceGxmColorSurfaceGetData", color_get_data),
    ("sceGxmColorSurfaceSetData", color_set_data),
    ("sceGxmColorSurfaceGetFormat", color_get_format),
    ("sceGxmColorSurfaceSetFormat", color_set_format),
    ("sceGxmColorSurfaceGetType", color_get_type),
    ("sceGxmColorSurfaceGetStrideInPixels", color_get_stride),
    ("sceGxmColorSurfaceGetGammaMode", color_get_gamma_mode),
    ("sceGxmColorSurfaceSetGammaMode", color_set_gamma_mode),
    ("sceGxmColorSurfaceGetDitherMode", color_get_dither_mode),
    ("sceGxmColorSurfaceSetDitherMode", color_set_dither_mode),
    ("sceGxmDepthStencilSurfaceInit", depth_init),
    ("sceGxmDepthStencilSurfaceInitDisabled", depth_init_disabled),
    ("sceGxmDepthStencilSurfaceIsEnabled", depth_is_enabled),
    ("sceGxmDepthStencilSurfaceGetBackgroundDepth", depth_get_background_depth),
    ("sceGxmDepthStencilSurfaceSetBackgroundDepth", depth_set_background_depth),
    ("sceGxmDepthStencilSurfaceGetBackgroundStencil", depth_get_background_stencil),
    ("sceGxmDepthStencilSurfaceSetBackgroundStencil", depth_set_background_stencil),
    ("sceGxmDepthStencilSurfaceGetForceLoadMode", depth_get_force_load),
    ("sceGxmDepthStencilSurfaceSetForceLoadMode", depth_set_force_load),
    ("sceGxmDepthStencilSurfaceGetForceStoreMode", depth_get_force_store),
    ("sceGxmDepthStencilSurfaceSetForceStoreMode", depth_set_force_store),
    ("sceGxmDepthStencilSurfaceGetFormat", depth_get_format),
    ("sceGxmDepthStencilSurfaceGetStrideInSamples", depth_get_stride),
];

const SCALE_MODES: u32 = 0x1;
const DITHER_MODES: u32 = 0x8;
const GAMMA_MODES: u32 = 0x3000;

/// a color surface as a scene sees it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ColorSurface {
    pub format: u32,
    pub layout: u32,
    pub modes: u32,
    pub width: u32,
    pub height: u32,
    pub stride: u32,
    pub data: u32,
    pub output_register_size: u32,
    /// x min, y min, x max and y max, inclusive.
    pub clip: [u32; 4],
}

impl ColorSurface {
    /// the surface a structure describes, none when disabled.
    pub fn read(memory: &Memory, address: u32) -> Option<ColorSurface> {
        if address == 0 || memory.read32(address + 0x28) == 0 {
            return None;
        }
        let word = |offset: u32| memory.read32(address + offset);
        let (low, high) = (word(0x20), word(0x24));
        Some(ColorSurface {
            format: word(0x00),
            layout: word(0x04),
            modes: word(0x08),
            width: word(0x0C),
            height: word(0x10),
            stride: word(0x14),
            data: word(0x18),
            output_register_size: word(0x1C),
            clip: [low & 0xFFFF, low >> 16, high & 0xFFFF, high >> 16],
        })
    }

    pub fn write(&self, memory: &Memory, address: u32) {
        let words = [
            self.format,
            self.layout,
            self.modes,
            self.width,
            self.height,
            self.stride,
            self.data,
            self.output_register_size,
            self.clip[0] | self.clip[1] << 16,
            self.clip[2] | self.clip[3] << 16,
            1,
            0,
        ];
        for (index, word) in words.into_iter().enumerate() {
            memory.write32(address + index as u32 * 4, word);
        }
    }
}

/// the depth/stencil formats, in the order their index is kept.
const DEPTH_FORMATS: [u32; 7] =
    [0x0004_4000, 0x0002_2000, 0x0006_6000, 0x000C_C000, 0x000E_E000, 0x0126_6000, 0x0244_4000];
const ENABLED: u32 = 1 << 31;
const FORCE_LOAD: u32 = 0x2;
const FORCE_STORE: u32 = 0x4;
const DEPTH_LAYOUTS: u32 = 0x0001_1000;

/// a depth/stencil surface as a scene sees it.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct DepthStencilSurface {
    pub format: u32,
    pub layout: u32,
    pub stride: u32,
    pub depth: u32,
    pub stencil: u32,
    pub background_depth: f32,
    pub background_stencil: u8,
    pub force_load: bool,
    pub force_store: bool,
}

impl DepthStencilSurface {
    /// the surface a structure describes, none when disabled.
    pub fn read(memory: &Memory, address: u32) -> Option<DepthStencilSurface> {
        if address == 0 {
            return None;
        }
        let control = memory.read32(address);
        if control & ENABLED == 0 {
            return None;
        }
        let background = memory.read32(address + 0x10);
        Some(DepthStencilSurface {
            format: DEPTH_FORMATS.get((background >> 24) as usize).copied().unwrap_or(DEPTH_FORMATS[0]),
            layout: control & DEPTH_LAYOUTS,
            stride: (background >> 8) & 0xFFFF,
            depth: memory.read32(address + 4),
            stencil: memory.read32(address + 8),
            background_depth: f32::from_bits(memory.read32(address + 12)),
            background_stencil: background as u8,
            force_load: control & FORCE_LOAD != 0,
            force_store: control & FORCE_STORE != 0,
        })
    }
}

fn color_field(call: &mut Call, offset: u32) {
    let surface = call.u32();
    let value = call.memory().read32(surface + offset);
    call.ret(value);
}

fn set_color_field(call: &mut Call, offset: u32, mask: u32) {
    let (surface, value) = (call.u32(), call.u32());
    let memory = call.memory();
    let word = memory.read32(surface + offset);
    memory.write32(surface + offset, (word & !mask) | (value & mask));
    call.ret(0u32);
}

/// sceGxmColorSurfaceInit(surface, format, layout, scale mode, output
/// register size, width, height, stride in pixels, data)
fn color_init(call: &mut Call) {
    let (address, format, layout, scale) = (call.u32(), call.u32(), call.u32(), call.u32());
    let (output_register_size, width, height, stride, data) =
        (call.u32(), call.u32(), call.u32(), call.u32(), call.u32());
    if address == 0 || data == 0 {
        return call.ret(super::INVALID_POINTER);
    }
    if width == 0 || height == 0 || width > 4096 || height > 4096 || stride < width {
        return call.ret(super::INVALID_VALUE);
    }
    let surface = ColorSurface {
        format,
        layout,
        modes: scale & SCALE_MODES,
        width,
        height,
        stride,
        data,
        output_register_size,
        clip: [0, 0, width - 1, height - 1],
    };
    surface.write(call.memory(), address);
    call.ret(0u32);
}

/// sceGxmColorSurfaceInitDisabled(surface)
fn color_init_disabled(call: &mut Call) {
    let address = call.u32();
    call.memory().fill(address, 0, 0x30);
    call.ret(0u32);
}

/// sceGxmColorSurfaceIsEnabled(surface)
fn color_is_enabled(call: &mut Call) {
    let address = call.u32();
    let enabled = ColorSurface::read(call.memory(), address).is_some();
    call.ret(enabled as u32);
}

/// sceGxmColorSurfaceGetClip(surface, &x min, &y min, &x max, &y max)
fn color_get_clip(call: &mut Call) {
    let (address, x_min, y_min, x_max, y_max) = (call.u32(), call.u32(), call.u32(), call.u32(), call.u32());
    let memory = call.memory();
    let (low, high) = (memory.read32(address + 0x20), memory.read32(address + 0x24));
    for (out, value) in [(x_min, low & 0xFFFF), (y_min, low >> 16), (x_max, high & 0xFFFF), (y_max, high >> 16)] {
        if out != 0 {
            memory.write32(out, value);
        }
    }
}

/// sceGxmColorSurfaceSetClip(surface, x min, y min, x max, y max)
fn color_set_clip(call: &mut Call) {
    let (address, x_min, y_min, x_max, y_max) = (call.u32(), call.u32(), call.u32(), call.u32(), call.u32());
    let memory = call.memory();
    memory.write32(address + 0x20, (x_min & 0xFFFF) | y_min << 16);
    memory.write32(address + 0x24, (x_max & 0xFFFF) | y_max << 16);
}

fn color_get_scale_mode(call: &mut Call) {
    let surface = call.u32();
    let modes = call.memory().read32(surface + 0x08);
    call.ret(modes & SCALE_MODES);
}

fn color_set_scale_mode(call: &mut Call) {
    set_color_field(call, 0x08, SCALE_MODES);
}

fn color_get_data(call: &mut Call) {
    color_field(call, 0x18);
}

fn color_set_data(call: &mut Call) {
    set_color_field(call, 0x18, u32::MAX);
}

fn color_get_format(call: &mut Call) {
    color_field(call, 0x00);
}

fn color_set_format(call: &mut Call) {
    set_color_field(call, 0x00, u32::MAX);
}

fn color_get_type(call: &mut Call) {
    color_field(call, 0x04);
}

fn color_get_stride(call: &mut Call) {
    color_field(call, 0x14);
}

fn color_get_gamma_mode(call: &mut Call) {
    let surface = call.u32();
    let modes = call.memory().read32(surface + 0x08);
    call.ret(modes & GAMMA_MODES);
}

fn color_set_gamma_mode(call: &mut Call) {
    set_color_field(call, 0x08, GAMMA_MODES);
}

fn color_get_dither_mode(call: &mut Call) {
    let surface = call.u32();
    let modes = call.memory().read32(surface + 0x08);
    call.ret(modes & DITHER_MODES);
}

fn color_set_dither_mode(call: &mut Call) {
    set_color_field(call, 0x08, DITHER_MODES);
}

/// sceGxmDepthStencilSurfaceInit(surface, format, layout, stride in
/// samples, depth data, stencil data)
fn depth_init(call: &mut Call) {
    let (address, format, layout, stride, depth, stencil) =
        (call.u32(), call.u32(), call.u32(), call.u32(), call.u32(), call.u32());
    if address == 0 {
        return call.ret(super::INVALID_POINTER);
    }
    let Some(index) = DEPTH_FORMATS.iter().position(|&known| known == format) else {
        return call.ret(super::INVALID_VALUE);
    };
    let memory = call.memory();
    memory.write32(address, ENABLED | (layout & DEPTH_LAYOUTS));
    memory.write32(address + 4, depth);
    memory.write32(address + 8, stencil);
    memory.write32(address + 12, 1.0f32.to_bits());
    memory.write32(address + 16, (index as u32) << 24 | (stride & 0xFFFF) << 8);
    call.ret(0u32);
}

/// sceGxmDepthStencilSurfaceInitDisabled(surface)
fn depth_init_disabled(call: &mut Call) {
    let address = call.u32();
    call.memory().fill(address, 0, 0x14);
    call.memory().write32(address + 12, 1.0f32.to_bits());
    call.ret(0u32);
}

fn depth_is_enabled(call: &mut Call) {
    let address = call.u32();
    let control = call.memory().read32(address);
    call.ret((control & ENABLED != 0) as u32);
}

fn depth_get_background_depth(call: &mut Call) {
    let address = call.u32();
    let depth = f32::from_bits(call.memory().read32(address + 12));
    call.ret(depth);
}

fn depth_set_background_depth(call: &mut Call) {
    let address = call.u32();
    let depth = call.f32();
    call.memory().write32(address + 12, depth.to_bits());
}

fn depth_get_background_stencil(call: &mut Call) {
    let address = call.u32();
    let background = call.memory().read32(address + 16);
    call.ret(background & 0xFF);
}

fn depth_set_background_stencil(call: &mut Call) {
    let (address, stencil) = (call.u32(), call.u32());
    let memory = call.memory();
    let background = memory.read32(address + 16);
    memory.write32(address + 16, (background & !0xFF) | (stencil & 0xFF));
}

fn depth_control_bit(call: &mut Call, bit: u32) {
    let address = call.u32();
    let control = call.memory().read32(address);
    call.ret(control & bit);
}

fn set_depth_control_bit(call: &mut Call, bit: u32) {
    let (address, value) = (call.u32(), call.u32());
    let memory = call.memory();
    let control = memory.read32(address);
    memory.write32(address, (control & !bit) | (value & bit));
}

fn depth_get_force_load(call: &mut Call) {
    depth_control_bit(call, FORCE_LOAD);
}

fn depth_set_force_load(call: &mut Call) {
    set_depth_control_bit(call, FORCE_LOAD);
}

fn depth_get_force_store(call: &mut Call) {
    depth_control_bit(call, FORCE_STORE);
}

fn depth_set_force_store(call: &mut Call) {
    set_depth_control_bit(call, FORCE_STORE);
}

fn depth_get_format(call: &mut Call) {
    let address = call.u32();
    let background = call.memory().read32(address + 16);
    call.ret(DEPTH_FORMATS.get((background >> 24) as usize).copied().unwrap_or(DEPTH_FORMATS[0]));
}

fn depth_get_stride(call: &mut Call) {
    let address = call.u32();
    let background = call.memory().read32(address + 16);
    call.ret((background >> 8) & 0xFFFF);
}
