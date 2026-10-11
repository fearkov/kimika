//! the barrel shifter, shared by data processing and register-offset
//! addressing in both states, and the two ways the states encode
//! immediates.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShiftType {
    Lsl,
    Lsr,
    Asr,
    Ror,
}

impl ShiftType {
    #[inline(always)]
    pub fn from_bits(value: u32) -> ShiftType {
        match value & 3 {
            0 => ShiftType::Lsl,
            1 => ShiftType::Lsr,
            2 => ShiftType::Asr,
            _ => ShiftType::Ror,
        }
    }
}

/// a shift by an amount from an immediate field, where LSR and ASR #0 mean
/// 32 and ROR #0 means RRX.
#[inline(always)]
pub fn shift_by_immediate(ty: ShiftType, value: u32, amount: u32, carry_in: bool) -> (u32, bool) {
    match ty {
        ShiftType::Lsl => {
            if amount == 0 {
                (value, carry_in)
            } else {
                (value << amount, (value >> (32 - amount)) & 1 != 0)
            }
        }
        ShiftType::Lsr => {
            if amount == 0 {
                (0, value >> 31 != 0)
            } else {
                (value >> amount, (value >> (amount - 1)) & 1 != 0)
            }
        }
        ShiftType::Asr => {
            if amount == 0 {
                (((value as i32) >> 31) as u32, value >> 31 != 0)
            } else {
                (((value as i32) >> amount) as u32, ((value as i32) >> (amount - 1)) & 1 != 0)
            }
        }
        ShiftType::Ror => {
            if amount == 0 {
                // a 33-bit rotate through the carry flag
                (((carry_in as u32) << 31) | (value >> 1), value & 1 != 0)
            } else {
                (value.rotate_right(amount), (value >> (amount - 1)) & 1 != 0)
            }
        }
    }
}

/// a shift by the bottom byte of a register.
#[inline(always)]
pub fn shift_by_register(ty: ShiftType, value: u32, amount: u32, carry_in: bool) -> (u32, bool) {
    let amount = amount & 0xFF;
    if amount == 0 {
        return (value, carry_in);
    }
    match ty {
        ShiftType::Lsl => match amount {
            1..=31 => (value << amount, (value >> (32 - amount)) & 1 != 0),
            32 => (0, value & 1 != 0),
            _ => (0, false),
        },
        ShiftType::Lsr => match amount {
            1..=31 => (value >> amount, (value >> (amount - 1)) & 1 != 0),
            32 => (0, value >> 31 != 0),
            _ => (0, false),
        },
        ShiftType::Asr => {
            if amount >= 32 {
                (((value as i32) >> 31) as u32, value >> 31 != 0)
            } else {
                (((value as i32) >> amount) as u32, ((value as i32) >> (amount - 1)) & 1 != 0)
            }
        }
        ShiftType::Ror => {
            let rotate = amount & 31;
            if rotate == 0 {
                // a multiple of 32 leaves the value, the carry is its top bit
                (value, value >> 31 != 0)
            } else {
                (value.rotate_right(rotate), (value >> (rotate - 1)) & 1 != 0)
            }
        }
    }
}

/// ARM's modified immediate, eight bits rotated right by twice four.
#[inline(always)]
pub fn arm_expand_immediate(imm12: u32, carry_in: bool) -> (u32, bool) {
    let imm = imm12 & 0xFF;
    let rotate = ((imm12 >> 8) & 0xF) * 2;
    if rotate == 0 {
        (imm, carry_in)
    } else {
        let value = imm.rotate_right(rotate);
        (value, value >> 31 != 0)
    }
}

/// Thumb's modified immediate: eight bits alone, repeated in some bytes of
/// the word, or with a top bit set and rotated to anywhere. none for the
/// repeating patterns of a zero byte, which are unpredictable.
#[inline(always)]
pub fn thumb_expand_immediate(imm12: u32, carry_in: bool) -> Option<(u32, bool)> {
    let imm8 = imm12 & 0xFF;
    if imm12 >> 10 == 0 {
        let value = match (imm12 >> 8) & 3 {
            0 => imm8,
            _ if imm8 == 0 => return None,
            1 => imm8 << 16 | imm8,
            2 => imm8 << 24 | imm8 << 8,
            _ => imm8 * 0x0101_0101,
        };
        Some((value, carry_in))
    } else {
        let value = (0x80 | (imm12 & 0x7F)).rotate_right(imm12 >> 7);
        Some((value, value >> 31 != 0))
    }
}
