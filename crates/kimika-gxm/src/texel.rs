//! the texel formats of textures: the channels a texel's bits hold, by
//! base format, and what color they make, by swizzle; and the block
//! compressed formats, which give a 4x4 block of texels at a time.
//!
//! a SceGxmTextureFormat is a base format in its top byte or'd with a
//! swizzle in bits 12 to 14. a texel's channels are counted from its
//! lowest bits, and the swizzles name the color they make from alpha
//! down to red: 000R gives the first channel as red with the rest 0, R111
//! gives it as alpha with white.

use crate::color::{Kind, A, B, G, R};
use crate::convert::{f16_to_f32, snorm, unorm};

/// where a color channel comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    Zero,
    One,
    /// the texel's channel, counted from its lowest bits.
    Channel(usize),
}

/// how a base format packs its texels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Packing {
    /// channels of the given kinds and widths, from the lowest bits.
    Plain,
    /// a 4x4 block of BC1, BC2 or BC3, or of BC4 or BC5 signed or not.
    Bc(u8, bool),
    /// an index into a palette of 32-bit colors, of 4 or 8 bits.
    Palette(u32),
    /// a format this cannot read, which samples as magenta.
    Unknown,
}

/// a texture format's texels.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TexelFormat {
    pub packing: Packing,
    /// each channel's kind and width, from the lowest bits.
    pub channels: Vec<(Kind, u32)>,
    pub bits: u32,
    /// red, green, blue and alpha.
    pub swizzle: [Source; 4],
}

/// the kinds and widths a base format gives red, green, blue and alpha,
/// in the order an ABGR swizzle packs them, from the lowest bits.
fn plain(base: u32) -> Option<Vec<(Kind, u32)>> {
    use Kind::*;
    Some(match base {
        0x00 => vec![(Unorm, 8)],
        0x01 => vec![(Snorm, 8)],
        0x02 => vec![(Unorm, 4); 4],
        0x03 => vec![(Unorm, 2), (Unorm, 3), (Unorm, 3), (Unorm, 8)],
        0x04 => vec![(Unorm, 5), (Unorm, 5), (Unorm, 5), (Unorm, 1)],
        0x05 => vec![(Unorm, 5), (Unorm, 6), (Unorm, 5)],
        0x06 => vec![(Snorm, 5), (Snorm, 5), (Unorm, 6)],
        0x07 => vec![(Unorm, 8); 2],
        0x08 => vec![(Snorm, 8); 2],
        0x09 => vec![(Unorm, 16)],
        0x0A => vec![(Snorm, 16)],
        0x0B => vec![(Float, 16)],
        0x0C => vec![(Unorm, 8); 4],
        0x0D => vec![(Snorm, 8); 4],
        0x0E => vec![(Unorm, 10), (Unorm, 10), (Unorm, 10), (Unorm, 2)],
        0x0F => vec![(Unorm, 16); 2],
        0x10 => vec![(Snorm, 16); 2],
        0x11 => vec![(Float, 16); 2],
        0x12 | 0x13 => vec![(Float, 32)],
        0x14 => vec![(Unorm, 8), (Snorm, 8), (Snorm, 8), (Unorm, 8)],
        0x15 => vec![(Unorm, 24), (Unorm, 8)],
        // 32-bit integers, read as fractions as nothing else fits
        0x17 => vec![(Unorm, 32)],
        0x18 => vec![(Snorm, 32)],
        0x1A => vec![(SmallFloat, 11), (SmallFloat, 11), (SmallFloat, 10)],
        0x1B => vec![(Float, 16); 4],
        0x1C => vec![(Unorm, 16); 4],
        0x1D => vec![(Snorm, 16); 4],
        0x1E => vec![(Float, 32); 2],
        0x1F => vec![(Unorm, 32); 2],
        0x98 => vec![(Unorm, 8); 3],
        0x99 => vec![(Snorm, 8); 3],
        0x9A => vec![(SmallFloat, 10), (SmallFloat, 10), (SmallFloat, 10), (Unorm, 2)],
        _ => return None,
    })
}

impl TexelFormat {
    pub fn from_gxm(format: u32) -> TexelFormat {
        let base = format >> 24;
        let swizzle = (format >> 12) & 7;
        let packing = match base {
            0x85 => Packing::Bc(1, false),
            0x86 => Packing::Bc(2, false),
            0x87 => Packing::Bc(3, false),
            0x88 => Packing::Bc(4, false),
            0x89 => Packing::Bc(4, true),
            0x8A => Packing::Bc(5, false),
            0x8B => Packing::Bc(5, true),
            0x94 => Packing::Palette(4),
            0x95 => Packing::Palette(8),
            _ if plain(base).is_some() => Packing::Plain,
            _ => Packing::Unknown,
        };
        let mut channels = plain(base).unwrap_or_default();
        let count = match packing {
            Packing::Plain => channels.len(),
            Packing::Bc(1..=3, _) | Packing::Palette(_) => 4,
            Packing::Bc(4, _) => 1,
            Packing::Bc(_, _) => 2,
            Packing::Unknown => 0,
        };
        use Source::{Channel as C, One, Zero};
        let swizzle = match count {
            4 => {
                // the order the channels sit in, from the lowest bits, and
                // whether alpha is forced to 1
                let (order, opaque) = match swizzle {
                    0 => ([R, G, B, A], false),
                    1 => ([B, G, R, A], false),
                    2 => ([A, B, G, R], false),
                    3 => ([A, R, G, B], false),
                    4 => ([R, G, B, A], true),
                    5 => ([B, G, R, A], true),
                    6 => ([A, B, G, R], true),
                    _ => ([A, R, G, B], true),
                };
                // alpha keeps its width wherever it goes, so the widths move
                // with the channels
                if packing == Packing::Plain {
                    let widths = channels.clone();
                    channels = order.iter().map(|&channel| widths[channel]).collect();
                }
                let mut sources = [Zero; 4];
                for (position, &channel) in order.iter().enumerate() {
                    sources[channel] = C(position);
                }
                if opaque {
                    sources[A] = One;
                }
                sources
            }
            3 => {
                let order = if swizzle == 0 { [R, G, B] } else { [B, G, R] };
                let widths = channels.clone();
                channels = order.iter().map(|&channel| widths[channel]).collect();
                let mut sources = [Zero, Zero, Zero, One];
                for (position, &channel) in order.iter().enumerate() {
                    sources[channel] = C(position);
                }
                sources
            }
            2 => match swizzle {
                1 => [C(0), C(1), Zero, Zero],
                2 => [C(0), C(0), C(0), C(1)],
                3 => [C(1), C(1), C(1), C(0)],
                4 => [C(0), C(1), C(0), C(1)],
                5 => [C(1), C(0), Zero, Zero],
                _ => [C(0), C(1), Zero, One],
            },
            1 => match swizzle {
                1 => [C(0), Zero, Zero, Zero],
                2 => [C(0), One, One, One],
                3 => [C(0), C(0), C(0), C(0)],
                4 => [C(0), C(0), C(0), Zero],
                5 => [C(0), C(0), C(0), One],
                6 => [Zero, Zero, Zero, C(0)],
                7 => [One, One, One, C(0)],
                _ => [C(0), Zero, Zero, One],
            },
            _ => [One, Zero, One, One],
        };
        let bits = match packing {
            Packing::Plain => channels.iter().map(|&(_, bits)| bits).sum(),
            Packing::Palette(bits) => bits,
            Packing::Bc(1 | 4, _) => 4,
            Packing::Bc(_, _) => 8,
            Packing::Unknown => 32,
        };
        TexelFormat { packing, channels, bits, swizzle }
    }

    /// whether texels come in 4x4 blocks.
    pub fn is_block(&self) -> bool {
        matches!(self.packing, Packing::Bc(_, _))
    }

    /// the bytes a 4x4 block takes, for a block format.
    pub fn block_bytes(&self) -> usize {
        match self.packing {
            Packing::Bc(1 | 4, _) => 8,
            _ => 16,
        }
    }

    /// the color a texel's raw channels make, through the swizzle.
    pub fn swizzled(&self, channels: [f32; 4]) -> [f32; 4] {
        self.swizzle.map(|source| match source {
            Source::Zero => 0.0,
            Source::One => 1.0,
            Source::Channel(index) => channels[index],
        })
    }

    /// a plain texel's channels, from its bytes, the first from the lowest
    /// bits.
    pub fn channels(&self, bytes: &[u8]) -> [f32; 4] {
        let mut raw = [0u8; 16];
        let length = bytes.len().min(16);
        raw[..length].copy_from_slice(&bytes[..length]);
        let value = u128::from_le_bytes(raw);
        let mut out = [0.0; 4];
        let mut shift = 0;
        for (index, &(kind, bits)) in self.channels.iter().enumerate().take(4) {
            let field = ((value >> shift) & ((1u128 << bits) - 1)) as u32;
            shift += bits;
            out[index] = match kind {
                Kind::Unorm => unorm(field, bits),
                Kind::Snorm => snorm(((field << (32 - bits)) as i32) >> (32 - bits), bits),
                Kind::Float if bits == 16 => f16_to_f32(field as u16),
                Kind::Float => f32::from_bits(field),
                Kind::SmallFloat => small_float(field, bits),
            };
        }
        out
    }
}

fn small_float(bits: u32, width: u32) -> f32 {
    let mantissa_bits = width - 5;
    let exponent = bits >> mantissa_bits;
    let mantissa = (bits & ((1 << mantissa_bits) - 1)) as f32 / (1 << mantissa_bits) as f32;
    match exponent {
        0 => mantissa * 2f32.powi(-14),
        31 => f32::INFINITY,
        _ => (1.0 + mantissa) * 2f32.powi(exponent as i32 - 15),
    }
}

fn rgb565(value: u16) -> [f32; 3] {
    [unorm((value & 0x1F) as u32, 5), unorm(((value >> 5) & 0x3F) as u32, 6), unorm((value >> 11) as u32, 5)]
}

/// the 16 colors of a BC1 block's color half, in RGBA order, row by row;
/// with three colors and transparent black when the first endpoint is not
/// above the second, unless the block always has four.
fn bc1_colors(block: &[u8], always_four: bool) -> [[f32; 4]; 16] {
    let first = u16::from_le_bytes([block[0], block[1]]);
    let second = u16::from_le_bytes([block[2], block[3]]);
    let (a, b) = (rgb565(first), rgb565(second));
    let mix = |weight_a: f32, weight_b: f32| -> [f32; 4] {
        let total = weight_a + weight_b;
        [
            (a[0] * weight_a + b[0] * weight_b) / total,
            (a[1] * weight_a + b[1] * weight_b) / total,
            (a[2] * weight_a + b[2] * weight_b) / total,
            1.0,
        ]
    };
    let palette = if first > second || always_four {
        [mix(1.0, 0.0), mix(0.0, 1.0), mix(2.0, 1.0), mix(1.0, 2.0)]
    } else {
        [mix(1.0, 0.0), mix(0.0, 1.0), mix(1.0, 1.0), [0.0; 4]]
    };
    let indices = u32::from_le_bytes([block[4], block[5], block[6], block[7]]);
    std::array::from_fn(|texel| palette[((indices >> (texel * 2)) & 3) as usize])
}

/// the 16 values of a BC4 style block of 8 bytes: two endpoints and 3-bit
/// indices.
fn bc4_values(block: &[u8], signed: bool) -> [f32; 16] {
    let (first, second) = if signed {
        (snorm(block[0] as i8 as i32, 8), snorm(block[1] as i8 as i32, 8))
    } else {
        (unorm(block[0] as u32, 8), unorm(block[1] as u32, 8))
    };
    let above = if signed { (block[0] as i8) > (block[1] as i8) } else { block[0] > block[1] };
    let palette: [f32; 8] = if above {
        std::array::from_fn(|index| match index {
            0 => first,
            1 => second,
            _ => (first * (8 - index) as f32 + second * (index - 1) as f32) / 7.0,
        })
    } else {
        std::array::from_fn(|index| match index {
            0 => first,
            1 => second,
            6 => if signed { -1.0 } else { 0.0 },
            7 => 1.0,
            _ => (first * (6 - index) as f32 + second * (index - 1) as f32) / 5.0,
        })
    };
    let mut indices = 0u64;
    for (index, &byte) in block[2..8].iter().enumerate() {
        indices |= (byte as u64) << (index * 8);
    }
    std::array::from_fn(|texel| palette[((indices >> (texel * 3)) & 7) as usize])
}

/// the 16 texels of a BCn block, row by row, as raw channels before the
/// swizzle: RGBA for BC1 to BC3, the value for BC4, the two values for
/// BC5.
pub fn decode_block(kind: u8, signed: bool, block: &[u8]) -> [[f32; 4]; 16] {
    match kind {
        1 => bc1_colors(block, false),
        2 => {
            let mut colors = bc1_colors(&block[8..16], true);
            for (texel, color) in colors.iter_mut().enumerate() {
                let byte = block[texel / 2];
                let nibble = if texel % 2 == 0 { byte & 0xF } else { byte >> 4 };
                color[3] = unorm(nibble as u32, 4);
            }
            colors
        }
        3 => {
            let mut colors = bc1_colors(&block[8..16], true);
            let alphas = bc4_values(&block[0..8], false);
            for (color, alpha) in colors.iter_mut().zip(alphas) {
                color[3] = alpha;
            }
            colors
        }
        4 => bc4_values(&block[0..8], signed).map(|value| [value, 0.0, 0.0, 1.0]),
        _ => {
            let (first, second) = (bc4_values(&block[0..8], signed), bc4_values(&block[8..16], signed));
            std::array::from_fn(|texel| [first[texel], second[texel], 0.0, 1.0])
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_formats_swizzle_into_colors() {
        let abgr = TexelFormat::from_gxm(0x0C00_0000);
        assert_eq!(abgr.bits, 32);
        assert_eq!(abgr.swizzled(abgr.channels(&[255, 0, 0, 128])), [1.0, 0.0, 0.0, 128.0 / 255.0]);
        let argb = TexelFormat::from_gxm(0x0C00_1000);
        assert_eq!(argb.swizzled(argb.channels(&[255, 0, 0, 255])), [0.0, 0.0, 1.0, 1.0]);
        // an alpha texture is white with the texel as alpha
        let alpha = TexelFormat::from_gxm(0x0000_7000);
        assert_eq!(alpha.swizzled(alpha.channels(&[51])), [1.0, 1.0, 1.0, 0.2]);
        // luminance and alpha
        let la = TexelFormat::from_gxm(0x0700_2000);
        assert_eq!(la.swizzled(la.channels(&[255, 0])), [1.0, 1.0, 1.0, 0.0]);
    }

    #[test]
    fn a_one_bit_alpha_stays_one_bit_when_it_moves() {
        // U5U5U5U1_RGBA: alpha in the lowest bit, red in the top five
        let rgba = TexelFormat::from_gxm(0x0400_2000);
        assert_eq!(rgba.channels[0], (Kind::Unorm, 1));
        assert_eq!(rgba.swizzled(rgba.channels(&0xF801u16.to_le_bytes())), [1.0, 0.0, 0.0, 1.0]);
        // X1U5U5U5_1BGR ignores its alpha bit
        let opaque = TexelFormat::from_gxm(0x0400_4000);
        assert_eq!(opaque.swizzled(opaque.channels(&0x001Fu16.to_le_bytes())), [1.0, 0.0, 0.0, 1.0]);
    }

    #[test]
    fn bc1_blocks_mix_their_endpoints() {
        // white and black endpoints, the texels 0, 1, 2, 3 in turn
        let block = [0xFF, 0xFF, 0x00, 0x00, 0b1110_0100, 0, 0, 0];
        let texels = decode_block(1, false, &block);
        assert_eq!(texels[0], [1.0, 1.0, 1.0, 1.0]);
        assert_eq!(texels[1], [0.0, 0.0, 0.0, 1.0]);
        assert!((texels[2][0] - 2.0 / 3.0).abs() < 1e-6);
        assert!((texels[3][0] - 1.0 / 3.0).abs() < 1e-6);
        // with the endpoints the other way round, index 3 is transparent
        let block = [0x00, 0x00, 0xFF, 0xFF, 0b1100_0000, 0, 0, 0];
        assert_eq!(decode_block(1, false, &block)[3], [0.0; 4]);
    }

    #[test]
    fn bc3_alpha_interpolates_in_sevenths() {
        let mut block = [0u8; 16];
        block[0] = 255;
        block[1] = 0;
        // texel 0 takes index 2, 6/7 of the way to the first endpoint
        block[2] = 2;
        block[8..10].copy_from_slice(&0xFFFFu16.to_le_bytes());
        let texels = decode_block(3, false, &block);
        assert!((texels[0][3] - 6.0 / 7.0).abs() < 1e-6);
        assert_eq!(texels[1][3], 1.0);
    }
}
