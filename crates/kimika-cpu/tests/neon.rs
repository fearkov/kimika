//! NEON, Advanced SIMD. the expected values came from QEMU running the same
//! instructions from the same registers, which the oracle example does for
//! any piece of assembly.

mod common;

use common::{assert_reg, run_thumb_with, run_to_exit, run_with, Run, TestBus, CODE_BASE, DATA_BASE};
use kimika_cpu::{Cpu, Exit};

const QC: u32 = 1 << 27;

/// what a test starts from: D registers, core registers, FPSCR, and bytes
/// at DATA_BASE.
#[derive(Default)]
struct Start<'a> {
    d: &'a [(usize, u64)],
    r: &'a [(usize, u32)],
    fpscr: u32,
    memory: &'a [u8],
}

fn set_up(start: &Start, cpu: &mut Cpu, bus: &mut TestBus) {
    for &(d, value) in start.d {
        cpu.vfp.set_d(d, value);
    }
    for &(r, value) in start.r {
        cpu.regs[r] = value;
    }
    cpu.vfp.fpscr = start.fpscr;
    bus.write_bytes(DATA_BASE, start.memory);
}

fn neon(source: &str, start: Start) -> Option<Run> {
    run_with(source, |cpu, bus| set_up(&start, cpu, bus))
}

fn neon_thumb(source: &str, start: Start) -> Option<Run> {
    run_thumb_with(source, |cpu, bus| set_up(&start, cpu, bus))
}

/// asserts D registers hold these values, in hex on failure.
fn assert_d(r: &Run, expected: &[(usize, u64)]) {
    for &(d, want) in expected {
        let got = r.d_bits(d);
        assert_eq!(got, want, "d{d} = 0x{got:016X}, expected 0x{want:016X}\n{:?}", r.cpu);
    }
}

/// VADD and VSUB wrap, VQADD and VQSUB saturate and set QC.
#[test]
fn integer_arithmetic_wraps_and_saturates() {
    let Some(r) = neon(
        "vadd.i8 d0, d1, d2
         vsub.i16 d3, d1, d2
         vqadd.s8 d4, d1, d2
         vqadd.u8 d5, d1, d2
         vqsub.u16 d6, d2, d1
         vqadd.s64 d7, d1, d2",
        Start { d: &[(1, 0x7F80_017F_00FF_8001), (2, 0x0180_FF01_0001_8080)], ..Start::default() },
    ) else {
        return;
    };
    assert_d(
        &r,
        &[
            (0, 0x8000_0080_0000_0081),
            (3, 0x7E00_027E_00FE_FF81),
            (4, 0x7F80_007F_0000_8081),
            (5, 0x80FF_FF80_00FF_FF81),
            (6, 0x0000_FD82_0000_007F),
            (7, 0x7FFF_FFFF_FFFF_FFFF),
        ],
    );
    assert_eq!(r.cpu.vfp.fpscr, QC);
}

/// the halving adds keep the carry, comparisons give all ones.
#[test]
fn halving_and_comparisons() {
    let Some(r) = neon(
        "vhadd.s8 d0, d1, d2
         vrhadd.u8 d3, d1, d2
         vhsub.s16 d4, d1, d2
         vcgt.s8 d5, d1, d2
         vcge.u16 d6, d1, d2
         vtst.32 d7, d1, d2
         vceq.i8 d8, d1, d2",
        Start { d: &[(1, 0x7F80_017F_00FF_8001), (2, 0x0180_FF01_0001_8080)], ..Start::default() },
    ) else {
        return;
    };
    assert_d(
        &r,
        &[
            (0, 0x4080_0040_0000_80C0),
            (3, 0x4080_8040_0080_8041),
            (4, 0x3F00_013F_007F_FFC0),
            (5, 0xFF00_FFFF_0000_00FF),
            (6, 0xFFFF_0000_FFFF_0000),
            (7, 0xFFFF_FFFF_FFFF_FFFF),
            (8, 0x00FF_0000_FF00_FF00),
        ],
    );
}

#[test]
fn maximum_minimum_and_differences() {
    let Some(r) = neon(
        "vmax.s16 d0, d1, d2
         vmin.u32 d3, d1, d2
         vabd.s8 d4, d1, d2
         vaba.u8 d5, d1, d2",
        Start {
            d: &[(1, 0x7F80_017F_00FF_8001), (2, 0x0180_FF01_0001_8080), (5, 0x0102_0304_0506_0708)],
            ..Start::default()
        },
    ) else {
        return;
    };
    assert_d(
        &r,
        &[
            (0, 0x7F80_017F_00FF_8080),
            (3, 0x0180_FF01_0001_8080),
            (4, 0x7E00_027E_0002_0081),
            (5, 0x7F02_0182_0504_0787),
        ],
    );
}

/// VMUL of polynomials is carryless.
#[test]
fn multiplies() {
    let Some(r) = neon(
        "vmul.i16 q0, q1, q2
         vmla.i32 d6, d2, d4
         vmls.i8 d7, d2, d4
         vmul.p8 d8, d2, d5",
        Start {
            d: &[
                (1, 0x7F80_017F_00FF_8001),
                (2, 0x0180_FF01_0001_8080),
                (4, 0x80FF_7F01_02FE_40C0),
                (5, 0x7F01_807F_03FD_3F41),
                (3, 0x1234_5678_90AB_CDEF),
                (6, 0x1111_1111_2222_2222),
                (7, 0x0102_0304_0506_0708),
            ],
            ..Start::default()
        },
    ) else {
        return;
    };
    assert_d(
        &r,
        &[
            (0, 0x7E80_7E01_02FE_6000),
            (1, 0xDE34_E588_F9FF_1AAF),
            (6, 0x9312_8F12_0262_8222),
            (7, 0x8182_8203_0508_0708),
            (8, 0x7F80_807F_00FD_8080),
        ],
    );
}

/// pairs of n, then pairs of m.
#[test]
fn pairwise() {
    let Some(r) = neon(
        "vpadd.i16 d0, d1, d2
         vpmax.s8 d3, d1, d2
         vpmin.u32 d4, d1, d2",
        Start { d: &[(1, 0x7F80_017F_00FF_8001), (2, 0x0180_FF01_0001_8080)], ..Start::default() },
    ) else {
        return;
    };
    assert_d(&r, &[(0, 0x0081_8081_80FF_8100), (3, 0x0101_0180_7F7F_0001), (4, 0x0001_8080_00FF_8001)]);
}

/// the minimum times itself saturates.
#[test]
fn doubling_high_multiplies() {
    let Some(r) = neon(
        "vqdmulh.s16 d0, d1, d2
         vqrdmulh.s32 d3, d1, d2
         vqrdmulh.s16 d4, d5, d5",
        Start {
            d: &[(1, 0x8000_4000_1234_7FFF), (2, 0x8000_4000_FEDC_7FFF), (5, 0x0001_FFFF_4000_C000)],
            ..Start::default()
        },
    ) else {
        return;
    };
    assert_d(&r, &[(0, 0x7FFF_2000_FFD6_7FFE), (3, 0x7FFF_8000_FFD6_8A70), (4, 0x0000_0000_2000_2000)]);
    assert_eq!(r.cpu.vfp.fpscr, QC);
}

/// m's lanes shifted by the bottom byte of n's, negative to the right.
#[test]
fn shifts_by_register() {
    let Some(r) = neon(
        "vshl.s8 d0, d1, d2
         vshl.u64 d3, d1, d2
         vrshl.s16 d4, d1, d2
         vqshl.u8 d5, d1, d2
         vqrshl.s32 d6, d1, d2
         vqshl.s16 d7, d1, d2",
        Start { d: &[(1, 0x80FF_7F01_02FE_40C3), (2, 0x01FF_07F8_FC04_02C1)], ..Start::default() },
    ) else {
        return;
    };
    assert_d(
        &r,
        &[
            (0, 0x00FF_8000_00E0_00FF),
            (3, 0x0000_0000_0000_0001),
            (4, 0xC080_007F_2FE0_0000),
            (5, 0xFF7F_FF00_00FF_FF00),
            (6, 0xFF80_FF7F_0000_0000),
            (7, 0xC07F_007F_2FE0_0000),
        ],
    );
    assert_eq!(r.cpu.vfp.fpscr, QC);
}

#[test]
fn bitwise() {
    let Some(r) = neon(
        "vand d0, d1, d2
         vbic d3, d1, d2
         vorr d4, d1, d2
         vorn d5, d1, d2
         veor d6, d1, d2
         vbsl d7, d1, d2
         vbit d8, d1, d2
         vbif d9, d1, d2",
        Start {
            d: &[
                (1, 0xFF00_FF00_F0F0_F0F0),
                (2, 0x0F0F_0F0F_FFFF_0000),
                (7, 0xFFFF_0000_FF00_FF00),
                (8, 0xFFFF_0000_FF00_FF00),
                (9, 0xFFFF_0000_FF00_FF00),
            ],
            ..Start::default()
        },
    ) else {
        return;
    };
    assert_d(
        &r,
        &[
            (0, 0x0F00_0F00_F0F0_0000),
            (3, 0xF000_F000_0000_F0F0),
            (4, 0xFF0F_FF0F_FFFF_F0F0),
            (5, 0xFFF0_FFF0_F0F0_FFFF),
            (6, 0xF00F_F00F_0F0F_F0F0),
            (7, 0xFF00_0F0F_F0FF_F000),
            (8, 0xFFF0_0F00_F0F0_FF00),
            (9, 0xFF0F_F000_FF00_F0F0),
        ],
    );
}

#[test]
fn long_and_wide() {
    let Some(r) = neon(
        "vaddl.s8 q0, d4, d5
         vaddw.u16 q1, q1, d5
         vsubl.u32 q4, d4, d5
         vabdl.s16 q5, d4, d5
         vabal.u8 q6, d4, d5",
        Start {
            d: &[
                (4, 0x80FF_7F01_02FE_40C0),
                (5, 0x7F01_807F_03FD_3F41),
                (2, 0x0001_0002_0003_0004),
                (3, 0xFFFF_0000_FFFF_0000),
                (12, 0x0001_0001_0001_0001),
                (13, 0x0002_0002_0002_0002),
            ],
            ..Start::default()
        },
    ) else {
        return;
    };
    assert_d(
        &r,
        &[
            (0, 0x0005_FFFB_007F_0001),
            (1, 0xFFFF_0000_FFFF_0080),
            (2, 0x0001_03FF_0003_3F45),
            (3, 0xFFFF_7F01_FFFF_807F),
            (8, 0xFFFF_FFFF_FF01_017F),
            (9, 0x0000_0000_01FD_FE82),
            (10, 0x0000_00FF_0000_017F),
            (11, 0x0000_FE02_0000_FE82),
            (12, 0x0002_0002_0002_0080),
            (13, 0x0003_0100_0003_0080),
        ],
    );
}

#[test]
fn narrowed_high_halves() {
    let Some(r) = neon(
        "vaddhn.i16 d0, q1, q2
         vraddhn.i32 d1, q1, q2
         vsubhn.i64 d6, q1, q2
         vrsubhn.i16 d7, q1, q2",
        Start {
            d: &[
                (2, 0x7FFF_8000_0180_FF7F),
                (3, 0x1234_5678_FFFF_FFFF),
                (4, 0x0001_8000_0080_0081),
                (5, 0x0000_0001_0000_0001),
            ],
            ..Start::default()
        },
    ) else {
        return;
    };
    assert_d(
        &r,
        &[
            (0, 0x1256_FF00_8000_0200),
            (1, 0x1234_0000_8001_0201),
            (6, 0x1234_5677_7FFE_0000),
            (7, 0x1256_0000_8000_01FF),
        ],
    );
}

#[test]
fn long_multiplies() {
    let Some(r) = neon(
        "vmull.s8 q0, d4, d5
         vmull.u32 q1, d4, d5
         vmlal.s16 q4, d4, d5
         vmlsl.u16 q5, d4, d5
         vmull.p8 q6, d4, d5",
        Start {
            d: &[
                (4, 0x80FF_7F01_02FE_40C0),
                (5, 0x7F01_807F_03FD_3F41),
                (8, 0x0000_0001_0000_0002),
                (9, 0x0000_0003_0000_0004),
                (10, 0x1000_0000_1000_0000),
                (11, 0x2000_0000_2000_0000),
            ],
            ..Start::default()
        },
    ) else {
        return;
    };
    assert_d(
        &r,
        &[
            (0, 0x0006_0006_0FC0_EFC0),
            (1, 0xC080_FFFF_C080_007F),
            (2, 0x000B_F0C5_923D_B0C0),
            (3, 0x3FFF_81C0_BC41_817F),
            (8, 0x000B_EF07_0FFF_B0C2),
            (9, 0xC0FE_0202_C0BE_8183),
            (10, 0x0FF4_10FA_0000_4F40),
            (11, 0xE000_FE01_E040_7E81),
            (12, 0x0006_5456_0FC0_30C0),
            (13, 0x3F80_00FF_3F80_007F),
        ],
    );
}

#[test]
fn saturating_doubling_long() {
    let Some(r) = neon(
        "vqdmull.s16 q0, d4, d5
         vqdmlal.s32 q1, d4, d5
         vqdmlsl.s16 q4, d4, d5",
        Start {
            d: &[
                (4, 0x8000_0001_8000_0000),
                (5, 0x8000_FFFF_8000_0002),
                (2, 0x7FFF_FFFF_FFFF_FFF0),
                (3, 0x0000_0000_0000_0010),
                (8, 0x8000_0000_0000_0000),
                (9, 0x7FFF_FFFF_0000_0000),
            ],
            ..Start::default()
        },
    ) else {
        return;
    };
    assert_d(
        &r,
        &[
            (0, 0x7FFF_FFFF_0000_0000),
            (1, 0x7FFF_FFFF_FFFF_FFFE),
            (2, 0x7FFF_FFFF_FFFF_FFFF),
            (3, 0x7FFF_0000_0002_000E),
            (9, 0x0000_0000_0000_0002),
        ],
    );
    assert_eq!(r.cpu.vfp.fpscr, QC);
}

#[test]
fn by_scalar() {
    let Some(r) = neon(
        "vmul.i16 d0, d1, d2[3]
         vmla.f32 q2, q4, d6[1]
         vmull.u16 q5, d1, d2[1]
         vqdmulh.s32 d14, d1, d6[0]
         vmls.i32 d15, d1, d6[1]",
        Start {
            d: &[
                (1, 0x8000_4000_1234_7FFF),
                (2, 0x0003_FFFF_0002_0001),
                (4, 0x3F80_0000_4000_0000),
                (5, 0xC040_0000_BF80_0000),
                (6, 0x4000_0000_8000_0000),
                (8, 0x3F00_0000_3F00_0000),
                (9, 0x4080_0000_4100_0000),
                (15, 0x0000_0005_0000_0006),
            ],
            ..Start::default()
        },
    ) else {
        return;
    };
    assert_d(
        &r,
        &[
            (0, 0x8000_C000_369C_7FFD),
            (4, 0x4000_0000_4040_0000),
            (5, 0x40A0_0000_4170_0000),
            (10, 0x0000_2468_0000_FFFE),
            (11, 0x0001_0000_0000_8000),
            (14, 0x7FFF_C000_EDCB_8001),
            (15, 0x0000_0005_4000_0006),
        ],
    );
}

#[test]
fn shifts_by_immediate() {
    let Some(r) = neon(
        "vshr.s8 d0, d1, #3
         vshr.u64 d2, d1, #64
         vsra.u16 d3, d1, #8
         vrshr.s32 d4, d1, #31
         vrsra.u8 d5, d1, #1
         vsri.16 d6, d1, #4
         vshl.i32 d7, d1, #5
         vsli.8 d8, d1, #7",
        Start {
            d: &[
                (1, 0x80FF_7F01_02FE_40C3),
                (3, 0x0001_0002_0003_0004),
                (5, 0x0102_0304_0506_0708),
                (6, 0xFFFF_FFFF_FFFF_FFFF),
                (8, 0x5555_5555_5555_5555),
            ],
            ..Start::default()
        },
    ) else {
        return;
    };
    assert_d(
        &r,
        &[
            (0, 0xF0FF_0F00_00FF_08F8),
            (3, 0x0081_0081_0005_0044),
            (4, 0xFFFF_FFFF_0000_0000),
            (5, 0x4182_4305_0685_276A),
            (6, 0xF80F_F7F0_F02F_F40C),
            (7, 0x1FEF_E020_5FC8_1860),
            (8, 0x55D5_D5D5_5555_55D5),
        ],
    );
}

#[test]
fn saturating_shifts() {
    let Some(r) = neon(
        "vqshl.s8 d0, d1, #2
         vqshlu.s16 d2, d1, #3
         vqshl.u64 d3, d1, #1",
        Start { d: &[(1, 0x80FF_7F01_02FE_40C3)], ..Start::default() },
    ) else {
        return;
    };
    assert_d(&r, &[(0, 0x80FC_7F04_08F8_7F80), (2, 0x0000_FFFF_17F0_FFFF), (3, 0xFFFF_FFFF_FFFF_FFFF)]);
    assert_eq!(r.cpu.vfp.fpscr, QC);
}

#[test]
fn narrowing_shifts() {
    let Some(r) = neon(
        "vshrn.i16 d0, q1, #4
         vrshrn.i32 d1, q1, #16
         vqshrn.s64 d6, q1, #1
         vqrshrun.s16 d7, q1, #3
         vqshrun.s32 d8, q1, #2
         vqrshrn.u16 d9, q1, #8",
        Start { d: &[(2, 0x7FFF_8000_0180_FF7F), (3, 0x1234_5678_FFFF_FFFF)], ..Start::default() },
    ) else {
        return;
    };
    assert_d(
        &r,
        &[
            (0, 0x2367_FFFF_FF00_18F7),
            (1, 0x1234_0000_8000_0181),
            (6, 0x7FFF_FFFF_7FFF_FFFF),
            (7, 0xFFFF_0000_FF00_3000),
            (8, 0xFFFF_0000_FFFF_FFFF),
            (9, 0x1256_FFFF_8080_02FF),
        ],
    );
    assert_eq!(r.cpu.vfp.fpscr, QC);
}

/// VMOVL is VSHLL by nothing, and VSHLL by the lane's size has its own encoding.
#[test]
fn long_shifts() {
    let Some(r) = neon(
        "vshll.s8 q0, d4, #3
         vmovl.u16 q1, d4
         vshll.i32 q4, d4, #32",
        Start { d: &[(4, 0x80FF_7F01_02FE_40C0), (5, 0x7F01_807F_03FD_3F41)], ..Start::default() },
    ) else {
        return;
    };
    assert_d(
        &r,
        &[
            (0, 0x0010_FFF0_0200_FE00),
            (1, 0xFC00_FFF8_03F8_0008),
            (2, 0x0000_02FE_0000_40C0),
            (3, 0x0000_80FF_0000_7F01),
            (8, 0x02FE_40C0_0000_0000),
            (9, 0x80FF_7F01_0000_0000),
        ],
    );
}

#[test]
fn fixed_point() {
    let Some(r) = neon(
        "vcvt.s32.f32 d0, d4, #8
         vcvt.f32.u32 d1, d5, #16
         vcvt.u32.f32 q1, q4, #1",
        Start {
            d: &[
                (4, 0xC049_0FDB_3FC0_0000),
                (5, 0x0001_8000_8000_0000),
                (8, 0x4F80_0000_BF80_0000),
                (9, 0x7FC0_0000_0000_0001),
            ],
            ..Start::default()
        },
    ) else {
        return;
    };
    assert_d(&r, &[(0, 0xFFFF_FCDC_0000_0180), (1, 0x3FC0_0000_4700_0000), (2, 0xFFFF_FFFF_0000_0000)]);
}

#[test]
fn immediates() {
    let Some(r) = neon(
        "vmov.i32 d0, #0x12
         vmov.i16 d1, #0x3400
         vmvn.i32 d2, #0xAB00
         vorr.i32 d3, #0x00FF0000
         vbic.i16 d4, #0x00F0
         vmov.i64 d5, #0xFF00FF0000FFFF00
         vmov.f32 q3, #-1.5
         vmov.i32 d8, #0x12FF
         vmvn.i32 d9, #0x56FFFF
         vmov.i8 d10, #0x81",
        Start { d: &[(3, 0x0102_0304_0506_0708), (4, 0xFFFF_FFFF_FFFF_FFFF)], ..Start::default() },
    ) else {
        return;
    };
    assert_d(
        &r,
        &[
            (0, 0x0000_0012_0000_0012),
            (1, 0x3400_3400_3400_3400),
            (2, 0xFFFF_54FF_FFFF_54FF),
            (3, 0x01FF_0304_05FF_0708),
            (4, 0xFF0F_FF0F_FF0F_FF0F),
            (5, 0xFF00_FF00_00FF_FF00),
            (6, 0xBFC0_0000_BFC0_0000),
            (7, 0xBFC0_0000_BFC0_0000),
            (8, 0x0000_12FF_0000_12FF),
            (9, 0xFFA9_0000_FFA9_0000),
            (10, 0x8181_8181_8181_8181),
        ],
    );
}

#[test]
fn reversals_and_pairs() {
    let Some(r) = neon(
        "vrev64.8 d0, d4
         vrev32.16 d1, d4
         vrev16.8 d2, d4
         vpaddl.s8 d3, d4
         vpadal.u16 d5, d4",
        Start {
            d: &[(4, 0x80FF_7F01_02FE_40C0), (5, 0x7F01_807F_03FD_3F41), (5, 0x0000_0001_0000_0002)],
            ..Start::default()
        },
    ) else {
        return;
    };
    assert_d(
        &r,
        &[
            (0, 0xC040_FE02_017F_FF80),
            (1, 0x7F01_80FF_40C0_02FE),
            (2, 0xFF80_017F_FE02_C040),
            (3, 0xFF7F_0080_0000_0000),
            (5, 0x0001_0001_0000_43C0),
        ],
    );
}

#[test]
fn counting() {
    let Some(r) = neon(
        "vcls.s8 d0, d4
         vclz.i16 d1, d4
         vcnt.8 d2, d4
         vmvn d3, d4",
        Start { d: &[(4, 0x80FF_7F01_02FE_40C0), (5, 0x7F01_807F_03FD_3F41)], ..Start::default() },
    ) else {
        return;
    };
    assert_d(
        &r,
        &[
            (0, 0x0007_0006_0506_0001),
            (1, 0x0000_0001_0006_0001),
            (2, 0x0108_0701_0107_0102),
            (3, 0x7F00_80FE_FD01_BF3F),
        ],
    );
}

/// the minimum has no positive.
#[test]
fn saturating_unary() {
    let Some(r) = neon(
        "vqabs.s8 d0, d4
         vqneg.s16 d1, d4",
        Start { d: &[(4, 0x80FF_7F01_02FE_40C0), (5, 0x7F01_807F_03FD_3F41)], ..Start::default() },
    ) else {
        return;
    };
    assert_d(&r, &[(0, 0x7F01_7F01_0202_4040), (1, 0x7F01_80FF_FD02_BF40)]);
    assert_eq!(r.cpu.vfp.fpscr, QC);
}

#[test]
fn comparisons_with_zero() {
    let Some(r) = neon(
        "vcgt.s8 d0, d4, #0
         vcle.s16 d1, d4, #0
         vclt.s32 d2, d4, #0
         vceq.i8 d3, d4, #0
         vcge.f32 d5, d6, #0
         vclt.f32 d7, d6, #0
         vabs.s8 d8, d4
         vneg.s16 d9, d4
         vabs.f32 d10, d6
         vneg.f32 d11, d6",
        Start {
            d: &[(4, 0x80FF_7F01_02FE_40C0), (5, 0x7F01_807F_03FD_3F41), (6, 0x8000_0001_FFC0_0000)],
            ..Start::default()
        },
    ) else {
        return;
    };
    assert_d(
        &r,
        &[
            (0, 0x0000_FFFF_FF00_FF00),
            (1, 0xFFFF_0000_0000_0000),
            (2, 0xFFFF_FFFF_0000_0000),
            (5, 0xFFFF_FFFF_0000_0000),
            (8, 0x8001_7F01_0202_4040),
            (9, 0x7F01_80FF_FD02_BF40),
            (10, 0x0000_0001_7FC0_0000),
            (11, 0x0000_0001_7FC0_0000),
        ],
    );
}

#[test]
fn permutations() {
    let Some(r) = neon(
        "vswp d0, d1
         vtrn.16 d2, d3
         vuzp.8 d4, d5
         vzip.16 q6, q7",
        Start {
            d: &[
                (0, 0x0706_0504_0302_0100),
                (1, 0x0F0E_0D0C_0B0A_0908),
                (2, 0x0003_0002_0001_0000),
                (3, 0x0013_0012_0011_0010),
                (4, 0x0706_0504_0302_0100),
                (5, 0x0F0E_0D0C_0B0A_0908),
                (12, 0x0003_0002_0001_0000),
                (13, 0x0007_0006_0005_0004),
                (14, 0x0013_0012_0011_0010),
                (15, 0x0017_0016_0015_0014),
            ],
            ..Start::default()
        },
    ) else {
        return;
    };
    assert_d(
        &r,
        &[
            (0, 0x0F0E_0D0C_0B0A_0908),
            (1, 0x0706_0504_0302_0100),
            (2, 0x0012_0002_0010_0000),
            (3, 0x0013_0003_0011_0001),
            (4, 0x0E0C_0A08_0604_0200),
            (5, 0x0F0D_0B09_0705_0301),
            (12, 0x0011_0001_0010_0000),
            (13, 0x0013_0003_0012_0002),
            (14, 0x0015_0005_0014_0004),
            (15, 0x0017_0007_0016_0006),
        ],
    );
}

#[test]
fn narrowing_moves() {
    let Some(r) = neon(
        "vmovn.i32 d0, q1
         vqmovn.s16 d1, q2
         vqmovun.s32 d6, q4
         vqmovn.u64 d7, q5",
        Start {
            d: &[
                (2, 0x7FFF_8000_0180_FF7F),
                (3, 0x1234_5678_FFFF_FFFF),
                (4, 0x7FFF_8000_0180_FF7F),
                (5, 0x0012_FF80_007F_0100),
                (8, 0x8000_0000_0001_0000),
                (9, 0x7FFF_FFFF_0000_FFFF),
                (10, 0x0000_0001_FFFF_FFFF),
                (11, 0x0000_0000_FFFF_FFFF),
            ],
            ..Start::default()
        },
    ) else {
        return;
    };
    assert_d(
        &r,
        &[
            (0, 0x5678_FFFF_8000_FF7F),
            (1, 0x1280_7F7F_7F80_7F80),
            (6, 0xFFFF_FFFF_0000_FFFF),
            (7, 0xFFFF_FFFF_FFFF_FFFF),
        ],
    );
    assert_eq!(r.cpu.vfp.fpscr, QC);
}

/// single to half precision rounds to nearest.
#[test]
fn halves() {
    let Some(r) = neon(
        "vcvt.f16.f32 d0, q1
         vcvt.f32.f16 q2, d6",
        Start {
            d: &[(2, 0x477F_F000_3EAA_AAAB), (3, 0x7F80_0001_3380_0000), (6, 0x7E00_FC00_03FF_3C00)],
            ..Start::default()
        },
    ) else {
        return;
    };
    assert_d(&r, &[(0, 0x7E00_0001_7C00_3555), (4, 0x387F_C000_3F80_0000), (5, 0x7FC0_0000_FF80_0000)]);
}

#[test]
fn estimates() {
    let Some(r) = neon(
        "vrecpe.f32 q0, q1
         vrsqrte.f32 q2, q1
         vrecpe.u32 d6, d8
         vrsqrte.u32 d7, d8",
        Start {
            d: &[(2, 0x4049_0000_3F80_0000), (3, 0x7F80_0000_8040_0000), (8, 0x4000_0000_C000_0000)],
            ..Start::default()
        },
    ) else {
        return;
    };
    assert_d(
        &r,
        &[
            (0, 0x3EA3_0000_3F7F_8000),
            (1, 0x0000_0000_FF80_0000),
            (4, 0x3F10_8000_3F7F_8000),
            (5, 0x0000_0000_FF80_0000),
            (6, 0xFFFF_FFFF_AA80_0000),
            (7, 0xFF80_0000_9380_0000),
        ],
    );
}

/// toward zero, saturating, NaN to zero.
#[test]
fn float_integer_conversions() {
    let Some(r) = neon(
        "vcvt.s32.f32 q0, q1
         vcvt.u32.f32 d4, d2
         vcvt.f32.s32 d5, d6
         vcvt.f32.u32 d7, d6",
        Start {
            d: &[(2, 0xC049_0FDB_3FFF_FFFF), (3, 0x7FC0_0000_CF80_0000), (6, 0xFFFF_FFFF_7FFF_FFFF)],
            ..Start::default()
        },
    ) else {
        return;
    };
    assert_d(
        &r,
        &[
            (0, 0xFFFF_FFFD_0000_0001),
            (1, 0x0000_0000_8000_0000),
            (4, 0x0000_0000_0000_0001),
            (5, 0xBF80_0000_4F00_0000),
            (7, 0x4F80_0000_4F00_0000),
        ],
    );
}

/// NEON flushes subnormals and gives the default NaN whatever FPSCR says, and rounds to nearest.
#[test]
fn float_arithmetic_is_standard() {
    let Some(r) = neon(
        "vadd.f32 d0, d1, d2
         vmul.f32 d3, d1, d2
         vmla.f32 d4, d1, d2
         vmls.f32 d5, d1, d2
         vsub.f32 d6, d1, d2
         vabd.f32 d7, d1, d2",
        Start {
            d: &[
                (1, 0x7F80_0001_3F80_0001),
                (2, 0x3F80_0000_0000_FFFF),
                (4, 0x3F80_0000_3F80_0000),
                (5, 0x3F80_0000_3F80_0000),
            ],
            fpscr: 0x00C0_0000,
            ..Start::default()
        },
    ) else {
        return;
    };
    assert_d(
        &r,
        &[
            (0, 0x7FC0_0000_3F80_0001),
            (3, 0x7FC0_0000_0000_0000),
            (4, 0x7FC0_0000_3F80_0000),
            (5, 0x7FC0_0000_3F80_0000),
            (6, 0x7FC0_0000_3F80_0001),
            (7, 0x7FC0_0000_3F80_0001),
        ],
    );
}

/// of two zeros the maximum is +0 and the minimum -0.
#[test]
fn float_comparisons_and_extremes() {
    let Some(r) = neon(
        "vceq.f32 d0, d1, d2
         vcge.f32 d3, d1, d2
         vcgt.f32 d4, d1, d2
         vacge.f32 d5, d1, d2
         vacgt.f32 d6, d1, d2
         vmax.f32 d7, d1, d2
         vmin.f32 d8, d1, d2
         vpadd.f32 d9, d1, d2
         vpmax.f32 d10, d1, d2
         vpmin.f32 d11, d1, d2",
        Start { d: &[(1, 0x8000_0000_C000_0000), (2, 0x0000_0000_3F80_0000)], ..Start::default() },
    ) else {
        return;
    };
    assert_d(
        &r,
        &[
            (0, 0xFFFF_FFFF_0000_0000),
            (3, 0xFFFF_FFFF_0000_0000),
            (5, 0xFFFF_FFFF_FFFF_FFFF),
            (6, 0x0000_0000_FFFF_FFFF),
            (7, 0x0000_0000_3F80_0000),
            (8, 0x8000_0000_C000_0000),
            (9, 0x3F80_0000_C000_0000),
            (10, 0x3F80_0000_8000_0000),
            (11, 0x0000_0000_C000_0000),
        ],
    );
}

/// an infinity times zero counts as zero.
#[test]
fn reciprocal_steps() {
    let Some(r) = neon(
        "vrecps.f32 d0, d1, d2
         vrsqrts.f32 d3, d1, d2",
        Start { d: &[(1, 0x7F80_0000_3FC0_0000), (2, 0x0000_0000_4000_0000)], ..Start::default() },
    ) else {
        return;
    };
    assert_d(&r, &[(0, 0x4000_0000_BF80_0000), (3, 0x3FC0_0000_0000_0000)]);
}

#[test]
fn extract_table_and_duplicate() {
    let Some(r) = neon(
        "vext.8 d0, d1, d2, #3
         vext.8 q2, q4, q5, #13
         vtbl.8 d6, {d1, d2}, d3
         vtbx.8 d7, {d1}, d3
         vdup.16 q6, d1[2]
         vdup.8 d14, r0
         vdup.32 q8, r1",
        Start {
            d: &[
                (1, 0x0706_0504_0302_0100),
                (2, 0x0F0E_0D0C_0B0A_0908),
                (3, 0x10FF_0F08_0701_0009),
                (7, 0xAAAA_AAAA_AAAA_AAAA),
                (8, 0x1716_1514_1312_1110),
                (9, 0x1F1E_1D1C_1B1A_1918),
                (10, 0x2726_2524_2322_2120),
                (11, 0x2F2E_2D2C_2B2A_2928),
            ],
            r: &[(0, 0x1234), (1, 0xCAFE_F00D)],
            ..Start::default()
        },
    ) else {
        return;
    };
    assert_d(
        &r,
        &[
            (0, 0x0A09_0807_0605_0403),
            (4, 0x2423_2221_201F_1E1D),
            (5, 0x2C2B_2A29_2827_2625),
            (6, 0x0000_0F08_0701_0009),
            (7, 0xAAAA_AAAA_0701_00AA),
            (12, 0x0504_0504_0504_0504),
            (13, 0x0504_0504_0504_0504),
            (14, 0x3434_3434_3434_3434),
            (16, 0xCAFE_F00D_CAFE_F00D),
            (17, 0xCAFE_F00D_CAFE_F00D),
        ],
    );
}

/// VLD2 to VLD4 take elements apart in turn.
#[test]
fn loads_of_structures() {
    let Some(r) = neon(
        "vld1.32 {d0-d1}, [r0]!
         vld2.8 {d2, d3}, [r1]
         vld3.16 {d4, d5, d6}, [r2], r3
         vld4.8 {d8, d10, d12, d14}, [r4]",
        Start {
            r: &[(0, DATA_BASE), (1, DATA_BASE), (2, DATA_BASE), (3, 0x10), (4, DATA_BASE)],
            memory: &[
                0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0A, 0x0B, 0x0C, 0x0D, 0x0E, 0x0F, 0x10,
                0x11, 0x12, 0x13, 0x14, 0x15, 0x16, 0x17, 0x18, 0x19, 0x1A, 0x1B, 0x1C, 0x1D, 0x1E, 0x1F,
            ],
            ..Start::default()
        },
    ) else {
        return;
    };
    assert_d(
        &r,
        &[
            (0, 0x0706_0504_0302_0100),
            (1, 0x0F0E_0D0C_0B0A_0908),
            (2, 0x0E0C_0A08_0604_0200),
            (3, 0x0F0D_0B09_0705_0301),
            (4, 0x1312_0D0C_0706_0100),
            (5, 0x1514_0F0E_0908_0302),
            (6, 0x1716_1110_0B0A_0504),
            (8, 0x1C18_1410_0C08_0400),
            (10, 0x1D19_1511_0D09_0501),
            (12, 0x1E1A_1612_0E0A_0602),
            (14, 0x1F1B_1713_0F0B_0703),
        ],
    );
    assert_reg!(r, 0, DATA_BASE + 16);
    assert_reg!(r, 2, DATA_BASE + 16);
}

#[test]
fn stores_of_structures() {
    let Some(r) = neon(
        "vst4.16 {d0-d3}, [r0]
         vst1.8 {d4}, [r1]!
         vst3.32 {d5, d7, d9}, [r2]",
        Start {
            d: &[
                (0, 0x0003_0002_0001_0000),
                (1, 0x0013_0012_0011_0010),
                (2, 0x0023_0022_0021_0020),
                (3, 0x0033_0032_0031_0030),
                (4, 0x8877_6655_4433_2211),
                (5, 0x0000_0001_0000_0000),
                (7, 0x0000_0011_0000_0010),
                (9, 0x0000_0021_0000_0020),
            ],
            r: &[(0, DATA_BASE), (1, DATA_BASE + 32), (2, DATA_BASE + 40)],
            ..Start::default()
        },
    ) else {
        return;
    };
    assert_d(&r, &[]);
    assert_reg!(r, 1, DATA_BASE + 40);
    assert_eq!(
        r.bus.mem[DATA_BASE as usize..DATA_BASE as usize + 64],
        [
            0x00, 0x00, 0x10, 0x00, 0x20, 0x00, 0x30, 0x00, 0x01, 0x00, 0x11, 0x00, 0x21, 0x00, 0x31, 0x00, 0x02, 0x00,
            0x12, 0x00, 0x22, 0x00, 0x32, 0x00, 0x03, 0x00, 0x13, 0x00, 0x23, 0x00, 0x33, 0x00, 0x11, 0x22, 0x33, 0x44,
            0x55, 0x66, 0x77, 0x88, 0x00, 0x00, 0x00, 0x00, 0x10, 0x00, 0x00, 0x00, 0x20, 0x00, 0x00, 0x00, 0x01, 0x00,
            0x00, 0x00, 0x11, 0x00, 0x00, 0x00, 0x21, 0x00, 0x00, 0x00
        ]
    );
}

/// one lane, and every lane from one element.
#[test]
fn lanes() {
    let Some(r) = neon(
        "vld1.16 {d0[2]}, [r0]
         vld2.32 {d2[1], d4[1]}, [r1]!
         vld3.8 {d6[], d7[], d8[]}, [r2]
         vld4.16 {d10[], d11[], d12[], d13[]}, [r3]
         vld1.32 {d14[], d15[]}, [r4]",
        Start {
            d: &[(0, 0xFFFF_FFFF_FFFF_FFFF), (2, 0x1111_1111_1111_1111), (4, 0x2222_2222_2222_2222)],
            r: &[(0, DATA_BASE), (1, DATA_BASE), (2, DATA_BASE), (3, DATA_BASE), (4, DATA_BASE)],
            memory: &[0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0A, 0x0B, 0x0C, 0x0D, 0x0E, 0x0F],
            ..Start::default()
        },
    ) else {
        return;
    };
    assert_d(
        &r,
        &[
            (0, 0xFFFF_0100_FFFF_FFFF),
            (2, 0x0302_0100_1111_1111),
            (4, 0x0706_0504_2222_2222),
            (7, 0x0101_0101_0101_0101),
            (8, 0x0202_0202_0202_0202),
            (10, 0x0100_0100_0100_0100),
            (11, 0x0302_0302_0302_0302),
            (12, 0x0504_0504_0504_0504),
            (13, 0x0706_0706_0706_0706),
            (14, 0x0302_0100_0302_0100),
            (15, 0x0302_0100_0302_0100),
        ],
    );
    assert_reg!(r, 1, DATA_BASE + 8);
}

/// the same instructions as Thumb has them.
#[test]
fn thumb_encodings() {
    let Some(r) = neon_thumb(
        "vqadd.s8 d4, d1, d2
         vmul.f32 q3, q4, q5
         vshr.u16 d0, d1, #4
         vext.8 d14, d1, d2, #1
         vld2.8 {d16, d17}, [r0]!
         vdup.16 d18, r1",
        Start {
            d: &[
                (1, 0x7F80_017F_00FF_8001),
                (2, 0x0180_FF01_0001_8080),
                (8, 0x3F80_0000_4000_0000),
                (9, 0x4040_0000_4080_0000),
                (10, 0x4000_0000_C000_0000),
                (11, 0x3F00_0000_3E80_0000),
            ],
            r: &[(0, DATA_BASE), (1, 0xBEEF)],
            memory: &[0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0A, 0x0B, 0x0C, 0x0D, 0x0E, 0x0F],
            ..Start::default()
        },
    ) else {
        return;
    };
    assert_d(
        &r,
        &[
            (0, 0x07F8_0017_000F_0800),
            (4, 0x7F80_007F_0000_8081),
            (6, 0x4000_0000_C080_0000),
            (7, 0x3FC0_0000_3F80_0000),
            (14, 0x807F_8001_7F00_FF80),
            (16, 0x0E0C_0A08_0604_0200),
            (17, 0x0F0D_0B09_0705_0301),
            (18, 0xBEEF_BEEF_BEEF_BEEF),
        ],
    );
    assert_reg!(r, 0, DATA_BASE + 16);
    assert_eq!(r.cpu.vfp.fpscr, QC);
}

/// what the Cortex-A9 does not have, or ARM leaves unpredictable: a Q form
/// with an odd register, VFPv4's VFMA, ARMv8's VRINTN, AESE and SHA1C, and a
/// list of registers past d31.
#[test]
fn what_the_cortex_a9_refuses() {
    let cases = [
        (".inst 0xF2001840", false),
        (".inst 0xF2000C50", false),
        (".inst 0xF3BA0400", false),
        (".inst 0xF3B00300", false),
        (".inst 0xF2000C40", false),
        (".inst 0xF460FA0F", false),
        (".inst.w 0xEF001840", true),
        (".inst.w 0xF960FA0F", true),
    ];
    for (source, thumb) in cases {
        let Some(r) = run_to_exit(source, thumb, |_, _| {}) else { return };
        let opcode = u32::from_str_radix(source.rsplit("0x").next().unwrap(), 16).unwrap();
        assert_eq!(r.exit, Exit::Undefined { pc: CODE_BASE, opcode }, "{source}");
    }
}
