//! the fixed-function state a draw runs with: depth and stencil tests for
//! each face, how polygons are filled, culling, the viewport and the
//! region clip, with the SceGxm values the game sets them with.

/// a comparison, as the depth and stencil tests have it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Compare {
    Never,
    Less,
    Equal,
    LessEqual,
    Greater,
    NotEqual,
    GreaterEqual,
    Always,
}

impl Compare {
    /// from the three bits SceGxmDepthFunc and SceGxmStencilFunc keep at
    /// different places.
    pub fn from_bits(bits: u32) -> Compare {
        match bits & 7 {
            0 => Compare::Never,
            1 => Compare::Less,
            2 => Compare::Equal,
            3 => Compare::LessEqual,
            4 => Compare::Greater,
            5 => Compare::NotEqual,
            6 => Compare::GreaterEqual,
            _ => Compare::Always,
        }
    }

    /// a SceGxmDepthFunc.
    pub fn from_depth_func(value: u32) -> Compare {
        Compare::from_bits(value >> 22)
    }

    /// a SceGxmStencilFunc.
    pub fn from_stencil_func(value: u32) -> Compare {
        Compare::from_bits(value >> 25)
    }

    pub fn test<T: PartialOrd>(self, incoming: T, stored: T) -> bool {
        match self {
            Compare::Never => false,
            Compare::Less => incoming < stored,
            Compare::Equal => incoming == stored,
            Compare::LessEqual => incoming <= stored,
            Compare::Greater => incoming > stored,
            Compare::NotEqual => incoming != stored,
            Compare::GreaterEqual => incoming >= stored,
            Compare::Always => true,
        }
    }
}

/// what a stencil test does to the stored value, SceGxmStencilOp.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StencilOp {
    Keep,
    Zero,
    Replace,
    Increment,
    Decrement,
    Invert,
    IncrementWrap,
    DecrementWrap,
}

impl StencilOp {
    pub fn from_gxm(value: u32) -> StencilOp {
        match value & 7 {
            0 => StencilOp::Keep,
            1 => StencilOp::Zero,
            2 => StencilOp::Replace,
            3 => StencilOp::Increment,
            4 => StencilOp::Decrement,
            5 => StencilOp::Invert,
            6 => StencilOp::IncrementWrap,
            _ => StencilOp::DecrementWrap,
        }
    }

    pub fn apply(self, stored: u8, reference: u8) -> u8 {
        match self {
            StencilOp::Keep => stored,
            StencilOp::Zero => 0,
            StencilOp::Replace => reference,
            StencilOp::Increment => stored.saturating_add(1),
            StencilOp::Decrement => stored.saturating_sub(1),
            StencilOp::Invert => !stored,
            StencilOp::IncrementWrap => stored.wrapping_add(1),
            StencilOp::DecrementWrap => stored.wrapping_sub(1),
        }
    }
}

/// how a face's polygons are drawn, SceGxmPolygonMode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PolygonMode {
    TriangleFill,
    Line,
    Point,
    TriangleLine,
    TrianglePoint,
}

impl PolygonMode {
    pub fn from_gxm(value: u32) -> PolygonMode {
        match value {
            0x0000_8000 => PolygonMode::Line,
            0x0001_0000 | 0x0001_8000 | 0x0002_0000 => PolygonMode::Point,
            0x0002_8000 => PolygonMode::TriangleLine,
            0x0003_0000 => PolygonMode::TrianglePoint,
            _ => PolygonMode::TriangleFill,
        }
    }
}

/// the state each face, front and back, has of its own.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Face {
    pub depth_func: Compare,
    pub depth_write: bool,
    pub stencil_func: Compare,
    pub stencil_fail: StencilOp,
    pub depth_fail: StencilOp,
    pub depth_pass: StencilOp,
    pub stencil_compare_mask: u8,
    pub stencil_write_mask: u8,
    pub stencil_reference: u8,
    pub polygon_mode: PolygonMode,
    pub point_line_width: u32,
    pub fragment_program: bool,
    /// the depth bias's factor and units.
    pub depth_bias: (i32, i32),
}

impl Default for Face {
    fn default() -> Face {
        Face {
            depth_func: Compare::Always,
            depth_write: true,
            stencil_func: Compare::Always,
            stencil_fail: StencilOp::Keep,
            depth_fail: StencilOp::Keep,
            depth_pass: StencilOp::Keep,
            stencil_compare_mask: 0xFF,
            stencil_write_mask: 0xFF,
            stencil_reference: 0,
            polygon_mode: PolygonMode::TriangleFill,
            point_line_width: 1,
            fragment_program: true,
            depth_bias: (0, 0),
        }
    }
}

/// which faces are culled, SceGxmCullMode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Cull {
    #[default]
    None,
    Clockwise,
    CounterClockwise,
}

impl Cull {
    pub fn from_gxm(value: u32) -> Cull {
        match value {
            1 => Cull::Clockwise,
            2 => Cull::CounterClockwise,
            _ => Cull::None,
        }
    }
}

/// the viewport transform: window coordinates are the clip coordinates
/// divided by w, times the scale, plus the offset.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Viewport {
    pub enabled: bool,
    pub offset: [f32; 3],
    pub scale: [f32; 3],
}

impl Default for Viewport {
    fn default() -> Viewport {
        Viewport { enabled: true, offset: [0.0, 0.0, 0.5], scale: [1.0, -1.0, 0.5] }
    }
}

/// which part of the surface draws reach, SceGxmRegionClipMode with its
/// rectangle, inclusive.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RegionClip {
    #[default]
    None,
    /// nothing is drawn.
    All,
    /// only outside the rectangle.
    Outside([u32; 4]),
    /// only inside the rectangle.
    Inside([u32; 4]),
}

impl RegionClip {
    pub fn from_gxm(mode: u32, rectangle: [u32; 4]) -> RegionClip {
        match mode {
            0x4000_0000 => RegionClip::All,
            0x8000_0000 => RegionClip::Outside(rectangle),
            0xC000_0000 => RegionClip::Inside(rectangle),
            _ => RegionClip::None,
        }
    }

    pub fn allows(self, x: u32, y: u32) -> bool {
        let inside = |[x_min, y_min, x_max, y_max]: [u32; 4]| x >= x_min && x <= x_max && y >= y_min && y <= y_max;
        match self {
            RegionClip::None => true,
            RegionClip::All => false,
            RegionClip::Outside(rectangle) => !inside(rectangle),
            RegionClip::Inside(rectangle) => inside(rectangle),
        }
    }
}

/// every piece of fixed-function state a draw reads.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct RenderState {
    pub front: Face,
    pub back: Face,
    /// whether back faces use their own state rather than the front's.
    pub two_sided: bool,
    pub cull: Cull,
    pub viewport: Viewport,
    pub region_clip: RegionClip,
}

impl RenderState {
    pub fn face(&self, front: bool) -> &Face {
        if front || !self.two_sided {
            &self.front
        } else {
            &self.back
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gxm_values_name_the_right_tests_and_ops() {
        assert_eq!(Compare::from_depth_func(0x00C0_0000), Compare::LessEqual);
        assert_eq!(Compare::from_depth_func(0x01C0_0000), Compare::Always);
        assert_eq!(Compare::from_stencil_func(0x0400_0000), Compare::Equal);
        assert_eq!(Compare::from_stencil_func(0x0E00_0000), Compare::Always);
        assert!(Compare::Less.test(0.25, 0.5) && !Compare::Less.test(0.5, 0.5));
        assert_eq!(StencilOp::from_gxm(3).apply(255, 0), 255);
        assert_eq!(StencilOp::from_gxm(6).apply(255, 0), 0);
        assert_eq!(StencilOp::from_gxm(2).apply(7, 9), 9);
        assert_eq!(PolygonMode::from_gxm(0x0001_8000), PolygonMode::Point);
    }

    #[test]
    fn a_region_clip_keeps_one_side_of_its_rectangle() {
        let inside = RegionClip::from_gxm(0xC000_0000, [10, 10, 20, 20]);
        assert!(inside.allows(10, 20) && !inside.allows(21, 15));
        let outside = RegionClip::from_gxm(0x8000_0000, [10, 10, 20, 20]);
        assert!(!outside.allows(15, 15) && outside.allows(0, 0));
        assert!(!RegionClip::from_gxm(0x4000_0000, [0; 4]).allows(0, 0));
    }
}
