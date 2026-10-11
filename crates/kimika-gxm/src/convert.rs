//! the number formats the GPU moves between: IEEE half floats, and
//! integers read as fractions, unsigned from 0 to 1 and signed from -1 to
//! 1.

/// a half float's bits as a float.
pub fn f16_to_f32(bits: u16) -> f32 {
    let sign = ((bits >> 15) as u32) << 31;
    let exponent = ((bits >> 10) & 0x1F) as u32;
    let mantissa = (bits & 0x3FF) as u32;
    let magnitude = match exponent {
        0 if mantissa == 0 => 0,
        // subnormal: normalize it
        0 => {
            let shift = mantissa.leading_zeros() - 21;
            let mantissa = (mantissa << shift) & 0x3FF;
            (127 - 15 + 1 - shift) << 23 | mantissa << 13
        }
        0x1F => 0xFF << 23 | mantissa << 13,
        _ => (exponent + 127 - 15) << 23 | mantissa << 13,
    };
    f32::from_bits(sign | magnitude)
}

/// a float as a half float's bits, rounded to the nearest, ties to even.
pub fn f32_to_f16(value: f32) -> u16 {
    let bits = value.to_bits();
    let sign = ((bits >> 16) & 0x8000) as u16;
    let exponent = ((bits >> 23) & 0xFF) as i32;
    let mantissa = bits & 0x7F_FFFF;
    if exponent == 0xFF {
        let nan = if mantissa != 0 { 0x200 | (mantissa >> 13) as u16 } else { 0 };
        return sign | 0x7C00 | nan;
    }
    let half_exponent = exponent - 127 + 15;
    if half_exponent >= 0x1F {
        return sign | 0x7C00;
    }
    if half_exponent <= 0 {
        // subnormal or zero
        if half_exponent < -10 {
            return sign;
        }
        let full = mantissa | 0x80_0000;
        let shift = (14 - half_exponent) as u32;
        let rounded = round_shift(full, shift);
        return sign | rounded as u16;
    }
    let rounded = round_shift(mantissa, 13);
    // a carry out of the mantissa moves into the exponent, as it should
    sign | (((half_exponent as u32) << 10) + rounded) as u16
}

/// value shifted right, rounded to nearest with ties to even.
fn round_shift(value: u32, shift: u32) -> u32 {
    let kept = value >> shift;
    let rest = value & ((1 << shift) - 1);
    let half = 1 << (shift - 1);
    if rest > half || (rest == half && kept & 1 == 1) {
        kept + 1
    } else {
        kept
    }
}

/// an unsigned integer of bits bits as a fraction from 0 to 1.
pub fn unorm(value: u32, bits: u32) -> f32 {
    value as f32 / ((1u64 << bits) - 1) as f32
}

/// a signed integer of bits bits as a fraction from -1 to 1, its most
/// negative value as -1 too.
pub fn snorm(value: i32, bits: u32) -> f32 {
    (value as f32 / ((1i64 << (bits - 1)) - 1) as f32).max(-1.0)
}

/// a fraction from 0 to 1 as an unsigned integer of bits bits, clamped and
/// rounded.
pub fn to_unorm(value: f32, bits: u32) -> u32 {
    let max = ((1u64 << bits) - 1) as f32;
    let value = if value.is_nan() { 0.0 } else { value.clamp(0.0, 1.0) };
    (value * max + 0.5) as u32
}

/// a fraction from -1 to 1 as a signed integer of bits bits, clamped and
/// rounded.
pub fn to_snorm(value: f32, bits: u32) -> i32 {
    let max = ((1i64 << (bits - 1)) - 1) as f32;
    let value = if value.is_nan() { 0.0 } else { value.clamp(-1.0, 1.0) };
    (value * max).round() as i32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn half_floats_convert_both_ways() {
        for (bits, value) in [
            (0x0000, 0.0),
            (0x3C00, 1.0),
            (0xC000, -2.0),
            (0x3555, 0.333_251_95),
            (0x7BFF, 65504.0),
            (0x0001, 5.960_464_5e-8),
            (0x0400, 6.103_515_6e-5),
        ] {
            assert_eq!(f16_to_f32(bits), value, "{bits:04X}");
            assert_eq!(f32_to_f16(value), bits, "{value}");
        }
        assert!(f16_to_f32(0x7E00).is_nan());
        assert_eq!(f16_to_f32(0xFC00), f32::NEG_INFINITY);
        assert_eq!(f32_to_f16(1e6), 0x7C00);
        assert_eq!(f32_to_f16(f32::NAN) & 0x7C00, 0x7C00);
        // halfway between two halves goes to the even one
        assert_eq!(f32_to_f16(1.0 + 1.0 / 2048.0), 0x3C00);
        assert_eq!(f32_to_f16(1.0 + 3.0 / 2048.0), 0x3C02);
        // every half survives the round trip
        for bits in 0..=0xFFFFu16 {
            let value = f16_to_f32(bits);
            if !value.is_nan() {
                assert_eq!(f32_to_f16(value), bits);
            }
        }
    }

    #[test]
    fn normalized_integers_map_to_their_ranges() {
        assert_eq!(unorm(255, 8), 1.0);
        assert_eq!(unorm(0, 8), 0.0);
        assert_eq!(snorm(-128, 8), -1.0);
        assert_eq!(snorm(127, 8), 1.0);
        assert_eq!(to_unorm(0.5, 8), 128);
        assert_eq!(to_unorm(2.0, 8), 255);
        assert_eq!(to_snorm(-1.0, 16), -32767);
        assert_eq!(to_unorm(f32::NAN, 8), 0);
    }
}
