//! VFPv3 with 32 double registers, and the doors to NEON, which shares
//! them. the encodings are ARM's, chapter A7, which Thumb's are word for
//! word with the condition always.
//!
//! moves and the sign operations copy bits and nothing else: games keep two
//! singles in a double, or integers in either, whose bits must survive. only
//! arithmetic goes through ARM's floating point rules, in fp.

use crate::bits::{bit, bits};
use crate::fp::{self, Float, Operation};
use crate::{Bus, Cpu, Exit, PC};

#[derive(Clone)]
pub struct Vfp {
    /// d0..d31 as 64 words, dN in regs[2N] (low) and regs[2N + 1], so that
    /// s0..s31 are regs[0..32] and qN is d(2N) and d(2N + 1).
    pub regs: [u32; 64],
    pub fpscr: u32,
}

impl Default for Vfp {
    fn default() -> Self {
        Self::new()
    }
}

impl Vfp {
    pub fn new() -> Vfp {
        Vfp { regs: [0; 64], fpscr: 0 }
    }

    #[inline(always)]
    pub fn s(&self, index: usize) -> u32 {
        self.regs[index]
    }

    #[inline(always)]
    pub fn set_s(&mut self, index: usize, bits: u32) {
        self.regs[index] = bits;
    }

    #[inline(always)]
    pub fn d(&self, index: usize) -> u64 {
        self.regs[2 * index] as u64 | (self.regs[2 * index + 1] as u64) << 32
    }

    #[inline(always)]
    pub fn set_d(&mut self, index: usize, bits: u64) {
        self.regs[2 * index] = bits as u32;
        self.regs[2 * index + 1] = (bits >> 32) as u32;
    }
}

/// a single register's number, four bits and the extra one below them.
#[inline(always)]
fn single(field: u32, extra: bool) -> usize {
    (field << 1 | extra as u32) as usize
}

/// a double register's number, the extra bit on top.
#[inline(always)]
fn double(field: u32, extra: bool) -> usize {
    ((extra as u32) << 4 | field) as usize
}

/// the operand registers of a data-processing instruction, d, n and m, in
/// whichever size.
fn operands(op: u32, wide: bool) -> (usize, usize, usize) {
    let (vd, vn, vm) = (bits(op, 12, 15), bits(op, 16, 19), op & 0xF);
    let (d, n, m) = (bit(op, 22), bit(op, 7), bit(op, 5));
    if wide {
        (double(vd, d), double(vn, n), double(vm, m))
    } else {
        (single(vd, d), single(vn, n), single(vm, m))
    }
}

/// VFPv3's short vectors, which FPSCR's LEN and STRIDE turn a scalar
/// instruction into, wrapping around in banks of eight singles or four
/// doubles. a destination in the first bank is always a scalar, and so is
/// an m operand there.
struct Vector {
    length: usize,
    stride: usize,
    bank: usize,
}

impl Vector {
    fn new(fpscr: u32, wide: bool, destination: usize) -> Vector {
        let bank = if wide { 4 } else { 8 };
        let length = if destination < bank { 1 } else { bits(fpscr, 16, 18) as usize + 1 };
        Vector { length, stride: if bits(fpscr, 20, 21) == 0b11 { 2 } else { 1 }, bank }
    }

    fn step(&self, register: usize, i: usize) -> usize {
        (register & !(self.bank - 1)) | ((register + i * self.stride) & (self.bank - 1))
    }

    /// d, n and m of each step.
    fn registers(&self, d: usize, n: usize, m: usize) -> impl Iterator<Item = (usize, usize, usize)> + '_ {
        (0..self.length)
            .map(move |i| (self.step(d, i), self.step(n, i), if m < self.bank { m } else { self.step(m, i) }))
    }
}

/// the arithmetic of two operands and the accumulating forms.
#[derive(Clone, Copy)]
enum Arithmetic {
    MultiplyAccumulate,
    MultiplySubtract,
    NegateMultiplyAccumulate,
    NegateMultiplySubtract,
    Multiply,
    NegateMultiply,
    Add,
    Subtract,
    Divide,
}

/// the sign bit of a float of either size.
trait Sign: Float {
    fn negate(bits: Self::Bits) -> Self::Bits;
}

impl Sign for f32 {
    fn negate(bits: u32) -> u32 {
        bits ^ 0x8000_0000
    }
}

impl Sign for f64 {
    fn negate(bits: u64) -> u64 {
        bits ^ 0x8000_0000_0000_0000
    }
}

/// one step of an arithmetic instruction: the product rounded first, then
/// added, as VMLA and the rest do without fusing, each step with ARM's NaN
/// rules and the negations on the bits.
fn arithmetic<F: Sign>(fpscr: u32, operation: Arithmetic, a: F::Bits, b: F::Bits, accumulator: F::Bits) -> F::Bits {
    let calculate = |operation, x, y| fp::arithmetic::<F>(fpscr, operation, x, y);
    let product = || calculate(Operation::Multiply, a, b);
    let add = |x, y| calculate(Operation::Add, x, y);
    match operation {
        Arithmetic::Multiply => product(),
        Arithmetic::NegateMultiply => F::negate(product()),
        Arithmetic::MultiplyAccumulate => add(accumulator, product()),
        Arithmetic::MultiplySubtract => add(accumulator, F::negate(product())),
        Arithmetic::NegateMultiplyAccumulate => add(F::negate(accumulator), F::negate(product())),
        Arithmetic::NegateMultiplySubtract => add(F::negate(accumulator), product()),
        Arithmetic::Add => add(a, b),
        Arithmetic::Subtract => calculate(Operation::Subtract, a, b),
        Arithmetic::Divide => calculate(Operation::Divide, a, b),
    }
}

/// A7.5, CDP on coprocessors 10 and 11.
pub fn data_processing(cpu: &mut Cpu, op: u32) -> Option<Exit> {
    let wide = bit(op, 8);
    let (d, n, m) = operands(op, wide);
    let operation = match (bits(op, 20, 23) & 0b1011, bit(op, 6)) {
        (0b0000, false) => Arithmetic::MultiplyAccumulate,
        (0b0000, true) => Arithmetic::MultiplySubtract,
        (0b0001, false) => Arithmetic::NegateMultiplySubtract,
        (0b0001, true) => Arithmetic::NegateMultiplyAccumulate,
        (0b0010, false) => Arithmetic::Multiply,
        (0b0010, true) => Arithmetic::NegateMultiply,
        (0b0011, false) => Arithmetic::Add,
        (0b0011, true) => Arithmetic::Subtract,
        (0b1000, false) => Arithmetic::Divide,
        (0b1011, _) => return other(cpu, op, wide, d, m),
        // VFMA and the other fused forms are VFPv4's
        _ => return cpu.undefined(op),
    };
    let fpscr = cpu.vfp.fpscr;
    for (d, n, m) in Vector::new(fpscr, wide, d).registers(d, n, m) {
        let vfp = &mut cpu.vfp;
        if wide {
            let result = arithmetic::<f64>(fpscr, operation, vfp.d(n), vfp.d(m), vfp.d(d));
            vfp.set_d(d, result);
        } else {
            let result = arithmetic::<f32>(fpscr, operation, vfp.s(n), vfp.s(m), vfp.s(d));
            vfp.set_s(d, result);
        }
    }
    None
}

/// VFPv3's VMOV with an immediate: a sign, three bits of exponent and four
/// of fraction, the exponent's top bit the inverse of the next, which
/// repeats.
fn expand_immediate(imm8: u32, wide: bool) -> u64 {
    let sign = (imm8 >> 7) as u64;
    let b = (imm8 >> 6) & 1;
    let low = ((imm8 >> 4) & 3) as u64;
    let fraction = (imm8 & 0xF) as u64;
    if wide {
        let repeated = if b == 1 { 0xFF } else { 0 };
        let exponent = ((b ^ 1) as u64) << 10 | repeated << 2 | low;
        sign << 63 | exponent << 52 | fraction << 48
    } else {
        let repeated = if b == 1 { 0x1F } else { 0 };
        let exponent = ((b ^ 1) as u64) << 7 | repeated << 2 | low;
        sign << 31 | exponent << 23 | fraction << 19
    }
}

/// the rest of A7.5: VMOV, VABS, VNEG, VSQRT, the comparisons and the
/// conversions.
fn other(cpu: &mut Cpu, op: u32, wide: bool, d: usize, m: usize) -> Option<Exit> {
    let opc2 = bits(op, 16, 19);
    let opc3 = bits(op, 6, 7);
    let fpscr = cpu.vfp.fpscr;
    if opc3 & 1 == 0 {
        // VMOV with an immediate, which also takes part in short vectors.
        // bits 7 and 5 are meant to be zero
        if op & 0xA0 != 0 {
            return cpu.undefined(op);
        }
        let imm8 = bits(op, 16, 19) << 4 | (op & 0xF);
        let value = expand_immediate(imm8, wide);
        for (d, _, _) in Vector::new(fpscr, wide, d).registers(d, d, d) {
            if wide {
                cpu.vfp.set_d(d, value);
            } else {
                cpu.vfp.set_s(d, value as u32);
            }
        }
        return None;
    }
    match (opc2, opc3) {
        // VMOV, VABS, VNEG and VSQRT, on vectors too
        (0b0000 | 0b0001, _) => {
            for (d, _, m) in Vector::new(fpscr, wide, d).registers(d, d, m) {
                let vfp = &mut cpu.vfp;
                if wide {
                    let value = vfp.d(m);
                    let result = match (opc2, opc3) {
                        (0, 0b01) => value,
                        (0, _) => value & !(1 << 63),
                        (_, 0b01) => value ^ 1 << 63,
                        _ => fp::square_root::<f64>(fpscr, value),
                    };
                    vfp.set_d(d, result);
                } else {
                    let value = vfp.s(m);
                    let result = match (opc2, opc3) {
                        (0, 0b01) => value,
                        (0, _) => value & 0x7FFF_FFFF,
                        (_, 0b01) => value ^ 0x8000_0000,
                        _ => fp::square_root::<f32>(fpscr, value),
                    };
                    vfp.set_s(d, result);
                }
            }
        }
        // VCVTB and VCVTT, between a single and the bottom or top half of
        // another
        (0b0010 | 0b0011, _) => {
            if wide {
                return cpu.undefined(op);
            }
            let top = bit(op, 7);
            let (d, m) = (single(bits(op, 12, 15), bit(op, 22)), single(op & 0xF, bit(op, 5)));
            if opc2 == 0b0010 {
                let half = (cpu.vfp.s(m) >> if top { 16 } else { 0 }) as u16;
                cpu.vfp.set_s(d, fp::half_to_single(fpscr, half));
            } else {
                let half = fp::single_to_half(fpscr, cpu.vfp.s(m)) as u32;
                let old = cpu.vfp.s(d);
                cpu.vfp.set_s(d, if top { (old & 0xFFFF) | half << 16 } else { (old & 0xFFFF_0000) | half });
            }
        }
        // VCMP and VCMPE, with a register or with zero
        (0b0100 | 0b0101, _) => {
            if opc2 == 0b0101 && op & 0x2F != 0 {
                return cpu.undefined(op);
            }
            let flags = if wide {
                let b = if opc2 == 0b0101 { 0 } else { cpu.vfp.d(m) };
                fp::compare::<f64>(fpscr, cpu.vfp.d(d), b)
            } else {
                let b = if opc2 == 0b0101 { 0 } else { cpu.vfp.s(m) };
                fp::compare::<f32>(fpscr, cpu.vfp.s(d), b)
            };
            cpu.vfp.fpscr = (fpscr & 0x0FFF_FFFF) | flags << 28;
        }
        // VCVT between single and double, the destination the other size
        (0b0111, 0b11) => {
            if wide {
                let d = single(bits(op, 12, 15), bit(op, 22));
                let value = cpu.vfp.d(m);
                let result = if f64::from_bits(value).is_nan() {
                    nan_narrowed(fpscr, value)
                } else {
                    fp::narrow(fpscr, fp::input::<f64>(fpscr, value))
                };
                cpu.vfp.set_s(d, result);
            } else {
                let d = double(bits(op, 12, 15), bit(op, 22));
                let value = cpu.vfp.s(m);
                let result = if f32::from_bits(value).is_nan() {
                    nan_widened(fpscr, value)
                } else {
                    fp::output::<f64>(fpscr, fp::input::<f32>(fpscr, value) as f64)
                };
                cpu.vfp.set_d(d, result);
            }
        }
        // VCVT from an integer in a single register
        (0b1000, _) => {
            let integer = cpu.vfp.s(single(op & 0xF, bit(op, 5)));
            let value = if bit(op, 7) { integer as i32 as f64 } else { integer as f64 };
            if wide {
                cpu.vfp.set_d(d, fp::output::<f64>(fpscr, value));
            } else {
                // through f64 an i32 is exact, and rounding it to f32 once
                // is right
                cpu.vfp.set_s(d, fp::narrow(fpscr, value));
            }
        }
        // VCVT between floating and fixed point, in place
        (0b1010 | 0b1011 | 0b1110 | 0b1111, _) => {
            let size = if bit(op, 7) { 32 } else { 16 };
            let fraction = size - (bits(op, 0, 3) << 1 | bit(op, 5) as u32);
            let signed = !bit(op, 16);
            if opc2 & 0b0100 != 0 {
                let value = if wide {
                    fp::input::<f64>(fpscr, cpu.vfp.d(d))
                } else {
                    fp::input::<f32>(fpscr, cpu.vfp.s(d)) as f64
                };
                // the result extended to the register, by its sign when signed
                let fixed = fp::to_fixed(value, fraction, size, signed);
                let extended = if signed && size == 16 { fixed as u16 as i16 as i32 as u32 } else { fixed };
                if wide {
                    cpu.vfp.set_d(d, if signed { extended as i32 as i64 as u64 } else { extended as u64 });
                } else {
                    cpu.vfp.set_s(d, extended);
                }
            } else {
                let fixed = if wide { cpu.vfp.d(d) as u32 } else { cpu.vfp.s(d) };
                // from fixed point rounds to nearest, whatever the mode
                let value = fp::from_fixed(fixed, fraction, size, signed);
                if wide {
                    cpu.vfp.set_d(d, fp::output::<f64>(fpscr, value));
                } else {
                    cpu.vfp.set_s(d, fp::narrow(fpscr & !(3 << 22), value));
                }
            }
        }
        // VCVT and VCVTR to an integer in a single register, VCVT rounding
        // toward zero and VCVTR in FPSCR's mode
        (0b1100 | 0b1101, _) => {
            let value =
                if wide { fp::input::<f64>(fpscr, cpu.vfp.d(m)) } else { fp::input::<f32>(fpscr, cpu.vfp.s(m)) as f64 };
            let mode = if bit(op, 7) { 3 } else { fp::rounding(fpscr) };
            let d = single(bits(op, 12, 15), bit(op, 22));
            cpu.vfp.set_s(d, fp::to_integer(value, mode, opc2 == 0b1101));
        }
        _ => return cpu.undefined(op),
    }
    None
}

/// a double NaN made single: the default one under DN, else quieted with
/// the top of its payload.
fn nan_narrowed(fpscr: u32, bits: u64) -> u32 {
    if fpscr & fp::DN != 0 {
        return fp::DEFAULT_NAN_32;
    }
    ((bits >> 32) as u32 & 0x8000_0000) | 0x7FC0_0000 | ((bits >> 29) as u32 & 0x003F_FFFF)
}

/// a single NaN made double.
fn nan_widened(fpscr: u32, bits: u32) -> u64 {
    if fpscr & fp::DN != 0 {
        return fp::DEFAULT_NAN_64;
    }
    ((bits & 0x8000_0000) as u64) << 32 | 0x7FF8_0000_0000_0000 | ((bits & 0x003F_FFFF) as u64) << 29
}

/// A7.8, MRC and MCR on coprocessors 10 and 11: VMOV between a core
/// register and a single or a scalar of a double, VDUP, VMRS and VMSR.
pub fn register_transfer(cpu: &mut Cpu, op: u32) -> Option<Exit> {
    let reading = bit(op, 20);
    let t = bits(op, 12, 15) as usize;
    let opc1 = bits(op, 21, 23);
    let wide = bit(op, 8);
    if !wide && opc1 == 0b111 {
        return system_register(cpu, op, reading, t);
    }
    if t == PC {
        return cpu.undefined(op);
    }
    if !wide && opc1 == 0 {
        let n = single(bits(op, 16, 19), bit(op, 7));
        if reading {
            cpu.regs[t] = cpu.vfp.s(n);
        } else {
            cpu.vfp.set_s(n, cpu.regs[t]);
        }
        return None;
    }
    if !wide {
        return cpu.undefined(op);
    }
    // a scalar, a lane of a double: opc1:opc2 give its size and index, and
    // VDUP is the write with bit 23 set
    let n = double(bits(op, 16, 19), bit(op, 7));
    let opc2 = bits(op, 5, 6);
    if !reading && bit(op, 23) {
        return crate::neon::duplicate_core(cpu, op);
    }
    let selector = (opc1 & 0b11) << 2 | opc2;
    let (size, index) = match selector {
        s if s & 0b1000 != 0 => (8, s & 0b111),
        s if s & 0b0001 != 0 => (16, (s >> 1) & 0b11),
        s if s & 0b0011 == 0 => (32, s >> 2),
        _ => return cpu.undefined(op),
    };
    let shift = index * size;
    let mask = if size == 32 { u64::from(u32::MAX) } else { (1u64 << size) - 1 } << shift;
    let lanes = cpu.vfp.d(n);
    if reading {
        let unsigned = bit(op, 23);
        if size == 32 && unsigned {
            return cpu.undefined(op);
        }
        let value = ((lanes & mask) >> shift) as u32;
        cpu.regs[t] = match (size, unsigned) {
            (8, false) => value as u8 as i8 as i32 as u32,
            (16, false) => value as u16 as i16 as i32 as u32,
            _ => value,
        };
    } else {
        cpu.vfp.set_d(n, (lanes & !mask) | ((cpu.regs[t] as u64) << shift & mask));
    }
    None
}

/// VMRS and VMSR. user mode reaches FPSCR and nothing else: FPSID, the
/// MVFRs and FPEXC are for the kernel.
fn system_register(cpu: &mut Cpu, op: u32, reading: bool, t: usize) -> Option<Exit> {
    if bits(op, 16, 19) != 1 {
        return cpu.undefined(op);
    }
    match (reading, t == PC) {
        // VMRS APSR_nzcv, FPSCR moves the comparison flags across
        (true, true) => {
            let value = cpu.vfp.fpscr;
            cpu.cpsr.n = bit(value, 31);
            cpu.cpsr.z = bit(value, 30);
            cpu.cpsr.c = bit(value, 29);
            cpu.cpsr.v = bit(value, 28);
        }
        (true, false) => cpu.regs[t] = cpu.vfp.fpscr,
        (false, true) => return cpu.undefined(op),
        (false, false) => cpu.vfp.fpscr = cpu.regs[t],
    }
    None
}

/// MCRR and MRRC: VMOV between two core registers and two singles or a
/// double.
pub fn transfer_pair(cpu: &mut Cpu, op: u32) -> Option<Exit> {
    let reading = bit(op, 20);
    let (t, t2) = (bits(op, 12, 15) as usize, bits(op, 16, 19) as usize);
    if t == PC || t2 == PC || (reading && t == t2) || bits(op, 6, 7) != 0 || !bit(op, 4) {
        return cpu.undefined(op);
    }
    if bit(op, 8) {
        let m = double(op & 0xF, bit(op, 5));
        if reading {
            let value = cpu.vfp.d(m);
            cpu.regs[t] = value as u32;
            cpu.regs[t2] = (value >> 32) as u32;
        } else {
            cpu.vfp.set_d(m, cpu.regs[t] as u64 | (cpu.regs[t2] as u64) << 32);
        }
    } else {
        let m = single(op & 0xF, bit(op, 5));
        if m == 31 {
            return cpu.undefined(op);
        }
        if reading {
            cpu.regs[t] = cpu.vfp.s(m);
            cpu.regs[t2] = cpu.vfp.s(m + 1);
        } else {
            cpu.vfp.set_s(m, cpu.regs[t]);
            cpu.vfp.set_s(m + 1, cpu.regs[t2]);
        }
    }
    None
}

/// A7.6: VLDR, VSTR, VLDM, VSTM, VPUSH and VPOP.
pub fn load_store<B: Bus>(cpu: &mut Cpu, bus: &mut B, op: u32) -> Option<Exit> {
    let wide = bit(op, 8);
    let (index, add, writeback, loading) = (bit(op, 24), bit(op, 23), bit(op, 21), bit(op, 20));
    let n = bits(op, 16, 19) as usize;
    let imm8 = op & 0xFF;
    let first = if wide { double(bits(op, 12, 15), bit(op, 22)) } else { single(bits(op, 12, 15), bit(op, 22)) };
    // the PC as a base is word aligned
    let base = if n == PC { cpu.aligned_pc() } else { cpu.regs[n] };
    if index && !writeback {
        // VLDR and VSTR
        let address = if add { base.wrapping_add(imm8 * 4) } else { base.wrapping_sub(imm8 * 4) };
        transfer(cpu, bus, wide, loading, first, address);
        return None;
    }
    // VLDM and VSTM increment after, or decrement before with writeback,
    // which the PC can't take
    if index == add || (n == PC && writeback) {
        return cpu.undefined(op);
    }
    // doubles count in pairs of words, FLDMX's odd count leaving one over
    let count = if wide { imm8 / 2 } else { imm8 } as usize;
    let last = first + count;
    if count == 0 || last > 32 || (wide && count > 16) {
        return cpu.undefined(op);
    }
    let start = if add { base } else { base.wrapping_sub(imm8 * 4) };
    for (i, register) in (first..last).enumerate() {
        let address = start.wrapping_add(i as u32 * if wide { 8 } else { 4 });
        transfer(cpu, bus, wide, loading, register, address);
    }
    if writeback {
        cpu.regs[n] = if add { base.wrapping_add(imm8 * 4) } else { base.wrapping_sub(imm8 * 4) };
    }
    None
}

fn transfer<B: Bus>(cpu: &mut Cpu, bus: &mut B, wide: bool, loading: bool, register: usize, address: u32) {
    match (wide, loading) {
        (true, true) => {
            let value = bus.read32(address) as u64 | (bus.read32(address.wrapping_add(4)) as u64) << 32;
            cpu.vfp.set_d(register, value);
        }
        (true, false) => {
            let value = cpu.vfp.d(register);
            bus.write32(address, value as u32);
            bus.write32(address.wrapping_add(4), (value >> 32) as u32);
        }
        (false, true) => cpu.vfp.set_s(register, bus.read32(address)),
        (false, false) => bus.write32(address, cpu.vfp.s(register)),
    }
}
