//! the formats of color surfaces: how many bits each channel of a pixel
//! takes, what kind of number they hold, and in what order they are
//! packed, the first from the lowest bit.
//!
//! a SceGxmColorFormat is a base format, which gives each channel its
//! width and kind, or'd with a swizzle, which places the channels: ABGR
//! has red in the lowest bits and alpha in the highest, and the others
//! name their channels from the highest bits down too.

use crate::convert::{f16_to_f32, f32_to_f16, snorm, to_snorm, to_unorm, unorm};

/// a channel of a color, as an index into RGBA.
pub const R: usize = 0;
pub const G: usize = 1;
pub const B: usize = 2;
pub const A: usize = 3;

/// what a channel's bits hold.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// a fraction from 0 to 1.
    Unorm,
    /// a fraction from -1 to 1.
    Snorm,
    /// an IEEE float of 16 or 32 bits.
    Float,
    /// an unsigned float without a sign, of 10 or 11 bits.
    SmallFloat,
}

/// a channel's place in a pixel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Field {
    pub channel: usize,
    pub kind: Kind,
    pub bits: u32,
    pub shift: u32,
}

/// a color surface's format, its fields from the lowest bit up.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ColorFormat {
    pub fields: Vec<Field>,
    pub bits: u32,
}

/// the widths and kinds a base format gives red, green, blue and alpha,
/// none for a channel it does not have.
fn base(format: u32) -> Option<[Option<(Kind, u32)>; 4]> {
    use Kind::*;
    let u = |bits| Some((Unorm, bits));
    let s = |bits| Some((Snorm, bits));
    let f = |bits| Some((Float, bits));
    let small = |bits| Some((SmallFloat, bits));
    Some(match format & 0xF180_0000 {
        0x0000_0000 => [u(8), u(8), u(8), u(8)],
        0x1000_0000 => [u(8), u(8), u(8), None],
        0x3000_0000 => [u(5), u(6), u(5), None],
        0x4000_0000 => [u(5), u(5), u(5), u(1)],
        0x5000_0000 => [u(4), u(4), u(4), u(4)],
        0x6000_0000 => [u(3), u(3), u(2), u(8)],
        0xF000_0000 => [f(16), None, None, None],
        0x0080_0000 => [f(16), f(16), None, None],
        0x1080_0000 => [f(32), None, None, None],
        0x2080_0000 => [s(16), None, None, None],
        0x3080_0000 => [s(16), s(16), None, None],
        0x4080_0000 => [u(16), None, None, None],
        0x5080_0000 => [u(16), u(16), None, None],
        0x6080_0000 => [u(10), u(10), u(10), u(2)],
        0x8080_0000 => [u(8), None, None, None],
        0x9080_0000 => [s(8), None, None, None],
        0xA080_0000 => [s(5), s(5), u(6), None],
        0xB080_0000 => [u(8), u(8), None, None],
        0xC080_0000 => [s(8), s(8), None, None],
        0xD080_0000 => [u(8), s(8), s(8), u(8)],
        0xE080_0000 => [s(8), s(8), s(8), s(8)],
        0x0100_0000 => [f(16), f(16), f(16), f(16)],
        0x1100_0000 => [f(32), f(32), None, None],
        0x2100_0000 => [small(11), small(11), small(10), None],
        0x4100_0000 => [small(10), small(10), small(10), u(2)],
        _ => return None,
    })
}

impl ColorFormat {
    /// a SceGxmColorFormat's layout, none for one this does not draw to.
    pub fn from_gxm(format: u32) -> Option<ColorFormat> {
        let widths = base(format)?;
        let present: Vec<usize> = (0..4).filter(|&channel| widths[channel].is_some()).collect();
        let swizzle = (format >> 20) & 3;
        let order: Vec<usize> = match present.len() {
            4 => match swizzle {
                0 => vec![R, G, B, A],
                1 => vec![B, G, R, A],
                2 => vec![A, B, G, R],
                _ => vec![A, R, G, B],
            },
            3 => match swizzle {
                0 => vec![R, G, B],
                _ => vec![B, G, R],
            },
            // the second channel of a two channel format is green, or
            // alpha for the formats that leave green out
            2 => {
                let second = present[1];
                match swizzle {
                    0 => vec![R, second],
                    1 => vec![second, R],
                    2 => vec![A, R],
                    _ => vec![R, A],
                }
            }
            _ => match swizzle {
                0 => vec![R],
                // U8_A and S8_A, or the green of the others
                _ if format & 0xE080_0000 == 0x8080_0000 => vec![A],
                _ => vec![G],
            },
        };
        let mut shift = 0;
        let mut fields = Vec::new();
        for channel in order {
            // a channel the swizzle names but the base has not, as alpha in
            // place of green, takes that slot's width
            let (kind, bits) = widths[channel].or(widths[G]).or(widths[R])?;
            fields.push(Field { channel, kind, bits, shift });
            shift += bits;
        }
        Some(ColorFormat { fields, bits: shift })
    }

    pub fn bytes_per_pixel(&self) -> usize {
        self.bits.div_ceil(8) as usize
    }

    /// the color a pixel's bytes hold, in RGBA order, with what a format
    /// leaves out as 0 for color and 1 for alpha.
    pub fn decode(&self, bytes: &[u8]) -> [f32; 4] {
        let mut raw = [0u8; 16];
        raw[..bytes.len()].copy_from_slice(bytes);
        let value = u128::from_le_bytes(raw);
        let mut color = [0.0, 0.0, 0.0, 1.0];
        for field in &self.fields {
            let bits = ((value >> field.shift) & ((1u128 << field.bits) - 1)) as u32;
            color[field.channel] = match field.kind {
                Kind::Unorm => unorm(bits, field.bits),
                Kind::Snorm => snorm(sign_extend(bits, field.bits), field.bits),
                Kind::Float if field.bits == 16 => f16_to_f32(bits as u16),
                Kind::Float => f32::from_bits(bits),
                Kind::SmallFloat => small_to_f32(bits, field.bits),
            };
        }
        color
    }

    /// a color in RGBA order as a pixel's bytes.
    pub fn encode(&self, color: [f32; 4], bytes: &mut [u8]) {
        let mut value = 0u128;
        for field in &self.fields {
            let component = color[field.channel];
            let bits = match field.kind {
                Kind::Unorm => to_unorm(component, field.bits),
                Kind::Snorm => to_snorm(component, field.bits) as u32 & mask(field.bits),
                Kind::Float if field.bits == 16 => f32_to_f16(component) as u32,
                Kind::Float => component.to_bits(),
                Kind::SmallFloat => f32_to_small(component, field.bits),
            };
            value |= ((bits & mask(field.bits)) as u128) << field.shift;
        }
        let length = bytes.len();
        bytes.copy_from_slice(&value.to_le_bytes()[..length]);
    }
}

fn mask(bits: u32) -> u32 {
    if bits >= 32 {
        u32::MAX
    } else {
        (1 << bits) - 1
    }
}

fn sign_extend(value: u32, bits: u32) -> i32 {
    ((value << (32 - bits)) as i32) >> (32 - bits)
}

/// an unsigned float of 10 or 11 bits, a 5 bit exponent over the rest, as
/// a float.
fn small_to_f32(bits: u32, width: u32) -> f32 {
    let mantissa_bits = width - 5;
    let exponent = bits >> mantissa_bits;
    let mantissa = bits & ((1 << mantissa_bits) - 1);
    match exponent {
        0 => mantissa as f32 / (1 << mantissa_bits) as f32 * 2f32.powi(-14),
        31 if mantissa == 0 => f32::INFINITY,
        31 => f32::NAN,
        _ => (1.0 + mantissa as f32 / (1 << mantissa_bits) as f32) * 2f32.powi(exponent as i32 - 15),
    }
}

/// a float as an unsigned float of 10 or 11 bits, negatives as 0.
fn f32_to_small(value: f32, width: u32) -> u32 {
    let mantissa_bits = width - 5;
    if value.is_nan() {
        return (31 << mantissa_bits) | 1;
    }
    if value <= 0.0 {
        return 0;
    }
    // through a half float, which has the same exponent, losing the sign
    // and the low bits of the mantissa
    let half = f32_to_f16(value) as u32;
    if half >= 0x7C00 {
        return 31 << mantissa_bits;
    }
    let shift = 10 - mantissa_bits;
    let rounded = (half + (1 << (shift - 1))) >> shift;
    rounded.min((31 << mantissa_bits) - 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn round_trip(format: u32, bytes: &[u8]) -> Vec<u8> {
        let layout = ColorFormat::from_gxm(format).unwrap();
        assert_eq!(layout.bytes_per_pixel(), bytes.len());
        let mut out = vec![0; bytes.len()];
        layout.encode(layout.decode(bytes), &mut out);
        out
    }

    #[test]
    fn abgr_keeps_red_in_the_first_byte() {
        let layout = ColorFormat::from_gxm(0x0000_0000).unwrap();
        assert_eq!(layout.decode(&[255, 0, 0, 255]), [1.0, 0.0, 0.0, 1.0]);
        let argb = ColorFormat::from_gxm(0x0010_0000).unwrap();
        assert_eq!(argb.decode(&[255, 0, 0, 255]), [0.0, 0.0, 1.0, 1.0]);
        let mut bytes = [0; 4];
        layout.encode([0.0, 0.5, 1.0, 0.0], &mut bytes);
        assert_eq!(bytes, [0, 128, 255, 0]);
    }

    #[test]
    fn packed_formats_put_each_channel_where_its_name_says() {
        // 565 with blue on top: pure blue is 0xF800
        let bgr = ColorFormat::from_gxm(0x3000_0000).unwrap();
        assert_eq!(bgr.decode(&0xF800u16.to_le_bytes()), [0.0, 0.0, 1.0, 1.0]);
        // 5551 RGBA keeps its one alpha bit at the bottom
        let rgba = ColorFormat::from_gxm(0x4020_0000).unwrap();
        assert_eq!(rgba.decode(&0x0001u16.to_le_bytes()), [0.0, 0.0, 0.0, 1.0]);
        assert_eq!(rgba.decode(&0xF800u16.to_le_bytes()), [1.0, 0.0, 0.0, 0.0]);
        // U8_A is alpha alone
        let alpha = ColorFormat::from_gxm(0x8090_0000).unwrap();
        assert_eq!(alpha.decode(&[255]), [0.0, 0.0, 0.0, 1.0]);
        assert_eq!(alpha.fields[0].channel, A);
    }

    #[test]
    fn every_format_survives_a_round_trip() {
        for (format, bytes) in [
            (0x0000_0000u32, vec![1u8, 2, 3, 4]),
            (0x1000_0000, vec![9, 8, 7]),
            (0x5020_0000, vec![0x34, 0x12]),
            (0x0100_0000, vec![0x00, 0x3C, 0x00, 0xC0, 0x00, 0x38, 0x00, 0x00]),
            (0x1100_0000, vec![0, 0, 128, 63, 0, 0, 0, 64]),
            (0x6080_0000, vec![0xFF, 0x03, 0x00, 0xC0]),
            (0xE080_0000, vec![0x7F, 0x81, 0x00, 0x40]),
            (0x5080_0000, vec![0x34, 0x12, 0xCD, 0xAB]),
        ] {
            assert_eq!(round_trip(format, &bytes), bytes, "format 0x{format:08X}");
        }
    }

    #[test]
    fn small_floats_hold_what_they_can() {
        let layout = ColorFormat::from_gxm(0x2100_0000).unwrap();
        let mut bytes = [0; 4];
        layout.encode([1.0, 0.5, 2.0, 1.0], &mut bytes);
        assert_eq!(layout.decode(&bytes), [1.0, 0.5, 2.0, 1.0]);
    }
}
