//! contexts and what they draw with: the render targets and surfaces of
//! the scene under way, the programs, streams, textures and uniform
//! buffers set, and the fixed-function state, front and back.

use kimika_gxm::state::{Compare, Cull, PolygonMode, RegionClip, RenderState, StencilOp};

use super::super::{Call, Function};
use super::surface::{ColorSurface, DepthStencilSurface};
use super::{notify, RenderTarget, INVALID_POINTER, INVALID_VALUE, NOT_WITHIN_SCENE, OUT_OF_MEMORY, WITHIN_SCENE};
use crate::memory::Memory;

pub const FUNCTIONS: &[(&str, Function)] = &[
    ("sceGxmCreateContext", create_context),
    ("sceGxmDestroyContext", destroy_context),
    ("sceGxmGetRenderTargetMemSize", get_render_target_mem_size),
    ("sceGxmCreateRenderTarget", create_render_target),
    ("sceGxmDestroyRenderTarget", destroy_render_target),
    ("sceGxmRenderTargetGetDriverMemBlock", render_target_get_driver_mem_block),
    ("sceGxmBeginScene", begin_scene),
    ("sceGxmEndScene", end_scene),
    ("sceGxmMidSceneFlush", mid_scene_flush),
    ("sceGxmFinish", finish),
    ("sceGxmSetVertexStream", set_vertex_stream),
    ("sceGxmSetVertexTexture", set_vertex_texture),
    ("sceGxmSetFragmentTexture", set_fragment_texture),
    ("sceGxmSetVertexUniformBuffer", set_vertex_uniform_buffer),
    ("sceGxmSetFragmentUniformBuffer", set_fragment_uniform_buffer),
    ("sceGxmSetVertexDefaultUniformBuffer", set_vertex_default_uniform_buffer),
    ("sceGxmSetFragmentDefaultUniformBuffer", set_fragment_default_uniform_buffer),
    ("sceGxmSetFrontDepthFunc", set_front_depth_func),
    ("sceGxmSetBackDepthFunc", set_back_depth_func),
    ("sceGxmSetFrontDepthWriteEnable", set_front_depth_write),
    ("sceGxmSetBackDepthWriteEnable", set_back_depth_write),
    ("sceGxmSetFrontFragmentProgramEnable", set_front_fragment_program_enable),
    ("sceGxmSetBackFragmentProgramEnable", set_back_fragment_program_enable),
    ("sceGxmSetFrontStencilRef", set_front_stencil_ref),
    ("sceGxmSetBackStencilRef", set_back_stencil_ref),
    ("sceGxmSetFrontStencilFunc", set_front_stencil_func),
    ("sceGxmSetBackStencilFunc", set_back_stencil_func),
    ("sceGxmSetFrontPointLineWidth", set_front_point_line_width),
    ("sceGxmSetBackPointLineWidth", set_back_point_line_width),
    ("sceGxmSetFrontPolygonMode", set_front_polygon_mode),
    ("sceGxmSetBackPolygonMode", set_back_polygon_mode),
    ("sceGxmSetFrontDepthBias", set_front_depth_bias),
    ("sceGxmSetBackDepthBias", set_back_depth_bias),
    ("sceGxmSetTwoSidedEnable", set_two_sided_enable),
    ("sceGxmSetCullMode", set_cull_mode),
    ("sceGxmSetViewport", set_viewport),
    ("sceGxmSetViewportEnable", set_viewport_enable),
    ("sceGxmSetRegionClip", set_region_clip),
    ("sceGxmSetDefaultRegionClipAndViewport", set_default_region_clip_and_viewport),
    ("sceGxmSetFrontLineFillLastPixelEnable", ignored),
    ("sceGxmSetBackLineFillLastPixelEnable", ignored),
    ("sceGxmSetFrontVisibilityTestIndex", ignored),
    ("sceGxmSetBackVisibilityTestIndex", ignored),
    ("sceGxmSetFrontVisibilityTestOp", ignored),
    ("sceGxmSetBackVisibilityTestOp", ignored),
    ("sceGxmSetFrontVisibilityTestEnable", ignored),
    ("sceGxmSetBackVisibilityTestEnable", ignored),
    ("sceGxmSetVisibilityBuffer", ignored),
    ("sceGxmSetWClampValue", ignored),
    ("sceGxmSetWClampEnable", ignored),
    ("sceGxmSetWBufferEnable", ignored),
];

/// how many vertex streams, textures and uniform buffers a context holds;
/// uniform buffer 14 is the default one.
pub const STREAMS: usize = 16;
pub const TEXTURES: usize = 16;
pub const UNIFORM_BUFFERS: usize = 15;
pub const DEFAULT_UNIFORM_BUFFER: usize = 14;

/// a ring of memory the game gave a context, which default uniform
/// buffers are reserved from.
#[derive(Debug, Clone, Copy, Default)]
pub struct Ring {
    pub base: u32,
    pub size: u32,
    pub used: u32,
}

impl Ring {
    /// size bytes from the ring, back at its start when they do not fit
    /// at its end.
    pub fn reserve(&mut self, size: u32) -> Option<u32> {
        let size = size.next_multiple_of(16);
        if size > self.size {
            return None;
        }
        if self.used + size > self.size {
            self.used = 0;
        }
        let address = self.base + self.used;
        self.used += size;
        Some(address)
    }
}

/// the scene a context is drawing.
#[derive(Debug, Clone)]
pub struct Scene {
    pub target: u32,
    pub width: u32,
    pub height: u32,
    pub color: Option<ColorSurface>,
    pub depth_stencil: Option<DepthStencilSurface>,
}

/// what a context holds.
#[derive(Debug, Clone, Default)]
pub struct Context {
    pub vertex_ring: Ring,
    pub fragment_ring: Ring,
    pub vertex_program: u32,
    pub fragment_program: u32,
    pub vertex_streams: [u32; STREAMS],
    /// the 16 bytes of each SceGxmTexture set, copied when it is.
    pub vertex_textures: [[u32; 4]; TEXTURES],
    pub fragment_textures: [[u32; 4]; TEXTURES],
    pub vertex_uniform_buffers: [u32; UNIFORM_BUFFERS],
    pub fragment_uniform_buffers: [u32; UNIFORM_BUFFERS],
    pub render: RenderState,
    pub scene: Option<Scene>,
}

/// changes the context the call's first argument names, giving what the
/// change gives, none if there is no such context.
fn with_context<T>(call: &mut Call, change: impl FnOnce(&mut Context, &mut Call) -> T) -> Option<T> {
    let address = call.u32();
    let shared = call.shared;
    let mut gxm = shared.hle.gxm.lock().unwrap();
    let context = gxm.contexts.get_mut(&address)?;
    Some(change(context, call))
}

/// sceGxmCreateContext(params, &context): the context is the host memory
/// the game gave it.
fn create_context(call: &mut Call) {
    let (params, out) = (call.u32(), call.u32());
    if params == 0 || out == 0 {
        return call.ret(INVALID_POINTER);
    }
    let memory = call.memory();
    let word = |offset: u32| memory.read32(params + offset);
    let host = word(0x00);
    if host == 0 {
        return call.ret(INVALID_POINTER);
    }
    let context = Context {
        vertex_ring: Ring { base: word(0x10), size: word(0x14), used: 0 },
        fragment_ring: Ring { base: word(0x18), size: word(0x1C), used: 0 },
        ..Context::default()
    };
    call.shared.hle.gxm.lock().unwrap().contexts.insert(host, context);
    memory.write32(out, host);
    call.ret(0u32);
}

/// sceGxmDestroyContext(context)
fn destroy_context(call: &mut Call) {
    let context = call.u32();
    let removed = call.shared.hle.gxm.lock().unwrap().contexts.remove(&context);
    call.ret(if removed.is_some() { 0 } else { INVALID_POINTER });
}

/// sceGxmGetRenderTargetMemSize(params, &size): what the hardware would
/// keep for a render target, which nothing here needs.
fn get_render_target_mem_size(call: &mut Call) {
    let (_params, out) = (call.u32(), call.u32());
    if out == 0 {
        return call.ret(INVALID_POINTER);
    }
    call.memory().write32(out, 0x1000);
    call.ret(0u32);
}

/// sceGxmCreateRenderTarget(params, &target)
fn create_render_target(call: &mut Call) {
    let (params, out) = (call.u32(), call.u32());
    if params == 0 || out == 0 {
        return call.ret(INVALID_POINTER);
    }
    let memory = call.memory();
    let target = RenderTarget {
        width: memory.read16(params + 4) as u32,
        height: memory.read16(params + 6) as u32,
        multisample: memory.read16(params + 10) as u32,
    };
    let Some(address) = call.shared.kernel.allocate(memory, 0x10, 0x10) else {
        return call.ret(OUT_OF_MEMORY);
    };
    memory.write32(address, memory.read32(params + 0x10));
    call.shared.hle.gxm.lock().unwrap().render_targets.insert(address, target);
    memory.write32(out, address);
    call.ret(0u32);
}

/// sceGxmDestroyRenderTarget(target)
fn destroy_render_target(call: &mut Call) {
    let target = call.u32();
    let removed = call.shared.hle.gxm.lock().unwrap().render_targets.remove(&target);
    if removed.is_some() {
        call.shared.kernel.free(target);
    }
    call.ret(if removed.is_some() { 0 } else { INVALID_POINTER });
}

/// sceGxmRenderTargetGetDriverMemBlock(target, &block)
fn render_target_get_driver_mem_block(call: &mut Call) {
    let (target, out) = (call.u32(), call.u32());
    let memory = call.memory();
    memory.write32(out, memory.read32(target));
    call.ret(0u32);
}

/// sceGxmBeginScene(context, flags, render target, valid region, vertex
/// sync, fragment sync, color surface, depth/stencil surface)
fn begin_scene(call: &mut Call) {
    let (address, _flags, target, _region) = (call.u32(), call.u32(), call.u32(), call.u32());
    let (_vertex_sync, _fragment_sync, color, depth_stencil) = (call.u32(), call.u32(), call.u32(), call.u32());
    let memory = call.memory();
    let mut gxm = call.shared.hle.gxm.lock().unwrap();
    let Some(&RenderTarget { width, height, .. }) = gxm.render_targets.get(&target) else {
        return call.ret(INVALID_POINTER);
    };
    let Some(context) = gxm.contexts.get_mut(&address) else {
        return call.ret(INVALID_POINTER);
    };
    if context.scene.is_some() {
        return call.ret(WITHIN_SCENE);
    }
    context.scene = Some(Scene {
        target,
        width,
        height,
        color: ColorSurface::read(memory, color),
        depth_stencil: DepthStencilSurface::read(memory, depth_stencil),
    });
    context.render.region_clip = RegionClip::from_gxm(0xC000_0000, [0, 0, width - 1, height - 1]);
    call.ret(0u32);
}

/// sceGxmEndScene(context, vertex notification, fragment notification):
/// the scene is drawn by now, so both notifications are written.
fn end_scene(call: &mut Call) {
    let (address, vertex_notification, fragment_notification) = (call.u32(), call.u32(), call.u32());
    let ended = {
        let mut gxm = call.shared.hle.gxm.lock().unwrap();
        match gxm.contexts.get_mut(&address) {
            Some(context) => context.scene.take().ok_or(NOT_WITHIN_SCENE),
            None => Err(INVALID_POINTER),
        }
    };
    if let Err(error) = ended {
        return call.ret(error);
    }
    notify(call, vertex_notification);
    notify(call, fragment_notification);
    call.ret(0u32);
}

/// sceGxmMidSceneFlush(context, flags, vertex sync, vertex notification)
fn mid_scene_flush(call: &mut Call) {
    let (_context, _flags, _sync, notification) = (call.u32(), call.u32(), call.u32(), call.u32());
    notify(call, notification);
    call.ret(0u32);
}

/// sceGxmFinish(context), with nothing left to wait for.
fn finish(call: &mut Call) {
    call.ret(0u32);
}

fn ignored(call: &mut Call) {
    call.ret(0u32);
}

/// sceGxmSetVertexStream(context, index, data)
fn set_vertex_stream(call: &mut Call) {
    let result = with_context(call, |context, call| {
        let (index, data) = (call.u32() as usize, call.u32());
        let stream = context.vertex_streams.get_mut(index).ok_or(INVALID_VALUE)?;
        *stream = data;
        Ok(0)
    });
    call.result(result.unwrap_or(Err(INVALID_POINTER)));
}

fn read_texture(memory: &Memory, address: u32) -> [u32; 4] {
    std::array::from_fn(|index| memory.read32(address + index as u32 * 4))
}

/// sceGxmSetVertexTexture(context, index, texture)
fn set_vertex_texture(call: &mut Call) {
    let result = with_context(call, |context, call| {
        let (index, texture) = (call.u32() as usize, call.u32());
        let slot = context.vertex_textures.get_mut(index).ok_or(INVALID_VALUE)?;
        *slot = read_texture(call.memory(), texture);
        Ok(0)
    });
    call.result(result.unwrap_or(Err(INVALID_POINTER)));
}

/// sceGxmSetFragmentTexture(context, index, texture)
fn set_fragment_texture(call: &mut Call) {
    let result = with_context(call, |context, call| {
        let (index, texture) = (call.u32() as usize, call.u32());
        let slot = context.fragment_textures.get_mut(index).ok_or(INVALID_VALUE)?;
        *slot = read_texture(call.memory(), texture);
        Ok(0)
    });
    call.result(result.unwrap_or(Err(INVALID_POINTER)));
}

fn set_uniform_buffer(call: &mut Call, vertex: bool, index: Option<usize>) {
    let result = with_context(call, |context, call| {
        let index = match index {
            Some(index) => index,
            None => call.u32() as usize,
        };
        let data = call.u32();
        let buffers = if vertex { &mut context.vertex_uniform_buffers } else { &mut context.fragment_uniform_buffers };
        let slot = buffers.get_mut(index).ok_or(INVALID_VALUE)?;
        *slot = data;
        Ok(0)
    });
    call.result(result.unwrap_or(Err(INVALID_POINTER)));
}

/// sceGxmSetVertexUniformBuffer(context, index, data)
fn set_vertex_uniform_buffer(call: &mut Call) {
    set_uniform_buffer(call, true, None);
}

/// sceGxmSetFragmentUniformBuffer(context, index, data)
fn set_fragment_uniform_buffer(call: &mut Call) {
    set_uniform_buffer(call, false, None);
}

/// sceGxmSetVertexDefaultUniformBuffer(context, data)
fn set_vertex_default_uniform_buffer(call: &mut Call) {
    set_uniform_buffer(call, true, Some(DEFAULT_UNIFORM_BUFFER));
}

/// sceGxmSetFragmentDefaultUniformBuffer(context, data)
fn set_fragment_default_uniform_buffer(call: &mut Call) {
    set_uniform_buffer(call, false, Some(DEFAULT_UNIFORM_BUFFER));
}

/// changes the front face's state, or the back's, with the call's next
/// argument.
fn set_face(call: &mut Call, front: bool, change: impl FnOnce(&mut kimika_gxm::state::Face, &mut Call)) {
    with_context(call, |context, call| {
        let face = if front { &mut context.render.front } else { &mut context.render.back };
        change(face, call);
    });
}

fn set_front_depth_func(call: &mut Call) {
    set_face(call, true, |face, call| face.depth_func = Compare::from_depth_func(call.u32()));
}

fn set_back_depth_func(call: &mut Call) {
    set_face(call, false, |face, call| face.depth_func = Compare::from_depth_func(call.u32()));
}

/// SCE_GXM_DEPTH_WRITE_DISABLED
const DEPTH_WRITE_DISABLED: u32 = 0x0010_0000;
/// SCE_GXM_FRAGMENT_PROGRAM_DISABLED
const FRAGMENT_PROGRAM_DISABLED: u32 = 0x0020_0000;

fn set_front_depth_write(call: &mut Call) {
    set_face(call, true, |face, call| face.depth_write = call.u32() != DEPTH_WRITE_DISABLED);
}

fn set_back_depth_write(call: &mut Call) {
    set_face(call, false, |face, call| face.depth_write = call.u32() != DEPTH_WRITE_DISABLED);
}

fn set_front_fragment_program_enable(call: &mut Call) {
    set_face(call, true, |face, call| face.fragment_program = call.u32() != FRAGMENT_PROGRAM_DISABLED);
}

fn set_back_fragment_program_enable(call: &mut Call) {
    set_face(call, false, |face, call| face.fragment_program = call.u32() != FRAGMENT_PROGRAM_DISABLED);
}

fn set_front_stencil_ref(call: &mut Call) {
    set_face(call, true, |face, call| face.stencil_reference = call.u32() as u8);
}

fn set_back_stencil_ref(call: &mut Call) {
    set_face(call, false, |face, call| face.stencil_reference = call.u32() as u8);
}

/// sceGxmSetFrontStencilFunc(context, func, stencil fail, depth fail,
/// depth pass, compare mask, write mask) and the back's
fn stencil_func(face: &mut kimika_gxm::state::Face, call: &mut Call) {
    face.stencil_func = Compare::from_stencil_func(call.u32());
    face.stencil_fail = StencilOp::from_gxm(call.u32());
    face.depth_fail = StencilOp::from_gxm(call.u32());
    face.depth_pass = StencilOp::from_gxm(call.u32());
    face.stencil_compare_mask = call.u32() as u8;
    face.stencil_write_mask = call.u32() as u8;
}

fn set_front_stencil_func(call: &mut Call) {
    set_face(call, true, stencil_func);
}

fn set_back_stencil_func(call: &mut Call) {
    set_face(call, false, stencil_func);
}

fn set_front_point_line_width(call: &mut Call) {
    set_face(call, true, |face, call| face.point_line_width = call.u32());
}

fn set_back_point_line_width(call: &mut Call) {
    set_face(call, false, |face, call| face.point_line_width = call.u32());
}

fn set_front_polygon_mode(call: &mut Call) {
    set_face(call, true, |face, call| face.polygon_mode = PolygonMode::from_gxm(call.u32()));
}

fn set_back_polygon_mode(call: &mut Call) {
    set_face(call, false, |face, call| face.polygon_mode = PolygonMode::from_gxm(call.u32()));
}

fn set_front_depth_bias(call: &mut Call) {
    set_face(call, true, |face, call| face.depth_bias = (call.i32(), call.i32()));
}

fn set_back_depth_bias(call: &mut Call) {
    set_face(call, false, |face, call| face.depth_bias = (call.i32(), call.i32()));
}

/// sceGxmSetTwoSidedEnable(context, mode)
fn set_two_sided_enable(call: &mut Call) {
    with_context(call, |context, call| context.render.two_sided = call.u32() != 0);
}

/// sceGxmSetCullMode(context, mode)
fn set_cull_mode(call: &mut Call) {
    with_context(call, |context, call| context.render.cull = Cull::from_gxm(call.u32()));
}

/// sceGxmSetViewport(context, x offset, x scale, y offset, y scale, z
/// offset, z scale), the floats in registers as the hard-float calling
/// convention passes them.
fn set_viewport(call: &mut Call) {
    with_context(call, |context, call| {
        let (x_offset, x_scale, y_offset, y_scale) = (call.f32(), call.f32(), call.f32(), call.f32());
        let (z_offset, z_scale) = (call.f32(), call.f32());
        let viewport = &mut context.render.viewport;
        viewport.offset = [x_offset, y_offset, z_offset];
        viewport.scale = [x_scale, y_scale, z_scale];
    });
}

/// SCE_GXM_VIEWPORT_DISABLED
const VIEWPORT_DISABLED: u32 = 0x0001_0000;

/// sceGxmSetViewportEnable(context, mode)
fn set_viewport_enable(call: &mut Call) {
    with_context(call, |context, call| context.render.viewport.enabled = call.u32() != VIEWPORT_DISABLED);
}

/// sceGxmSetRegionClip(context, mode, x min, y min, x max, y max)
fn set_region_clip(call: &mut Call) {
    with_context(call, |context, call| {
        let mode = call.u32();
        let rectangle = [call.u32(), call.u32(), call.u32(), call.u32()];
        context.render.region_clip = RegionClip::from_gxm(mode, rectangle);
    });
}

/// sceGxmSetDefaultRegionClipAndViewport(context, x max, y max): drawing
/// inside the rectangle, and a viewport that maps it to clip space.
fn set_default_region_clip_and_viewport(call: &mut Call) {
    with_context(call, |context, call| {
        let (x_max, y_max) = (call.u32(), call.u32());
        context.render.region_clip = RegionClip::from_gxm(0xC000_0000, [0, 0, x_max, y_max]);
        let (half_width, half_height) = ((x_max + 1) as f32 / 2.0, (y_max + 1) as f32 / 2.0);
        let viewport = &mut context.render.viewport;
        viewport.offset = [half_width, half_height, 0.5];
        viewport.scale = [half_width, -half_height, 0.5];
    });
}
