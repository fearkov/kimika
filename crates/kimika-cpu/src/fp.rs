//! floating point the way ARM does it, on top of the host's: the host
//! computes, rounding to nearest, and this makes the result ARM's. inputs
//! and outputs flush to zero under FZ, a NaN out of numbers is ARM's
//! default NaN rather than x86's, and a NaN in comes out the way ARM picks
//! it, the first signaling one quieted, else the first quiet one. NEON
//! always works this way with FZ and default NaN on, whatever FPSCR says.
//!
//! FPSCR's other rounding modes start from the host's result too: how the
//! exact result compares with a rounded sum, product, quotient or root can
//! be worked out exactly, in double precision for singles and with a fused
//! multiply-add for doubles, and it says whether the mode wants the next
//! number up or down instead. that fails only for doubles whose error is
//! too small for a double to hold, results just above the subnormals.
//!
//! a result is flushed when it comes out subnormal, after rounding. ARM
//! looks before rounding, so a result just under the smallest normal number
//! that rounds up to it flushes there and does not here, which is the one
//! place this differs from the hardware. recompiled code does the same as
//! this, so the two always agree.

use std::cmp::Ordering;

pub const FZ: u32 = 1 << 24;
pub const DN: u32 = 1 << 25;
/// NEON's cumulative saturation flag.
pub const QC: u32 = 1 << 27;

/// what NEON works with whatever FPSCR holds: flush to zero, default NaN,
/// round to nearest.
pub const STANDARD: u32 = FZ | DN;

pub const DEFAULT_NAN_32: u32 = 0x7FC0_0000;
pub const DEFAULT_NAN_64: u64 = 0x7FF8_0000_0000_0000;

/// the rounding mode in FPSCR's bits 23:22: nearest, toward plus infinity,
/// toward minus infinity, toward zero.
pub fn rounding(fpscr: u32) -> u32 {
    (fpscr >> 22) & 3
}

/// the operations both sizes have, so that one generic body serves both.
pub trait Float:
    Copy
    + PartialEq
    + PartialOrd
    + std::ops::Add<Output = Self>
    + std::ops::Sub<Output = Self>
    + std::ops::Mul<Output = Self>
    + std::ops::Div<Output = Self>
    + std::ops::Neg<Output = Self>
{
    type Bits: Copy + PartialEq;
    const DEFAULT_NAN: Self::Bits;
    const ZERO: Self;
    fn from_bits(bits: Self::Bits) -> Self;
    fn to_bits(self) -> Self::Bits;
    fn is_nan(self) -> bool;
    fn is_infinite(self) -> bool;
    fn is_sign_negative(self) -> bool;
    fn sqrt(self) -> Self;
    fn is_signaling(bits: Self::Bits) -> bool;
    fn quiet(bits: Self::Bits) -> Self::Bits;
    fn is_subnormal_bits(bits: Self::Bits) -> bool;
    fn signed_zero(bits: Self::Bits) -> Self::Bits;
    /// the largest finite number or the infinity, by sign.
    fn largest(negative: bool) -> Self;
    fn infinity(negative: bool) -> Self;
    /// the next number up, toward plus infinity, or down.
    fn step(self, up: bool) -> Self;
    /// how the exact a + b compares with the rounded sum, and likewise for
    /// the product, the quotient and the square root.
    fn sum_error(a: Self, b: Self, sum: Self) -> Ordering;
    fn product_error(a: Self, b: Self, product: Self) -> Ordering;
    fn quotient_error(a: Self, b: Self, quotient: Self) -> Ordering;
    fn root_error(a: Self, root: Self) -> Ordering;
}

fn sign_of<F: PartialOrd + Default>(value: F) -> Ordering {
    value.partial_cmp(&F::default()).unwrap_or(Ordering::Equal)
}

/// TwoSum, the error of a rounded sum, exact whatever the operands.
macro_rules! sum_error {
    ($a:expr, $b:expr, $sum:expr) => {{
        let (a, b, sum) = ($a, $b, $sum);
        let b_part = sum - a;
        sign_of((a - (sum - b_part)) + (b - b_part))
    }};
}

macro_rules! steps {
    ($float:ty, $sign:expr) => {
        fn step(self, up: bool) -> $float {
            if self.is_nan() || (self.is_infinite() && (self > 0.0) == up) {
                return self;
            }
            if self == 0.0 {
                return <$float>::from_bits(if up { 1 } else { $sign | 1 });
            }
            let bits = self.to_bits();
            <$float>::from_bits(if (self > 0.0) == up { bits + 1 } else { bits - 1 })
        }
    };
}

impl Float for f32 {
    type Bits = u32;
    const DEFAULT_NAN: u32 = DEFAULT_NAN_32;
    const ZERO: f32 = 0.0;
    fn from_bits(bits: u32) -> f32 {
        f32::from_bits(bits)
    }
    fn to_bits(self) -> u32 {
        f32::to_bits(self)
    }
    fn is_nan(self) -> bool {
        f32::is_nan(self)
    }
    fn is_infinite(self) -> bool {
        f32::is_infinite(self)
    }
    fn is_sign_negative(self) -> bool {
        f32::is_sign_negative(self)
    }
    fn sqrt(self) -> f32 {
        f32::sqrt(self)
    }
    fn is_signaling(bits: u32) -> bool {
        bits & 0x7F80_0000 == 0x7F80_0000 && bits & 0x007F_FFFF != 0 && bits & 0x0040_0000 == 0
    }
    fn quiet(bits: u32) -> u32 {
        bits | 0x0040_0000
    }
    fn is_subnormal_bits(bits: u32) -> bool {
        bits & 0x7F80_0000 == 0 && bits & 0x007F_FFFF != 0
    }
    fn signed_zero(bits: u32) -> u32 {
        bits & 0x8000_0000
    }
    fn largest(negative: bool) -> f32 {
        if negative {
            -f32::MAX
        } else {
            f32::MAX
        }
    }
    fn infinity(negative: bool) -> f32 {
        if negative {
            f32::NEG_INFINITY
        } else {
            f32::INFINITY
        }
    }
    steps!(f32, 0x8000_0000);
    fn sum_error(a: f32, b: f32, sum: f32) -> Ordering {
        sum_error!(a, b, sum)
    }
    // a product of singles is exact as a double, and so are the remainders
    // below, whose terms cancel all but a few bits
    fn product_error(a: f32, b: f32, product: f32) -> Ordering {
        sign_of(a as f64 * b as f64 - product as f64)
    }
    fn quotient_error(a: f32, b: f32, quotient: f32) -> Ordering {
        let remainder = sign_of(a as f64 - quotient as f64 * b as f64);
        if b < 0.0 {
            remainder.reverse()
        } else {
            remainder
        }
    }
    fn root_error(a: f32, root: f32) -> Ordering {
        sign_of(a as f64 - root as f64 * root as f64)
    }
}

impl Float for f64 {
    type Bits = u64;
    const DEFAULT_NAN: u64 = DEFAULT_NAN_64;
    const ZERO: f64 = 0.0;
    fn from_bits(bits: u64) -> f64 {
        f64::from_bits(bits)
    }
    fn to_bits(self) -> u64 {
        f64::to_bits(self)
    }
    fn is_nan(self) -> bool {
        f64::is_nan(self)
    }
    fn is_infinite(self) -> bool {
        f64::is_infinite(self)
    }
    fn is_sign_negative(self) -> bool {
        f64::is_sign_negative(self)
    }
    fn sqrt(self) -> f64 {
        f64::sqrt(self)
    }
    fn is_signaling(bits: u64) -> bool {
        bits & 0x7FF0_0000_0000_0000 == 0x7FF0_0000_0000_0000
            && bits & 0x000F_FFFF_FFFF_FFFF != 0
            && bits & 0x0008_0000_0000_0000 == 0
    }
    fn quiet(bits: u64) -> u64 {
        bits | 0x0008_0000_0000_0000
    }
    fn is_subnormal_bits(bits: u64) -> bool {
        bits & 0x7FF0_0000_0000_0000 == 0 && bits & 0x000F_FFFF_FFFF_FFFF != 0
    }
    fn signed_zero(bits: u64) -> u64 {
        bits & 0x8000_0000_0000_0000
    }
    fn largest(negative: bool) -> f64 {
        if negative {
            -f64::MAX
        } else {
            f64::MAX
        }
    }
    fn infinity(negative: bool) -> f64 {
        if negative {
            f64::NEG_INFINITY
        } else {
            f64::INFINITY
        }
    }
    steps!(f64, 0x8000_0000_0000_0000);
    fn sum_error(a: f64, b: f64, sum: f64) -> Ordering {
        sum_error!(a, b, sum)
    }
    // with a fused multiply-add the error comes out exact, so long as it
    // is not below the subnormals; where it could be, the operands are
    // scaled by a power of two first, which changes no comparison
    fn product_error(a: f64, b: f64, product: f64) -> Ordering {
        if product == 0.0 {
            // the whole product underflowed, and it is a's sign times b's
            return if a == 0.0 || b == 0.0 {
                Ordering::Equal
            } else if (a < 0.0) != (b < 0.0) {
                Ordering::Less
            } else {
                Ordering::Greater
            };
        }
        if exponent(product) > -900 {
            return sign_of(a.mul_add(b, -product));
        }
        let (small, large) = if a.abs() <= b.abs() { (a, b) } else { (b, a) };
        sign_of(scaled(small, 600).mul_add(large, -scaled(product, 600)))
    }
    fn quotient_error(a: f64, b: f64, quotient: f64) -> Ordering {
        let k = (900 - exponent(a).max(exponent(b))).max(0);
        let (a, b) = (scaled(a, k), scaled(b, k));
        let remainder = sign_of((-quotient).mul_add(b, a));
        if b < 0.0 {
            remainder.reverse()
        } else {
            remainder
        }
    }
    fn root_error(a: f64, root: f64) -> Ordering {
        let k = if exponent(a) < -900 { 300 } else { 0 };
        sign_of((-scaled(root, k)).mul_add(scaled(root, k), scaled(a, 2 * k)))
    }
}

/// the power of two a double's magnitude is at least, its exponent, for
/// subnormals too, and far below them for zero.
fn exponent(x: f64) -> i32 {
    let bits = x.to_bits() & !(1 << 63);
    match bits >> 52 {
        0 if bits == 0 => -2000,
        0 => 63 - bits.leading_zeros() as i32 - 1074,
        biased => biased as i32 - 1023,
    }
}

/// x times 2^k, in two steps for a k past what one power of two holds.
fn scaled(x: f64, k: i32) -> f64 {
    let half = k / 2;
    x * 2f64.powi(half) * 2f64.powi(k - half)
}

/// an input as an operation sees it, a subnormal made a signed zero under
/// FZ.
#[inline(always)]
pub fn input<F: Float>(fpscr: u32, bits: F::Bits) -> F {
    if fpscr & FZ != 0 && F::is_subnormal_bits(bits) {
        F::from_bits(F::signed_zero(bits))
    } else {
        F::from_bits(bits)
    }
}

/// the NaN an operation on these operands gives, if any is a NaN: under DN
/// the default one, else the first signaling one quieted, else the first
/// quiet one.
#[inline]
pub fn nan_operand<F: Float>(fpscr: u32, operands: &[F::Bits]) -> Option<F::Bits> {
    if !operands.iter().any(|&bits| F::from_bits(bits).is_nan()) {
        return None;
    }
    if fpscr & DN != 0 {
        return Some(F::DEFAULT_NAN);
    }
    let signaling = operands.iter().find(|&&bits| F::is_signaling(bits));
    let chosen = signaling.or_else(|| operands.iter().find(|&&bits| F::from_bits(bits).is_nan())).copied()?;
    Some(F::quiet(chosen))
}

/// a result the host computed made ARM's: a NaN out of numbers becomes the
/// default NaN, a subnormal under FZ a signed zero.
#[inline(always)]
pub fn output<F: Float>(fpscr: u32, value: F) -> F::Bits {
    if value.is_nan() {
        return F::DEFAULT_NAN;
    }
    let bits = value.to_bits();
    if fpscr & FZ != 0 && F::is_subnormal_bits(bits) {
        F::signed_zero(bits)
    } else {
        bits
    }
}

/// a result rounded to nearest made the one a mode asks for, given how the
/// exact result compares with it: toward plus infinity it steps up if the
/// exact one is above, toward minus infinity down if below, and toward zero
/// back from whichever way it was rounded away.
fn directed<F: Float>(mode: u32, rounded: F, error: Ordering) -> F {
    let up = match (mode, error) {
        (1, Ordering::Greater) => true,
        (2, Ordering::Less) => false,
        (3, Ordering::Less) if rounded > F::ZERO => false,
        (3, Ordering::Greater) if rounded < F::ZERO => true,
        _ => return rounded,
    };
    rounded.step(up)
}

/// what an overflow gives in a mode: the infinity, unless the mode rounds
/// toward zero from that side, where it is the largest number.
fn overflow<F: Float>(mode: u32, negative: bool) -> F {
    match (mode, negative) {
        (0, _) | (1, false) | (2, true) => F::infinity(negative),
        _ => F::largest(negative),
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Operation {
    Add,
    Subtract,
    Multiply,
    Divide,
}

/// an operation of two operands, ARM's way, in FPSCR's rounding mode.
#[inline]
pub fn arithmetic<F: Float>(fpscr: u32, operation: Operation, a: F::Bits, b: F::Bits) -> F::Bits {
    if let Some(nan) = nan_operand::<F>(fpscr, &[a, b]) {
        return nan;
    }
    let (x, y) = (input::<F>(fpscr, a), input::<F>(fpscr, b));
    let y = if operation == Operation::Subtract { -y } else { y };
    let result = match operation {
        Operation::Add | Operation::Subtract => x + y,
        Operation::Multiply => x * y,
        Operation::Divide => x / y,
    };
    let mode = rounding(fpscr);
    if mode == 0 || result.is_nan() {
        return output(fpscr, result);
    }
    let result = if result.is_infinite() {
        // an infinity out of finite numbers is an overflow, but for a
        // division by zero
        let exact = x.is_infinite() || y.is_infinite() || (operation == Operation::Divide && y == F::ZERO);
        if exact {
            result
        } else {
            overflow(mode, result.is_sign_negative())
        }
    } else {
        match operation {
            // a sum that is exactly zero is +0 in every mode but toward minus
            // infinity, where it is -0 unless both operands were +0
            Operation::Add | Operation::Subtract if result == F::ZERO => {
                let both_positive = x == F::ZERO && y == F::ZERO && !x.is_sign_negative() && !y.is_sign_negative();
                if mode == 2 && !both_positive {
                    -F::ZERO
                } else {
                    result
                }
            }
            Operation::Add | Operation::Subtract => directed(mode, result, F::sum_error(x, y, result)),
            Operation::Multiply => directed(mode, result, F::product_error(x, y, result)),
            Operation::Divide => directed(mode, result, F::quotient_error(x, y, result)),
        }
    };
    output(fpscr, result)
}

/// VSQRT, in FPSCR's rounding mode.
pub fn square_root<F: Float>(fpscr: u32, a: F::Bits) -> F::Bits {
    if let Some(nan) = nan_operand::<F>(fpscr, &[a]) {
        return nan;
    }
    let x = input::<F>(fpscr, a);
    let result = x.sqrt();
    let mode = rounding(fpscr);
    if mode == 0 || result.is_nan() || result.is_infinite() || result == F::ZERO {
        return output(fpscr, result);
    }
    output(fpscr, directed(mode, result, F::root_error(x, result)))
}

/// a double made single in FPSCR's rounding mode, as VCVT does, and the
/// conversions of integers and fixed point to single precision after them,
/// whose values are exact as doubles.
pub fn narrow(fpscr: u32, value: f64) -> u32 {
    let rounded = value as f32;
    let mode = rounding(fpscr);
    let result = if mode == 0 || rounded.is_nan() || value.is_infinite() {
        rounded
    } else if rounded.is_infinite() {
        overflow(mode, value < 0.0)
    } else {
        directed(mode, rounded, sign_of(value - rounded as f64))
    };
    output::<f32>(fpscr, result)
}

/// VCMP's flags, NZCV, for a and b: unordered, equal, less or greater.
pub fn compare<F: Float>(fpscr: u32, a: F::Bits, b: F::Bits) -> u32 {
    let (x, y) = (input::<F>(fpscr, a), input::<F>(fpscr, b));
    if x.is_nan() || y.is_nan() {
        0b0011
    } else if x == y {
        0b0110
    } else if x < y {
        0b1000
    } else {
        0b0010
    }
}

/// a value rounded to an integer in a mode, still as a float.
pub fn round(value: f64, mode: u32) -> f64 {
    match mode {
        0 => {
            // to nearest, ties to even
            let rounded = value.round();
            if (value - value.trunc()).abs() == 0.5 && rounded % 2.0 != 0.0 {
                rounded - value.signum()
            } else {
                rounded
            }
        }
        1 => value.ceil(),
        2 => value.floor(),
        _ => value.trunc(),
    }
}

/// a float to a 32-bit integer, rounded in a mode and saturated, NaN
/// giving zero.
pub fn to_integer(value: f64, mode: u32, signed: bool) -> u32 {
    if value.is_nan() {
        return 0;
    }
    let rounded = round(value, mode);
    if signed {
        rounded.clamp(i32::MIN as f64, i32::MAX as f64) as i32 as u32
    } else {
        rounded.clamp(0.0, u32::MAX as f64) as u32
    }
}

/// a float to a fixed-point number of size bits with fraction bits after
/// the point, rounded toward zero and saturated, as VCVT to fixed does.
pub fn to_fixed(value: f64, fraction: u32, size: u32, signed: bool) -> u32 {
    if value.is_nan() {
        return 0;
    }
    let scaled = (value * 2f64.powi(fraction as i32)).trunc();
    let (min, max) = match (signed, size) {
        (true, 16) => (i16::MIN as f64, i16::MAX as f64),
        (true, _) => (i32::MIN as f64, i32::MAX as f64),
        (false, 16) => (0.0, u16::MAX as f64),
        (false, _) => (0.0, u32::MAX as f64),
    };
    let clamped = scaled.clamp(min, max);
    if signed {
        clamped as i64 as u32 & if size == 16 { 0xFFFF } else { u32::MAX }
    } else {
        clamped as u32
    }
}

/// a fixed-point number of size bits to a float, as VCVT from fixed does.
pub fn from_fixed(bits: u32, fraction: u32, size: u32, signed: bool) -> f64 {
    let integer = match (signed, size) {
        (true, 16) => bits as u16 as i16 as f64,
        (true, _) => bits as i32 as f64,
        (false, 16) => (bits & 0xFFFF) as f64,
        (false, _) => bits as f64,
    };
    integer / 2f64.powi(fraction as i32)
}

/// a half precision number to single precision, as VCVTB and VCVTT do with
/// IEEE halves, which are what FPSCR.AHP clear asks for. FZ never touches
/// halves.
pub fn half_to_single(fpscr: u32, half: u16) -> u32 {
    let sign = (half as u32 & 0x8000) << 16;
    let exponent = (half >> 10) & 0x1F;
    let mantissa = (half & 0x3FF) as u32;
    match exponent {
        0 if mantissa == 0 => sign,
        0 => {
            // a subnormal half is a normal single
            let shift = mantissa.leading_zeros() - 21;
            sign | (113 - shift) << 23 | ((mantissa << shift) & 0x3FF) << 13
        }
        0x1F if mantissa == 0 => sign | 0x7F80_0000,
        0x1F if fpscr & DN != 0 => DEFAULT_NAN_32,
        0x1F => sign | 0x7FC0_0000 | mantissa << 13,
        _ => sign | (exponent as u32 + 127 - 15) << 23 | mantissa << 13,
    }
}

/// value shifted right by shift bits, rounded in a mode, nearest or toward
/// plus or minus infinity or zero, for a number of the given sign.
fn shift_rounded(value: u64, shift: u32, mode: u32, negative: bool) -> u64 {
    if shift == 0 {
        return value;
    }
    let (kept, rest) = if shift >= 64 { (0, value) } else { (value >> shift, value & ((1 << shift) - 1)) };
    let up = match mode {
        0 => {
            let half = if shift > 64 { u64::MAX } else { 1u64 << (shift - 1) };
            rest > half || (rest == half && kept & 1 == 1)
        }
        1 => rest != 0 && !negative,
        2 => rest != 0 && negative,
        _ => false,
    };
    kept + up as u64
}

/// a single precision number to half precision, rounded in FPSCR's mode.
pub fn single_to_half(fpscr: u32, single: u32) -> u16 {
    let bits = input::<f32>(fpscr, single).to_bits();
    let sign = ((bits >> 16) & 0x8000) as u16;
    let negative = sign != 0;
    let exponent = (bits >> 23) & 0xFF;
    let mantissa = bits & 0x7F_FFFF;
    if exponent == 0xFF {
        return match mantissa {
            0 => sign | 0x7C00,
            _ if fpscr & DN != 0 => 0x7E00,
            _ => sign | 0x7E00 | (mantissa >> 13) as u16,
        };
    }
    if exponent == 0 && mantissa == 0 {
        return sign;
    }
    let mode = rounding(fpscr);
    // the half's exponent, and the significand with its leading one, cut
    // from 24 bits to 11, or fewer where the half is subnormal
    let half_exponent = exponent as i32 - 127 + 15;
    let significand = if exponent == 0 { mantissa } else { mantissa | 0x80_0000 } as u64;
    let shift = if half_exponent >= 1 { 13 } else { (14 - half_exponent) as u32 };
    let rounded = shift_rounded(significand, shift, mode, negative);
    // adding the significand, leading one and all, to the exponent less one
    // carries into the exponent when rounding overflows the mantissa
    let half = if half_exponent >= 1 { ((half_exponent as u64 - 1) << 10) + rounded } else { rounded };
    if half >= 0x7C00 {
        let to_infinity = mode == 0 || (mode == 1 && !negative) || (mode == 2 && negative);
        return sign | if to_infinity { 0x7C00 } else { 0x7BFF };
    }
    sign | half as u16
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nans_come_out_the_way_arm_picks_them() {
        let quiet = 0x7FC0_1234;
        let signaling = 0x7F80_0001;
        assert_eq!(nan_operand::<f32>(0, &[quiet, signaling]), Some(0x7FC0_0001));
        assert_eq!(nan_operand::<f32>(0, &[quiet, 0x3F80_0000]), Some(quiet));
        assert_eq!(nan_operand::<f32>(DN, &[quiet]), Some(DEFAULT_NAN_32));
        // a NaN out of numbers is ARM's positive one, not the host's
        assert_eq!(arithmetic::<f32>(0, Operation::Divide, 0, 0), DEFAULT_NAN_32);
    }

    #[test]
    fn flush_to_zero_flushes_inputs_and_outputs() {
        let tiny = 0x0000_0001;
        assert_eq!(arithmetic::<f32>(FZ, Operation::Multiply, tiny, 0x5F80_0000), 0);
        assert_eq!(
            arithmetic::<f32>(0, Operation::Multiply, tiny, 0x5F80_0000),
            (f32::from_bits(1) * 2f32.powi(64)).to_bits()
        );
        assert_eq!(arithmetic::<f32>(FZ, Operation::Multiply, 0x0080_0000, 0x3F00_0000), 0);
        assert_eq!(arithmetic::<f32>(FZ, Operation::Multiply, 0x8080_0000, 0x3F00_0000), 0x8000_0000);
    }

    #[test]
    fn halves_round_trip() {
        for half in [0u16, 0x3C00, 0xC000, 0x7BFF, 0x0001, 0x03FF, 0x0400, 0x7C00, 0xFC00, 0x3555] {
            assert_eq!(single_to_half(0, half_to_single(0, half)), half, "{half:04X}");
        }
        // 1/3 rounds to nearest
        assert_eq!(single_to_half(0, (1.0f32 / 3.0).to_bits()), 0x3555);
        assert_eq!(single_to_half(0, 70000f32.to_bits()), 0x7C00);
        assert_eq!(single_to_half(3 << 22, 70000f32.to_bits()), 0x7BFF);
    }

    #[test]
    fn fixed_point_saturates_and_truncates() {
        assert_eq!(to_fixed(1.5, 1, 32, true), 3);
        assert_eq!(to_fixed(-1.75, 1, 32, true), (-3i32) as u32);
        assert_eq!(to_fixed(1e10, 0, 16, true), 0x7FFF);
        assert_eq!(to_fixed(-1.0, 4, 16, false), 0);
        assert_eq!(from_fixed(0xFFFF, 8, 16, true), -1.0 / 256.0);
    }
}
