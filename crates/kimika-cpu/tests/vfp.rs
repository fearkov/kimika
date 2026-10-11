//! VFPv3 with 32 double registers.

mod common;

use common::{assert_flags, assert_reg, run_thumb_with, run_to_exit, run_with, CODE_BASE, DATA_BASE, STACK_TOP};
use kimika_cpu::Exit;

const FZ: u32 = 1 << 24;
const DN: u32 = 1 << 25;

fn set_s(cpu: &mut kimika_cpu::Cpu, index: usize, value: f32) {
    cpu.vfp.set_s(index, value.to_bits());
}

fn set_d(cpu: &mut kimika_cpu::Cpu, index: usize, value: f64) {
    cpu.vfp.set_d(index, value.to_bits());
}

#[test]
fn single_precision_arithmetic() {
    let Some(r) = run_with(
        "vadd.f32 s4, s0, s1\n vsub.f32 s5, s0, s1\n vmul.f32 s6, s0, s1\n vdiv.f32 s7, s0, s1\n vnmul.f32 s8, s0, s1",
        |cpu, _| {
            set_s(cpu, 0, 10.0);
            set_s(cpu, 1, 4.0);
        },
    ) else {
        return;
    };
    assert_eq!((r.s(4), r.s(5), r.s(6), r.s(7), r.s(8)), (14.0, 6.0, 40.0, 2.5, -40.0));
}

#[test]
fn double_precision_in_the_upper_registers() {
    let Some(r) = run_with("vadd.f64 d17, d16, d31\n vmul.f64 d18, d17, d17\n vsqrt.f64 d19, d18", |cpu, _| {
        set_d(cpu, 16, 1.5);
        set_d(cpu, 31, 2.5);
    }) else {
        return;
    };
    assert_eq!((r.d(17), r.d(18), r.d(19)), (4.0, 16.0, 4.0));
    // nothing leaked into the lower registers
    assert_eq!(r.d_bits(1), 0);
}

#[test]
fn multiply_accumulate_rounds_twice() {
    let Some(r) = run_with(
        "vmla.f32 s0, s1, s2\n vmls.f32 s3, s1, s2\n vnmla.f32 s4, s1, s2\n vnmls.f32 s5, s1, s2",
        |cpu, _| {
            for (index, value) in [(0, 1.0), (1, 3.0), (2, 4.0), (3, 1.0), (4, 1.0), (5, 1.0)] {
                set_s(cpu, index, value);
            }
        },
    ) else {
        return;
    };
    assert_eq!((r.s(0), r.s(3), r.s(4), r.s(5)), (13.0, -11.0, -13.0, 11.0));
}

#[test]
fn moves_and_signs_keep_every_bit_under_flush_to_zero() {
    // games keep two singles in a double, whose bits read as a subnormal
    // double when the high one is zero
    let Some(r) = run_with("vmov.f64 d8, d0\n vneg.f64 d9, d0\n vabs.f64 d10, d9\n vmov.f32 s30, s31", |cpu, _| {
        cpu.vfp.fpscr = FZ;
        set_s(cpu, 0, 2.5);
        cpu.vfp.set_s(31, 1);
    }) else {
        return;
    };
    assert_eq!(r.s(16), 2.5);
    assert_eq!(r.s(18), 2.5);
    assert_eq!(r.cpu.vfp.s(19), 0x8000_0000);
    assert_eq!(r.s(20), 2.5);
    assert_eq!(r.cpu.vfp.s(30), 1);
}

#[test]
fn immediates() {
    let Some(r) =
        run_with("vmov.f32 s0, #1.5\n vmov.f64 d17, #-2.0\n vmov.f32 s1, #0.125\n vmov.f64 d18, #31.0", |_, _| {})
    else {
        return;
    };
    assert_eq!((r.s(0), r.d(17), r.s(1), r.d(18)), (1.5, -2.0, 0.125, 31.0));
}

#[test]
fn a_nan_from_numbers_is_arms_default_one() {
    let Some(r) = run_with("vdiv.f32 s2, s0, s0\n vsub.f64 d2, d4, d4", |cpu, _| {
        set_d(cpu, 4, f64::INFINITY);
    }) else {
        return;
    };
    assert_eq!(r.cpu.vfp.s(2), 0x7FC0_0000);
    assert_eq!(r.d_bits(2), 0x7FF8_0000_0000_0000);
}

#[test]
fn nans_propagate_the_way_arm_picks_them() {
    let Some(r) =
        run_with("vadd.f32 s2, s0, s1\n vadd.f32 s3, s4, s1\n vmsr fpscr, r0\n vadd.f32 s5, s4, s1", |cpu, _| {
            cpu.vfp.set_s(0, 0x7FC0_1234);
            cpu.vfp.set_s(1, 0x7F80_0001);
            cpu.vfp.set_s(4, 0x7FC0_5678);
            cpu.regs[0] = DN;
        })
    else {
        return;
    };
    // the signaling one wins, quieted
    assert_eq!(r.cpu.vfp.s(2), 0x7FC0_0001);
    assert_eq!(r.cpu.vfp.s(3), 0x7FC0_0001);
    assert_eq!(r.cpu.vfp.s(5), 0x7FC0_0000);
}

#[test]
fn flush_to_zero_flushes_inputs_too() {
    let Some(r) = run_with("vmul.f32 s2, s0, s1\n vmsr fpscr, r1\n vmul.f32 s3, s0, s1", |cpu, _| {
        cpu.vfp.fpscr = FZ;
        set_s(cpu, 0, 1e-40);
        set_s(cpu, 1, 1e30);
    }) else {
        return;
    };
    assert_eq!(r.s(2), 0.0);
    assert_eq!(r.s(3), 1e-40f32 * 1e30);
}

#[test]
fn conversions_between_integers_and_floats() {
    let Some(r) = run_with(
        "vcvt.f32.s32 s1, s0\n vcvt.f64.u32 d1, s4\n vcvt.s32.f32 s5, s6\n vcvtr.s32.f32 s7, s6\n vcvt.u32.f64 s8, d5\n vmsr fpscr, r0\n vcvtr.s32.f32 s9, s6",
        |cpu, _| {
            cpu.vfp.set_s(0, (-7i32) as u32);
            cpu.vfp.set_s(4, 0xFFFF_FFFF);
            set_s(cpu, 6, -2.5);
            set_d(cpu, 5, -1.0);
            // round toward plus infinity
            cpu.regs[0] = 1 << 22;
        },
    ) else {
        return;
    };
    assert_eq!(r.s(1), -7.0);
    assert_eq!(r.d(1), 4294967295.0);
    assert_eq!(r.cpu.vfp.s(5) as i32, -2);
    // to nearest, ties to even
    assert_eq!(r.cpu.vfp.s(7) as i32, -2);
    assert_eq!(r.cpu.vfp.s(8), 0);
    assert_eq!(r.cpu.vfp.s(9) as i32, -2);
}

#[test]
fn conversions_between_sizes_and_fixed_point() {
    let Some(r) = run_with(
        "vcvt.f64.f32 d8, s0\n vcvt.f32.f64 s2, d8\n vcvt.s32.f32 s3, s3, #8\n vcvt.f32.u16 s4, s4, #4\n vcvtb.f16.f32 s5, s0\n vcvtt.f32.f16 s6, s7",
        |cpu, _| {
            set_s(cpu, 0, 1.5);
            set_s(cpu, 3, -1.25);
            cpu.vfp.set_s(4, 0x0000_0028);
            cpu.vfp.set_s(5, 0xAAAA_0000);
            cpu.vfp.set_s(7, 0xC000_0000);
        },
    ) else {
        return;
    };
    assert_eq!(r.d(8), 1.5);
    assert_eq!(r.s(2), 1.5);
    assert_eq!(r.cpu.vfp.s(3) as i32, -320);
    assert_eq!(r.s(4), 2.5);
    // the bottom half takes 1.5 as a half, the top keeps what it had
    assert_eq!(r.cpu.vfp.s(5), 0xAAAA_3E00);
    assert_eq!(r.s(6), -2.0);
}

#[test]
fn comparisons_reach_the_cpu_flags() {
    let Some(r) = run_with(
        "vcmp.f32 s0, s1\n vmrs APSR_nzcv, fpscr\n movlt r0, #1\n vcmp.f64 d1, #0\n vmrs APSR_nzcv, fpscr",
        |cpu, _| {
            set_s(cpu, 0, 1.0);
            set_s(cpu, 1, 2.0);
        },
    ) else {
        return;
    };
    assert_reg!(r, 0, 1);
    assert_flags!(r, "-ZC");
}

#[test]
fn transfers_with_core_registers() {
    let Some(r) = run_with(
        "vmov s9, r0\n vmov r1, s9\n vmov d20, r2, r3\n vmov r4, r5, d20\n vmov.32 d0[1], r0\n vmov.s8 r6, d1[7]\n vmov.u16 r7, d1[3]\n vmov.8 d2[2], r0\n vmov r8, r9, s14, s15",
        |cpu, _| {
            cpu.regs[0] = 0x1234_5678;
            cpu.regs[2] = 0xAAAA_AAAA;
            cpu.regs[3] = 0xBBBB_BBBB;
            cpu.vfp.set_d(1, 0x80FF_0000_0000_0000);
            cpu.vfp.set_s(14, 14);
            cpu.vfp.set_s(15, 15);
        },
    ) else {
        return;
    };
    assert_reg!(r, 1, 0x1234_5678);
    assert_reg!(r, 4, 0xAAAA_AAAA);
    assert_reg!(r, 5, 0xBBBB_BBBB);
    assert_eq!(r.d_bits(0), 0x1234_5678_0000_0000);
    assert_reg!(r, 6, 0xFFFF_FF80);
    assert_reg!(r, 7, 0x80FF);
    assert_eq!(r.d_bits(2), 0x0078_0000);
    assert_reg!(r, 8, 14);
    assert_reg!(r, 9, 15);
}

#[test]
fn loads_stores_and_the_stack() {
    let Some(r) = run_with(
        "vldr d16, [r0]\n vstr s1, [r0, #8]\n vldmia r0!, {s4-s6}\n vpush {d16-d17}\n vpop {d24-d25}\n vldr s7, value\n b over\n value: .word 0x3F800000\n over: nop",
        |cpu, bus| {
            cpu.regs[0] = DATA_BASE;
            bus.write_u32(DATA_BASE, 0x1111_1111);
            bus.write_u32(DATA_BASE + 4, 0x2222_2222);
            cpu.vfp.set_s(1, 0x3333_3333);
            cpu.vfp.set_d(17, 0x4444_4444_5555_5555);
        },
    ) else {
        return;
    };
    assert_eq!(r.d_bits(16), 0x2222_2222_1111_1111);
    assert_eq!((r.cpu.vfp.s(4), r.cpu.vfp.s(5), r.cpu.vfp.s(6)), (0x1111_1111, 0x2222_2222, 0x3333_3333));
    assert_reg!(r, 0, DATA_BASE + 12);
    assert_eq!(r.d_bits(24), 0x2222_2222_1111_1111);
    assert_eq!(r.d_bits(25), 0x4444_4444_5555_5555);
    assert_reg!(r, 13, STACK_TOP);
    assert_eq!(r.s(7), 1.0);
}

#[test]
fn system_registers() {
    let Some(r) = run_with("vmsr fpscr, r3\n vmrs r4, fpscr\n vmrs APSR_nzcv, fpscr", |cpu, _| {
        cpu.regs[3] = 0x6300_0000;
    }) else {
        return;
    };
    assert_reg!(r, 4, 0x6300_0000);
    assert_flags!(r, "-ZC");
    // FPSID, the MVFRs and FPEXC are the kernel's
    for source in ["vmrs r0, fpsid", "vmrs r0, mvfr0", "vmrs r0, mvfr1", "vmrs r0, fpexc", "vmsr fpexc, r0"] {
        let Some(r) = run_to_exit(source, false, |_, _| {}) else { return };
        assert!(matches!(r.exit, Exit::Undefined { pc: CODE_BASE, .. }), "{source}: {:?}", r.exit);
    }
}

#[test]
fn short_vectors_step_through_their_bank() {
    // LEN 4, STRIDE 1: s8-s11 = s16-s19 + s24, with s24 a scalar in the
    // last bank it is not, so it steps too
    let Some(r) = run_with("vmsr fpscr, r0\n vadd.f32 s8, s16, s24\n mov r1, #0\n vmsr fpscr, r1", |cpu, _| {
        cpu.regs[0] = 3 << 16;
        for i in 0..4 {
            set_s(cpu, 16 + i, i as f32);
            set_s(cpu, 24 + i, 10.0);
        }
    }) else {
        return;
    };
    assert_eq!((r.s(8), r.s(9), r.s(10), r.s(11)), (10.0, 11.0, 12.0, 13.0));
}

#[test]
fn thumb_reaches_the_same_instructions() {
    let Some(r) = run_thumb_with("vadd.f64 d17, d16, d16\n vmov r0, s0\n vldr s2, [r1]", |cpu, bus| {
        set_d(cpu, 16, 2.0);
        cpu.vfp.set_s(0, 7);
        cpu.regs[1] = DATA_BASE;
        bus.write_u32(DATA_BASE, 9);
    }) else {
        return;
    };
    assert_eq!(r.d(17), 4.0);
    assert_reg!(r, 0, 7);
    assert_eq!(r.cpu.vfp.s(2), 9);
}
