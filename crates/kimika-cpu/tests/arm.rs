//! ARM state.

mod common;

use common::{assert_flags, assert_reg, run, run_to_exit, run_with, TestBus, CODE_BASE, DATA_BASE, STACK_TOP};
use kimika_cpu::{Cpu, Exit};

// data processing

#[test]
fn sub_borrow_clears_carry() {
    let Some(r) = run("mov r0, #0\n subs r1, r0, #1") else { return };
    assert_reg!(r, 1, 0xFFFF_FFFF);
    assert_flags!(r, "N");
}

#[test]
fn sub_without_borrow_sets_carry() {
    let Some(r) = run("mov r0, #5\n subs r1, r0, #5") else { return };
    assert_reg!(r, 1, 0);
    assert_flags!(r, "-ZC");
}

#[test]
fn signed_overflow_sets_v() {
    let Some(r) = run_with("adds r2, r0, r1", |cpu, _| {
        cpu.regs[0] = 0x7FFF_FFFF;
        cpu.regs[1] = 1;
    }) else {
        return;
    };
    assert_reg!(r, 2, 0x8000_0000);
    assert_flags!(r, "N--V");
}

#[test]
fn adc_carries_in() {
    let Some(r) = run_with("adcs r2, r0, r1", |cpu, _| {
        cpu.regs[0] = 0xFFFF_FFFF;
        cpu.cpsr.c = true;
    }) else {
        return;
    };
    assert_reg!(r, 2, 0);
    assert_flags!(r, "-ZC");
}

#[test]
fn sbc_borrows_in() {
    // with carry clear SBC takes one more
    let Some(r) = run_with("sbcs r2, r0, r1", |cpu, _| {
        cpu.regs[0] = 10;
        cpu.regs[1] = 3;
    }) else {
        return;
    };
    assert_reg!(r, 2, 6);
    assert_flags!(r, "--C");
}

#[test]
fn rsc_reverses_the_operands() {
    let Some(r) = run_with("rsc r2, r0, r1", |cpu, _| {
        cpu.regs[0] = 3;
        cpu.regs[1] = 10;
        cpu.cpsr.c = true;
    }) else {
        return;
    };
    assert_reg!(r, 2, 7);
}

/// the immediate rotate only sets the carry when it rotates.
#[test]
fn immediate_rotate_updates_carry() {
    let Some(r) = run("movs r0, #0x80000000") else { return };
    assert_reg!(r, 0, 0x8000_0000);
    assert_flags!(r, "N-C");
}

#[test]
fn immediate_without_rotate_keeps_carry() {
    let Some(r) = run_with("movs r0, #1", |cpu, _| cpu.cpsr.c = true) else { return };
    assert_flags!(r, "--C");
}

#[test]
fn lsr_zero_means_thirty_two() {
    let Some(r) = run_with("movs r1, r0, lsr #32", |cpu, _| cpu.regs[0] = 0x8000_0000) else { return };
    assert_reg!(r, 1, 0);
    assert_flags!(r, "-ZC");
}

#[test]
fn asr_zero_means_thirty_two() {
    let Some(r) = run_with("movs r1, r0, asr #32", |cpu, _| cpu.regs[0] = 0x8000_0000) else { return };
    assert_reg!(r, 1, 0xFFFF_FFFF);
    assert_flags!(r, "N-C");
}

#[test]
fn rrx_rotates_through_carry() {
    let Some(r) = run_with("movs r1, r0, rrx", |cpu, _| {
        cpu.regs[0] = 3;
        cpu.cpsr.c = true;
    }) else {
        return;
    };
    assert_reg!(r, 1, 0x8000_0001);
    assert_flags!(r, "N-C");
}

#[test]
fn register_shift_of_thirty_two() {
    let Some(r) = run_with("movs r2, r0, lsl r1", |cpu, _| {
        cpu.regs[0] = 1;
        cpu.regs[1] = 32;
    }) else {
        return;
    };
    assert_reg!(r, 2, 0);
    assert_flags!(r, "-ZC");
}

#[test]
fn register_shift_of_zero_keeps_carry() {
    let Some(r) = run_with("movs r2, r0, lsl r1", |cpu, _| {
        cpu.regs[0] = 0x1234;
        cpu.cpsr.c = true;
    }) else {
        return;
    };
    assert_reg!(r, 2, 0x1234);
    assert_flags!(r, "--C");
}

#[test]
fn pc_reads_as_the_instruction_plus_eight() {
    let Some(r) = run("mov r0, pc\n adr r1, here\n here: nop") else { return };
    assert_reg!(r, 0, CODE_BASE + 8);
    assert_reg!(r, 1, CODE_BASE + 8);
}

#[test]
fn movw_and_movt_build_a_word() {
    let Some(r) = run("movw r0, #0x5678\n movt r0, #0x1234\n movw r1, #0xFFFF") else { return };
    assert_reg!(r, 0, 0x1234_5678);
    assert_reg!(r, 1, 0xFFFF);
}

/// ARMv7 turned a write of the PC by data processing in ARM state into an
/// interworking branch.
#[test]
fn moving_into_the_pc_interworks() {
    let Some(r) = run_to_exit(
        "mov pc, r0
         .thumb
         thumb: bkpt #1",
        false,
        |cpu, _| cpu.regs[0] = (CODE_BASE + 4) | 1,
    ) else {
        return;
    };
    assert_eq!(r.exit, Exit::Breakpoint { pc: CODE_BASE + 4, imm: 1 });
    assert!(r.cpu.cpsr.thumb);
}

// multiplies

#[test]
fn umull_and_smull_differ_in_sign() {
    let Some(r) = run_with("umull r0, r1, r4, r5\n smull r2, r3, r4, r5", |cpu, _| {
        cpu.regs[4] = 0xFFFF_FFFF;
        cpu.regs[5] = 2;
    }) else {
        return;
    };
    assert_reg!(r, 0, 0xFFFF_FFFE);
    assert_reg!(r, 1, 1);
    assert_reg!(r, 2, 0xFFFF_FFFE);
    assert_reg!(r, 3, 0xFFFF_FFFF);
}

#[test]
fn umaal_adds_both_halves() {
    let Some(r) = run_with("umaal r0, r1, r2, r3", |cpu, _| {
        cpu.regs[0] = 10;
        cpu.regs[1] = 20;
        cpu.regs[2] = 0x1000;
        cpu.regs[3] = 0x1000;
    }) else {
        return;
    };
    assert_reg!(r, 0, 0x0100_001E);
    assert_reg!(r, 1, 0);
}

#[test]
fn mls_takes_the_product_away() {
    let Some(r) = run_with("mls r0, r1, r2, r3", |cpu, _| {
        cpu.regs[1] = 6;
        cpu.regs[2] = 7;
        cpu.regs[3] = 100;
    }) else {
        return;
    };
    assert_reg!(r, 0, 58);
}

#[test]
fn smlabb_uses_the_bottom_halves() {
    let Some(r) = run_with("smlabb r0, r1, r2, r3", |cpu, _| {
        cpu.regs[1] = 0xDEAD_0003;
        cpu.regs[2] = 0xBEEF_0005;
        cpu.regs[3] = 7;
    }) else {
        return;
    };
    assert_reg!(r, 0, 22);
}

#[test]
fn smlabb_wraps_and_sets_q_when_it_overflows() {
    let Some(r) = run_with("smlabb r0, r1, r2, r3", |cpu, _| {
        cpu.regs[1] = 1;
        cpu.regs[2] = 1;
        cpu.regs[3] = 0x7FFF_FFFF;
    }) else {
        return;
    };
    assert_reg!(r, 0, 0x8000_0000);
    assert_flags!(r, "----Q");
}

#[test]
fn smultt_and_smulwb() {
    let Some(r) = run_with("smultt r0, r1, r2\n smulwb r3, r4, r5", |cpu, _| {
        cpu.regs[1] = 0x0003_DEAD;
        cpu.regs[2] = 0x0005_BEEF;
        cpu.regs[4] = 0x0001_0000;
        cpu.regs[5] = 4;
    }) else {
        return;
    };
    assert_reg!(r, 0, 15);
    assert_reg!(r, 3, 4);
}

#[test]
fn smlalbb_and_smlald_accumulate_into_a_pair() {
    let Some(r) = run_with("smlalbb r0, r1, r4, r5\n smlald r2, r3, r6, r7", |cpu, _| {
        cpu.regs[0] = 0xFFFF_FFFF;
        cpu.regs[4] = 2;
        cpu.regs[5] = 3;
        cpu.regs[2] = 1;
        cpu.regs[6] = 0x0002_0003;
        cpu.regs[7] = 0x0004_0005;
    }) else {
        return;
    };
    assert_reg!(r, 0, 5);
    assert_reg!(r, 1, 1);
    assert_reg!(r, 2, 24);
    assert_reg!(r, 3, 0);
}

#[test]
fn smmla_and_smmls_round_when_asked() {
    let Some(r) = run_with("smmlar r0, r4, r5, r6\n smmls r1, r4, r5, r6", |cpu, _| {
        cpu.regs[4] = 0x4000_0000;
        cpu.regs[5] = 3;
        cpu.regs[6] = 10;
    }) else {
        return;
    };
    // 0x4000_0000 * 3 is 0xC000_0000, which rounds the top word up
    assert_reg!(r, 0, 11);
    // (10 << 32) - 0xC000_0000 keeps 9 on top
    assert_reg!(r, 1, 9);
}

#[test]
fn qadd_saturates_and_sets_q() {
    let Some(r) = run_with("qadd r0, r1, r2", |cpu, _| {
        cpu.regs[1] = 0x7FFF_FFFF;
        cpu.regs[2] = 1;
    }) else {
        return;
    };
    assert_reg!(r, 0, 0x7FFF_FFFF);
    assert_flags!(r, "----Q");
}

#[test]
fn qdadd_doubles_before_adding() {
    let Some(r) = run_with("qdadd r0, r1, r2", |cpu, _| {
        cpu.regs[1] = 5;
        cpu.regs[2] = 10;
    }) else {
        return;
    };
    assert_reg!(r, 0, 25);
}

#[test]
fn clz_counts_leading_zeros() {
    let Some(r) = run_with("clz r0, r1\n clz r2, r3", |cpu, _| cpu.regs[1] = 0xFFFF) else { return };
    assert_reg!(r, 0, 16);
    assert_reg!(r, 2, 32);
}

// media and bit fields

#[test]
fn reversals() {
    let Some(r) = run_with("rev r0, r4\n rev16 r1, r4\n revsh r2, r4\n rbit r3, r4", |cpu, _| {
        cpu.regs[4] = 0x1122_33F0;
    }) else {
        return;
    };
    assert_reg!(r, 0, 0xF033_2211);
    assert_reg!(r, 1, 0x2211_F033);
    assert_reg!(r, 2, 0xFFFF_F033);
    assert_reg!(r, 3, 0x0FCC_4488);
}

#[test]
fn extends_with_rotation_and_accumulate() {
    let Some(r) = run_with(
        "sxtb r0, r4\n uxtb r1, r4\n sxth r2, r4\n uxtah r3, r5, r4, ror #16\n uxtb16 r6, r4\n sxtab16 r7, r5, r4",
        |cpu, _| {
            cpu.regs[4] = 0x1234_80F0;
            cpu.regs[5] = 1;
        },
    ) else {
        return;
    };
    assert_reg!(r, 0, 0xFFFF_FFF0);
    assert_reg!(r, 1, 0xF0);
    assert_reg!(r, 2, 0xFFFF_80F0);
    assert_reg!(r, 3, 0x1235);
    assert_reg!(r, 6, 0x0034_00F0);
    // the bytes 0x34 and 0xF0 sign-extended to halfwords, 1 added below
    assert_reg!(r, 7, 0x0034_FFF1);
}

#[test]
fn pkhbt_and_pkhtb_pick_opposite_halves() {
    let Some(r) = run_with("pkhbt r0, r4, r5\n pkhtb r1, r4, r5", |cpu, _| {
        cpu.regs[4] = 0xAAAA_BBBB;
        cpu.regs[5] = 0xCCCC_DDDD;
    }) else {
        return;
    };
    assert_reg!(r, 0, 0xCCCC_BBBB);
    assert_reg!(r, 1, 0xAAAA_DDDD);
}

#[test]
fn saturations_clamp() {
    let Some(r) =
        run_with("ssat r0, #8, r4\n usat r1, #8, r5\n ssat16 r2, #4, r6\n usat r3, #4, r7, asr #4", |cpu, _| {
            cpu.regs[4] = 1000;
            cpu.regs[5] = 0xFFFF_FFFF;
            cpu.regs[6] = 0x7FFF_FFF0;
            cpu.regs[7] = 0x80;
        })
    else {
        return;
    };
    assert_reg!(r, 0, 127);
    assert_reg!(r, 1, 0);
    // each halfword to 4 bits signed, 7 and -8
    assert_reg!(r, 2, 0x0007_FFF8);
    assert_reg!(r, 3, 8);
    assert_flags!(r, "----Q");
}

#[test]
fn sel_takes_the_ge_flags_of_a_parallel_add() {
    let Some(r) = run_with("uadd8 r0, r4, r5\n sel r1, r6, r7", |cpu, _| {
        cpu.regs[4] = 0x00FF_00FF;
        cpu.regs[5] = 0x0001_0001;
        cpu.regs[6] = 0xAABB_CCDD;
        cpu.regs[7] = 0x1122_3344;
    }) else {
        return;
    };
    // lanes 0 and 2 carry out, GE 0b0101
    assert_reg!(r, 0, 0);
    assert_reg!(r, 1, 0x11BB_33DD);
}

#[test]
fn parallel_halfwords_plain_and_saturating() {
    let Some(r) = run_with("sadd16 r0, r4, r5\n qadd16 r1, r6, r7\n uhsub8 r2, r4, r5", |cpu, _| {
        cpu.regs[4] = 0x0001_0002;
        cpu.regs[5] = 0x0003_0004;
        cpu.regs[6] = 0x7FFF_8000;
        cpu.regs[7] = 0x0001_8000;
    }) else {
        return;
    };
    assert_reg!(r, 0, 0x0004_0006);
    assert_reg!(r, 1, 0x7FFF_8000);
    // halved byte differences: (1-3)/2, (2-4)/2 in the low bytes
    assert_reg!(r, 2, 0x00FF_00FF);
}

#[test]
fn asx_and_sax_cross_the_halves() {
    let Some(r) = run_with("sasx r0, r4, r5\n ssax r1, r4, r5", |cpu, _| {
        cpu.regs[4] = 0x000A_0014;
        cpu.regs[5] = 0x0003_0002;
    }) else {
        return;
    };
    assert_reg!(r, 0, 0x000C_0011);
    assert_reg!(r, 1, 0x0008_0017);
}

#[test]
fn smlad_wraps_and_sets_q_when_it_overflows() {
    let Some(r) = run_with("smlad r0, r4, r5, r6\n smuad r1, r7, r8", |cpu, _| {
        cpu.regs[4] = 0x0001_0001;
        cpu.regs[5] = 0x0001_0001;
        cpu.regs[6] = 0x7FFF_FFFF;
        cpu.regs[7] = 0x0002_0003;
        cpu.regs[8] = 0x0004_0005;
    }) else {
        return;
    };
    assert_reg!(r, 0, 0x8000_0001);
    assert_reg!(r, 1, 23);
    assert_flags!(r, "----Q");
}

#[test]
fn usad8_sums_absolute_differences() {
    let Some(r) = run_with("usad8 r0, r4, r5\n usada8 r1, r4, r5, r6", |cpu, _| {
        cpu.regs[4] = 0x0A14_1E28;
        cpu.regs[5] = 0x0102_0304;
        cpu.regs[6] = 100;
    }) else {
        return;
    };
    assert_reg!(r, 0, 9 + 18 + 27 + 36);
    assert_reg!(r, 1, 190);
}

#[test]
fn bit_fields() {
    let Some(r) =
        run_with("ubfx r0, r4, #4, #8\n sbfx r1, r4, #4, #8\n bfi r2, r5, #8, #4\n bfc r3, #4, #8", |cpu, _| {
            cpu.regs[4] = 0x0000_0F80;
            cpu.regs[5] = 0xFFFF_FFFA;
            cpu.regs[2] = 0x1111_1111;
            cpu.regs[3] = 0xFFFF_FFFF;
        })
    else {
        return;
    };
    assert_reg!(r, 0, 0xF8);
    assert_reg!(r, 1, 0xFFFF_FFF8);
    assert_reg!(r, 2, 0x1111_1A11);
    assert_reg!(r, 3, 0xFFFF_F00F);
}

// loads and stores

#[test]
fn post_indexed_load_writes_the_base_back() {
    let Some(r) = run_with("ldr r0, [r1], #4", |cpu, bus| {
        cpu.regs[1] = DATA_BASE;
        bus.write_u32(DATA_BASE, 0xDEAD_BEEF);
    }) else {
        return;
    };
    assert_reg!(r, 0, 0xDEAD_BEEF);
    assert_reg!(r, 1, DATA_BASE + 4);
}

#[test]
fn scaled_register_offset() {
    let Some(r) = run_with("ldr r0, [r1, r2, lsl #2]", |cpu, bus| {
        cpu.regs[1] = DATA_BASE;
        cpu.regs[2] = 3;
        bus.write_u32(DATA_BASE + 12, 0xCAFE_0000);
    }) else {
        return;
    };
    assert_reg!(r, 0, 0xCAFE_0000);
}

#[test]
fn halfword_and_signed_loads() {
    let Some(r) = run_with("ldrh r0, [r4]\n ldrsh r1, [r4]\n ldrsb r2, [r4]\n ldrb r3, [r4]", |cpu, bus| {
        cpu.regs[4] = DATA_BASE;
        bus.write_u32(DATA_BASE, 0x80F0);
    }) else {
        return;
    };
    assert_reg!(r, 0, 0x80F0);
    assert_reg!(r, 1, 0xFFFF_80F0);
    assert_reg!(r, 2, 0xFFFF_FFF0);
    assert_reg!(r, 3, 0xF0);
}

#[test]
fn literal_loads_take_a_word_aligned_pc() {
    let Some(r) = run("ldr r0, value\n b over\n value: .word 0x600DF00D\n over: nop") else { return };
    assert_reg!(r, 0, 0x600D_F00D);
}

#[test]
fn storing_the_pc_stores_it_plus_eight() {
    let Some(r) = run_with("str pc, [r0]", |cpu, _| cpu.regs[0] = DATA_BASE) else { return };
    assert_eq!(r.bus.read_u32(DATA_BASE), CODE_BASE + 8);
}

#[test]
fn doubleword_round_trip() {
    let Some(r) = run_with("strd r0, r1, [r4]\n ldrd r2, r3, [r4]", |cpu, _| {
        cpu.regs[0] = 0x1111_1111;
        cpu.regs[1] = 0x2222_2222;
        cpu.regs[4] = DATA_BASE;
    }) else {
        return;
    };
    assert_reg!(r, 2, 0x1111_1111);
    assert_reg!(r, 3, 0x2222_2222);
}

#[test]
fn block_transfers_in_every_order() {
    let Some(r) =
        run_with("stmia r4!, {r0, r1, r2}\n ldmdb r4!, {r5, r6, r7}\n stmib r4, {r0}\n ldmda r8, {r9}", |cpu, _| {
            cpu.regs[0] = 0xA;
            cpu.regs[1] = 0xB;
            cpu.regs[2] = 0xC;
            cpu.regs[4] = DATA_BASE;
            cpu.regs[8] = DATA_BASE + 4;
        })
    else {
        return;
    };
    assert_reg!(r, 5, 0xA);
    assert_reg!(r, 6, 0xB);
    assert_reg!(r, 7, 0xC);
    assert_reg!(r, 4, DATA_BASE);
    assert_reg!(r, 9, 0xA);
}

#[test]
fn push_and_pop_round_trip() {
    let Some(r) = run_with("push {r0, r1, r2}\n pop {r3, r4, r5}", |cpu, _| {
        cpu.regs[0] = 1;
        cpu.regs[1] = 2;
        cpu.regs[2] = 3;
    }) else {
        return;
    };
    assert_reg!(r, 3, 1);
    assert_reg!(r, 4, 2);
    assert_reg!(r, 5, 3);
    assert_reg!(r, 13, STACK_TOP);
}

#[test]
fn swap_exchanges_memory_and_register() {
    let Some(r) = run_with("swp r0, r1, [r4]", |cpu, bus| {
        cpu.regs[1] = 0x5555_5555;
        cpu.regs[4] = DATA_BASE;
        bus.write_u32(DATA_BASE, 0xAAAA_AAAA);
    }) else {
        return;
    };
    assert_reg!(r, 0, 0xAAAA_AAAA);
    assert_eq!(r.bus.read_u32(DATA_BASE), 0x5555_5555);
}

// the exclusives and barriers

#[test]
fn strex_succeeds_only_after_ldrex() {
    let Some(r) =
        run_with("strex r0, r5, [r4]\n ldrex r1, [r4]\n strex r2, r6, [r4]\n strex r3, r5, [r4]", |cpu, _| {
            cpu.regs[4] = DATA_BASE;
            cpu.regs[5] = 0x1111_1111;
            cpu.regs[6] = 0x2222_2222;
        })
    else {
        return;
    };
    assert_reg!(r, 0, 1);
    assert_reg!(r, 2, 0);
    // a STREX ends the mark, the next one fails
    assert_reg!(r, 3, 1);
    assert_eq!(r.bus.read_u32(DATA_BASE), 0x2222_2222);
}

/// another core writing between the LDREX and the STREX shows up as memory
/// that no longer holds what LDREX read.
#[test]
fn strex_fails_when_the_word_changed() {
    let Some(r) = run_with(
        "ldrex r1, [r4]\n str r7, [r4]\n strex r2, r6, [r4]\n clrex\n ldrex r3, [r4]\n clrex\n strex r5, r6, [r4]",
        |cpu, _| {
            cpu.regs[4] = DATA_BASE;
            cpu.regs[6] = 0x2222_2222;
            cpu.regs[7] = 0x7777_7777;
        },
    ) else {
        return;
    };
    assert_reg!(r, 2, 1);
    assert_reg!(r, 3, 0x7777_7777);
    // CLREX drops the mark
    assert_reg!(r, 5, 1);
    assert_eq!(r.bus.read_u32(DATA_BASE), 0x7777_7777);
}

#[test]
fn ldrexd_and_strexd_move_a_pair() {
    let Some(r) = run_with(
        "ldrexd r0, r1, [r4]\n strexd r2, r6, r7, [r4]\n ldrexb r8, [r4]\n strexh r9, r6, [r4]",
        |cpu, bus| {
            cpu.regs[4] = DATA_BASE;
            cpu.regs[6] = 0x6666_6666;
            cpu.regs[7] = 0x7777_7777;
            bus.write_u32(DATA_BASE, 0x0102_0304);
            bus.write_u32(DATA_BASE + 4, 0x0506_0708);
        },
    ) else {
        return;
    };
    assert_reg!(r, 0, 0x0102_0304);
    assert_reg!(r, 1, 0x0506_0708);
    assert_reg!(r, 2, 0);
    assert_eq!(r.bus.read_u32(DATA_BASE + 4), 0x7777_7777);
    assert_reg!(r, 8, 0x66);
    // a halfword STREX after a byte LDREX doesn't match its mark
    assert_reg!(r, 9, 1);
}

#[test]
fn barriers_reach_the_bus() {
    let Some(r) = run("dmb ish\n dsb sy\n isb\n mcr p15, 0, r0, c7, c10, 5") else { return };
    assert_eq!(r.bus.barriers, 4);
}

#[test]
fn the_thread_id_registers() {
    let Some(r) =
        run_with("mrc p15, 0, r0, c13, c0, 3\n mcr p15, 0, r1, c13, c0, 2\n mrc p15, 0, r2, c13, c0, 2", |cpu, _| {
            cpu.cp15.thread_id_ro = 0x8100_0800;
            cpu.regs[1] = 0x1234;
        })
    else {
        return;
    };
    assert_reg!(r, 0, 0x8100_0800);
    assert_reg!(r, 2, 0x1234);
}

// branches

#[test]
fn bl_sets_the_link_register() {
    let Some(r) = run("bl target\n b end\n target: mov r0, #1\n end: nop") else { return };
    assert_reg!(r, 0, 1);
    assert_reg!(r, 14, CODE_BASE + 4);
}

#[test]
fn conditional_execution_skips() {
    let Some(r) = run("mov r0, #0\n cmp r0, #0\n movne r1, #1\n moveq r2, #2") else { return };
    assert_reg!(r, 1, 0);
    assert_reg!(r, 2, 2);
}

#[test]
fn svc_gives_its_immediate_with_the_pc_past_it() {
    let Some(r) = run_to_exit("svc #0x32", false, |_, _| {}) else { return };
    assert_eq!(r.exit, Exit::Supervisor(0x32));
    assert_eq!(r.cpu.regs[15], CODE_BASE + 4);
}

#[test]
fn bx_switches_to_thumb() {
    let Some(r) = run_to_exit("adr r0, thumb\n add r0, r0, #1\n bx r0\n .thumb\n thumb: bkpt #2", false, |_, _| {})
    else {
        return;
    };
    assert_eq!(r.exit, Exit::Breakpoint { pc: CODE_BASE + 12, imm: 2 });
}

#[test]
fn loads_of_the_pc_interwork() {
    let target = CODE_BASE + 8;
    let Some(r) = run_to_exit("ldr pc, [r0]\n push {r1}\n .thumb\n bkpt #3", false, |cpu, bus| {
        cpu.regs[0] = DATA_BASE;
        bus.write_u32(DATA_BASE, target | 1);
    }) else {
        return;
    };
    assert_eq!(r.exit, Exit::Breakpoint { pc: target, imm: 3 });
}

// what ARMv7 leaves unpredictable, which the CPU refuses so that recompiled
// code never has to guess what it does

fn undefined(opcode: u32, setup: impl FnOnce(&mut Cpu)) -> bool {
    let mut bus = TestBus::default();
    bus.write_u32(CODE_BASE, opcode);
    let mut cpu = Cpu::new();
    cpu.reset_to(CODE_BASE, STACK_TOP);
    cpu.regs[0] = DATA_BASE;
    setup(&mut cpu);
    matches!(cpu.step(&mut bus), Some(Exit::Undefined { .. }))
}

#[test]
fn the_unpredictable_is_undefined() {
    for (opcode, what) in [
        (0xE1C0_F0D0u32, "ldrd r15, [r0]"),
        (0xE1C0_10F0, "strd r1, [r0], odd register"),
        (0xE1B0_FF9F, "ldrexd r15, [r0]"),
        (0xE8B0_0003, "ldmia r0!, {r0, r1}"),
        (0xE8D0_0002, "ldmia r0, {r1}^"),
        (0xE25E_F004, "subs pc, lr, #4"),
        (0xE1A0_2F11, "mov r2, r1, lsl pc"),
        (0xE710_F110, "sdiv r0, r0, r1, which the Cortex-A9 lacks"),
        (0xEE00_0310, "mcr to coprocessor 3"),
        (0xE7F0_00F0, "udf"),
    ] {
        assert!(undefined(opcode, |_| {}), "{what}, 0x{opcode:08X}");
    }
}
