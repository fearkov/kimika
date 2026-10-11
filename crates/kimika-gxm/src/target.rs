//! what a scene draws into: its color surface, read from the game's memory
//! when the scene begins and written back when it ends, and its depth and
//! stencil, kept here for the length of the scene; and what each fragment
//! goes through on its way there: the region clip, the stencil and depth
//! tests, its program, and blending.

use crate::blend::Blend;
use crate::color::ColorFormat;
use crate::raster::{Bounds, Fragment};
use crate::state::RenderState;
use crate::Bus;

/// a color surface as the scene sees it.
#[derive(Debug, Clone)]
pub struct ColorSurface {
    pub format: ColorFormat,
    pub address: u32,
    /// pixels from one row to the next.
    pub stride: u32,
    pub width: u32,
    pub height: u32,
}

/// a depth and stencil surface as the scene sees it.
#[derive(Debug, Clone, Copy)]
pub struct DepthStencilSurface {
    pub depth: bool,
    pub stencil: bool,
    pub background_depth: f32,
    pub background_stencil: u8,
}

/// what a fragment program gives for a fragment: its color in RGBA order,
/// and its depth if it writes one.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Shaded {
    pub color: [f32; 4],
    pub depth: Option<f32>,
}

pub struct Target {
    pub width: u32,
    pub height: u32,
    color: Option<(ColorSurface, Vec<u8>)>,
    depth: Option<Vec<f32>>,
    stencil: Option<Vec<u8>>,
}

impl Target {
    /// a scene's target, its color surface read from memory.
    pub fn begin(
        bus: &impl Bus,
        width: u32,
        height: u32,
        color: Option<ColorSurface>,
        depth_stencil: Option<DepthStencilSurface>,
    ) -> Target {
        let color = color.map(|surface| {
            let mut bytes = vec![0; surface.stride as usize * surface.height as usize * surface.format.bytes_per_pixel()];
            bus.read(surface.address, &mut bytes);
            (surface, bytes)
        });
        let pixels = width as usize * height as usize;
        let depth = depth_stencil.filter(|surface| surface.depth).map(|surface| vec![surface.background_depth; pixels]);
        let stencil =
            depth_stencil.filter(|surface| surface.stencil).map(|surface| vec![surface.background_stencil; pixels]);
        Target { width, height, color, depth, stencil }
    }

    /// writes the color surface back to memory, the scene done.
    pub fn end(&self, bus: &impl Bus) {
        if let Some((surface, bytes)) = &self.color {
            bus.write(surface.address, bytes);
        }
    }

    /// the pixels draws may land in.
    pub fn bounds(&self) -> Bounds {
        let (mut width, mut height) = (self.width, self.height);
        if let Some((surface, _)) = &self.color {
            width = width.min(surface.width);
            height = height.min(surface.height);
        }
        Bounds { x_min: 0, y_min: 0, x_max: width.saturating_sub(1), y_max: height.saturating_sub(1) }
    }

    fn color_at(&self, x: u32, y: u32) -> Option<std::ops::Range<usize>> {
        let (surface, bytes) = self.color.as_ref()?;
        let size = surface.format.bytes_per_pixel();
        let start = (y as usize * surface.stride as usize + x as usize) * size;
        (start + size <= bytes.len()).then(|| start..start + size)
    }

    /// the color at a pixel, in RGBA order.
    pub fn read(&self, x: u32, y: u32) -> Option<[f32; 4]> {
        let range = self.color_at(x, y)?;
        let (surface, bytes) = self.color.as_ref()?;
        Some(surface.format.decode(&bytes[range]))
    }

    pub fn write(&mut self, x: u32, y: u32, color: [f32; 4]) {
        let Some(range) = self.color_at(x, y) else { return };
        if let Some((surface, bytes)) = &mut self.color {
            surface.format.encode(color, &mut bytes[range]);
        }
    }

    /// runs a fragment through the tests, its program and blending into
    /// the target. shade gives none for a fragment the program discards.
    /// late says whether the program decides the fragment's depth or
    /// whether it lives, so that the tests have to wait for it.
    pub fn fragment(
        &mut self,
        state: &RenderState,
        blend: &Blend,
        fragment: &Fragment,
        late: bool,
        shade: &mut impl FnMut(&Fragment) -> Option<Shaded>,
    ) {
        let (x, y) = (fragment.x, fragment.y);
        if !state.region_clip.allows(x, y) || x >= self.width || y >= self.height {
            return;
        }
        let face = state.face(fragment.front_facing);
        let index = (y * self.width + x) as usize;
        let mut shaded = None;
        if late && face.fragment_program {
            match shade(fragment) {
                Some(result) => shaded = Some(result),
                None => return,
            }
        }
        let depth = shaded.and_then(|result| result.depth).unwrap_or(fragment.z).clamp(0.0, 1.0);
        // the stencil test, then the depth test
        if let Some(stencil) = &mut self.stencil {
            let stored = stencil[index];
            let mask = face.stencil_compare_mask;
            let passes = face.stencil_func.test(face.stencil_reference & mask, stored & mask);
            let depth_passes = passes
                && self.depth.as_ref().is_none_or(|buffer| face.depth_func.test(depth, buffer[index]));
            let op = if !passes {
                face.stencil_fail
            } else if !depth_passes {
                face.depth_fail
            } else {
                face.depth_pass
            };
            let updated = op.apply(stored, face.stencil_reference);
            let write_mask = face.stencil_write_mask;
            stencil[index] = (stored & !write_mask) | (updated & write_mask);
            if !depth_passes {
                return;
            }
        } else if let Some(buffer) = &self.depth {
            if !face.depth_func.test(depth, buffer[index]) {
                return;
            }
        }
        if face.depth_write {
            if let Some(buffer) = &mut self.depth {
                buffer[index] = depth;
            }
        }
        if !face.fragment_program {
            return;
        }
        let shaded = match shaded {
            Some(shaded) => shaded,
            None => match shade(fragment) {
                Some(shaded) => shaded,
                None => return,
            },
        };
        let Some(destination) = self.read(x, y) else { return };
        let color = if blend.is_enabled() || blend.mask != crate::blend::MASK_ALL {
            blend.apply(shaded.color, destination)
        } else {
            shaded.color
        };
        self.write(x, y, color);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{Compare, Face};
    use crate::Flat;

    fn target(memory: &Flat) -> Target {
        let surface = ColorSurface {
            format: ColorFormat::from_gxm(0).unwrap(),
            address: memory.base,
            stride: 4,
            width: 4,
            height: 4,
        };
        let depth = DepthStencilSurface { depth: true, stencil: true, background_depth: 1.0, background_stencil: 0 };
        Target::begin(memory, 4, 4, Some(surface), Some(depth))
    }

    fn fragment(x: u32, y: u32, z: f32) -> Fragment<'static> {
        Fragment { x, y, z, w: 1.0, front_facing: true, varyings: &[] }
    }

    #[test]
    fn the_depth_test_keeps_the_nearest_fragment() {
        let memory = Flat::new(0x8100_0000, 64);
        let mut target = target(&memory);
        let face = Face { depth_func: Compare::Less, ..Face::default() };
        let state = RenderState { front: face, back: face, ..RenderState::default() };
        let mut red = |_: &Fragment| Some(Shaded { color: [1.0, 0.0, 0.0, 1.0], depth: None });
        let mut green = |_: &Fragment| Some(Shaded { color: [0.0, 1.0, 0.0, 1.0], depth: None });
        target.fragment(&state, &Blend::default(), &fragment(1, 1, 0.5), false, &mut red);
        target.fragment(&state, &Blend::default(), &fragment(1, 1, 0.7), false, &mut green);
        assert_eq!(target.read(1, 1), Some([1.0, 0.0, 0.0, 1.0]));
        target.fragment(&state, &Blend::default(), &fragment(1, 1, 0.2), false, &mut green);
        assert_eq!(target.read(1, 1), Some([0.0, 1.0, 0.0, 1.0]));
        target.end(&memory);
        assert_eq!(&memory.bytes.borrow()[20..24], &[0, 255, 0, 255]);
    }

    #[test]
    fn a_discarded_fragment_leaves_nothing_behind() {
        let memory = Flat::new(0x8100_0000, 64);
        let mut target = target(&memory);
        let state = RenderState::default();
        let mut discard = |_: &Fragment| None;
        target.fragment(&state, &Blend::default(), &fragment(0, 0, 0.5), true, &mut discard);
        assert_eq!(target.read(0, 0), Some([0.0, 0.0, 0.0, 0.0]));
    }
}
