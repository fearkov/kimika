//! blending a fragment's color into what the surface holds, as a fragment
//! program's SceGxmBlendInfo asks: a function and two factors for the
//! color and two for the alpha, and a mask of the channels written.

/// SceGxmBlendFunc.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Function {
    /// no blending, the fragment's value goes in as it is.
    None,
    Add,
    Subtract,
    ReverseSubtract,
    Min,
    Max,
}

impl Function {
    pub fn from_gxm(value: u8) -> Function {
        match value {
            1 => Function::Add,
            2 => Function::Subtract,
            3 => Function::ReverseSubtract,
            4 => Function::Min,
            5 => Function::Max,
            _ => Function::None,
        }
    }
}

/// SceGxmBlendFactor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Factor {
    Zero,
    One,
    SourceColor,
    OneMinusSourceColor,
    SourceAlpha,
    OneMinusSourceAlpha,
    DestinationColor,
    OneMinusDestinationColor,
    DestinationAlpha,
    OneMinusDestinationAlpha,
    SourceAlphaSaturate,
    DestinationAlphaSaturate,
}

impl Factor {
    pub fn from_gxm(value: u8) -> Factor {
        match value {
            0 => Factor::Zero,
            2 => Factor::SourceColor,
            3 => Factor::OneMinusSourceColor,
            4 => Factor::SourceAlpha,
            5 => Factor::OneMinusSourceAlpha,
            6 => Factor::DestinationColor,
            7 => Factor::OneMinusDestinationColor,
            8 => Factor::DestinationAlpha,
            9 => Factor::OneMinusDestinationAlpha,
            10 => Factor::SourceAlphaSaturate,
            11 => Factor::DestinationAlphaSaturate,
            _ => Factor::One,
        }
    }

    /// the factor for channel, 3 being alpha, of colors in RGBA order.
    fn value(self, source: [f32; 4], destination: [f32; 4], channel: usize) -> f32 {
        let alpha = channel == 3;
        match self {
            Factor::Zero => 0.0,
            Factor::One => 1.0,
            Factor::SourceColor => source[channel],
            Factor::OneMinusSourceColor => 1.0 - source[channel],
            Factor::SourceAlpha => source[3],
            Factor::OneMinusSourceAlpha => 1.0 - source[3],
            Factor::DestinationColor => destination[channel],
            Factor::OneMinusDestinationColor => 1.0 - destination[channel],
            Factor::DestinationAlpha => destination[3],
            Factor::OneMinusDestinationAlpha => 1.0 - destination[3],
            Factor::SourceAlphaSaturate if alpha => 1.0,
            Factor::SourceAlphaSaturate => source[3].min(1.0 - destination[3]),
            Factor::DestinationAlphaSaturate if alpha => 1.0,
            Factor::DestinationAlphaSaturate => destination[3].min(1.0 - source[3]),
        }
    }
}

/// SCE_GXM_COLOR_MASK_A, _R, _G and _B.
pub const MASK_A: u8 = 1;
pub const MASK_R: u8 = 2;
pub const MASK_G: u8 = 4;
pub const MASK_B: u8 = 8;
pub const MASK_ALL: u8 = 0xF;

/// a SceGxmBlendInfo.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Blend {
    pub mask: u8,
    pub color: (Function, Factor, Factor),
    pub alpha: (Function, Factor, Factor),
}

impl Default for Blend {
    fn default() -> Blend {
        Blend {
            mask: MASK_ALL,
            color: (Function::None, Factor::One, Factor::Zero),
            alpha: (Function::None, Factor::One, Factor::Zero),
        }
    }
}

impl Blend {
    /// the four bytes of a SceGxmBlendInfo: the mask, then the color and
    /// alpha functions, the color factors and the alpha factors, a nibble
    /// each with the first in the low one.
    pub fn from_bytes(bytes: [u8; 4]) -> Blend {
        Blend {
            mask: bytes[0] & MASK_ALL,
            color: (
                Function::from_gxm(bytes[1] & 0xF),
                Factor::from_gxm(bytes[2] & 0xF),
                Factor::from_gxm(bytes[2] >> 4),
            ),
            alpha: (
                Function::from_gxm(bytes[1] >> 4),
                Factor::from_gxm(bytes[3] & 0xF),
                Factor::from_gxm(bytes[3] >> 4),
            ),
        }
    }

    pub fn is_enabled(&self) -> bool {
        self.color.0 != Function::None || self.alpha.0 != Function::None
    }

    /// the color written, in RGBA order, for a fragment's source over the
    /// surface's destination, the channels the mask leaves out kept.
    pub fn apply(&self, source: [f32; 4], destination: [f32; 4]) -> [f32; 4] {
        let masks = [MASK_R, MASK_G, MASK_B, MASK_A];
        std::array::from_fn(|channel| {
            if self.mask & masks[channel] == 0 {
                return destination[channel];
            }
            let (function, source_factor, destination_factor) = if channel == 3 { self.alpha } else { self.color };
            let s = source[channel] * source_factor.value(source, destination, channel);
            let d = destination[channel] * destination_factor.value(source, destination, channel);
            match function {
                Function::None => source[channel],
                Function::Add => s + d,
                Function::Subtract => s - d,
                Function::ReverseSubtract => d - s,
                Function::Min => source[channel].min(destination[channel]),
                Function::Max => source[channel].max(destination[channel]),
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_over_blends_by_the_source_alpha() {
        // add, src alpha and one minus src alpha, for color and alpha
        let blend = Blend::from_bytes([MASK_ALL, 0x11, 0x54, 0x54]);
        assert!(blend.is_enabled());
        let out = blend.apply([1.0, 0.0, 0.0, 0.25], [0.0, 0.0, 1.0, 1.0]);
        assert_eq!(out, [0.25, 0.0, 0.75, 0.0625 + 0.75]);
    }

    #[test]
    fn the_mask_keeps_what_it_leaves_out() {
        let blend = Blend { mask: MASK_R | MASK_A, ..Blend::default() };
        assert!(!blend.is_enabled());
        assert_eq!(blend.apply([1.0, 1.0, 1.0, 1.0], [0.5, 0.5, 0.5, 0.5]), [1.0, 0.5, 0.5, 1.0]);
    }
}
