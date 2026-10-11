//! bitfield helpers.

/// bits hi..=lo of value, both ends included.
#[inline(always)]
pub const fn bits(value: u32, lo: u32, hi: u32) -> u32 {
    (value >> lo) & ((1u32 << (hi - lo + 1)).wrapping_sub(1))
}

/// one bit of value.
#[inline(always)]
pub const fn bit(value: u32, n: u32) -> bool {
    (value >> n) & 1 != 0
}

/// the low n bits of value, sign-extended.
#[inline(always)]
pub const fn sign_extend(value: u32, n: u32) -> i32 {
    let shift = 32 - n;
    ((value << shift) as i32) >> shift
}
