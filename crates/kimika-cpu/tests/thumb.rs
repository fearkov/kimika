//! Thumb state, the 16-bit instructions and Thumb-2's 32-bit ones.

mod common;

use common::{
    assert_flags, assert_reg, run_thumb, run_thumb_with, run_to_exit, TestBus, CODE_BASE, DATA_BASE, STACK_TOP,
};
use kimika_cpu::{Cpu, Exit};

// the 16-bit instructions

#[test]
fn moves_adds_and_compares() {
    let Some(r) = run_thumb("movs r0, #200\n adds r1, r0, #5\n subs r2, r1, r0\n cmp r2, #5") else { return };
    assert_reg!(r, 0, 200);
    assert_reg!(r, 1, 205);
    assert_reg!(r, 2, 5);
    assert_flags!(r, "-ZC");
}

#[test]
fn shifts_set_the_carry() {
    let Some(r) = run_thumb_with("lsls r1, r0, #1\n lsrs r2, r0, #32", |cpu, _| cpu.regs[0] = 0x8000_0001) else {
        return;
    };
    assert_reg!(r, 1, 2);
    assert_reg!(r, 2, 0);
    assert_flags!(r, "-ZC");
}

#[test]
fn the_low_register_operations() {
    let Some(r) = run_thumb_with(
        "ands r0, r4\n eors r1, r4\n orrs r2, r4\n bics r3, r4\n mvns r5, r4\n negs r6, r4\n muls r7, r4",
        |cpu, _| {
            cpu.regs[0] = 0xFF;
            cpu.regs[1] = 0xFF;
            cpu.regs[2] = 0x100;
            cpu.regs[3] = 0xFF;
            cpu.regs[4] = 0x0F;
            cpu.regs[7] = 3;
        },
    ) else {
        return;
    };
    assert_reg!(r, 0, 0x0F);
    assert_reg!(r, 1, 0xF0);
    assert_reg!(r, 2, 0x10F);
    assert_reg!(r, 3, 0xF0);
    assert_reg!(r, 5, 0xFFFF_FFF0);
    assert_reg!(r, 6, 0xFFFF_FFF1);
    assert_reg!(r, 7, 45);
}

#[test]
fn high_registers_move_add_and_compare() {
    let Some(r) = run_thumb_with("mov r9, r1\n add r9, r9\n cmp r9, r1\n mov r2, sp", |cpu, _| cpu.regs[1] = 7) else {
        return;
    };
    assert_reg!(r, 9, 14);
    assert_reg!(r, 2, STACK_TOP);
    assert_flags!(r, "--C");
}

#[test]
fn loads_and_stores_of_every_size() {
    let Some(r) = run_thumb_with(
        "str r1, [r0, #4]\n strh r1, [r0, #8]\n strb r1, [r0, #12]\n ldr r2, [r0, #4]\n ldrh r3, [r0, #8]\n
         ldrb r4, [r0, #12]\n movs r5, #4\n ldrsh r6, [r0, r5]\n ldrsb r7, [r0, r5]",
        |cpu, _| {
            cpu.regs[0] = DATA_BASE;
            cpu.regs[1] = 0x8899_AABB;
        },
    ) else {
        return;
    };
    assert_reg!(r, 2, 0x8899_AABB);
    assert_reg!(r, 3, 0xAABB);
    assert_reg!(r, 4, 0xBB);
    assert_reg!(r, 6, 0xFFFF_AABB);
    assert_reg!(r, 7, 0xFFFF_FFBB);
}

#[test]
fn literals_and_adr_take_a_word_aligned_pc() {
    // the load sits at an odd halfword, so its PC is not word aligned
    let Some(r) =
        run_thumb("nop\n ldr r0, value\n adr r1, value\n b over\n .align 2\n value: .word 0x600DF00D\n over: nop")
    else {
        return;
    };
    assert_reg!(r, 0, 0x600D_F00D);
    assert_eq!(r.r(1) % 4, 0);
}

#[test]
fn push_and_pop_with_lr_and_pc() {
    let Some(r) =
        run_thumb("bl function\n b end\n function: push {r4, lr}\n movs r4, #9\n mov r0, r4\n pop {r4, pc}\n end: nop")
    else {
        return;
    };
    assert_reg!(r, 0, 9);
    assert_reg!(r, 4, 0);
    assert_reg!(r, 13, STACK_TOP);
}

#[test]
fn stm_and_ldm_write_back() {
    let Some(r) = run_thumb_with("stmia r0!, {r1, r2}\n subs r0, #8\n ldmia r0, {r3, r4}", |cpu, _| {
        cpu.regs[0] = DATA_BASE;
        cpu.regs[1] = 11;
        cpu.regs[2] = 22;
    }) else {
        return;
    };
    assert_reg!(r, 3, 11);
    assert_reg!(r, 4, 22);
}

#[test]
fn cbz_and_cbnz() {
    let Some(r) = run_thumb("movs r0, #0\n cbz r0, zero\n movs r1, #1\n zero: cbnz r0, end\n movs r2, #2\n end: nop")
    else {
        return;
    };
    assert_reg!(r, 1, 0);
    assert_reg!(r, 2, 2);
}

#[test]
fn conditional_branches() {
    let Some(r) = run_thumb("movs r0, #3\n loop: subs r0, #1\n add r1, r1, #2\n bne loop") else { return };
    assert_reg!(r, 0, 0);
    assert_reg!(r, 1, 6);
}

// IT blocks

#[test]
fn an_it_block_runs_its_then_and_else() {
    let Some(r) = run_thumb("cmp r0, #0\n ite eq\n moveq r1, #1\n movne r2, #2\n movs r3, #3") else { return };
    assert_reg!(r, 1, 1);
    assert_reg!(r, 2, 0);
    assert_reg!(r, 3, 3);
    assert_eq!(r.cpu.cpsr.it, 0, "the block is over");
}

#[test]
fn inside_an_it_block_the_short_alu_ops_leave_the_flags() {
    // adds outside a block sets flags, add inside one does not
    let Some(r) = run_thumb_with("cmp r0, r0\n itt eq\n addeq r1, r1, r2\n addeq r3, r1, r2", |cpu, _| {
        cpu.regs[1] = 0xFFFF_FFFF;
        cpu.regs[2] = 1;
    }) else {
        return;
    };
    assert_reg!(r, 1, 0);
    assert_reg!(r, 3, 1);
    // still the flags of the cmp
    assert_flags!(r, "-ZC");
}

#[test]
fn a_four_instruction_block_with_both_conditions() {
    let Some(r) =
        run_thumb("movs r0, #5\n cmp r0, #4\n itete gt\n movgt r1, #1\n movle r2, #2\n movgt r3, #3\n movle r4, #4")
    else {
        return;
    };
    assert_reg!(r, 1, 1);
    assert_reg!(r, 2, 0);
    assert_reg!(r, 3, 3);
    assert_reg!(r, 4, 0);
}

#[test]
fn a_branch_may_end_an_it_block() {
    let Some(r) = run_thumb("movs r0, #1\n cmp r0, #1\n it eq\n beq.w skip\n movs r1, #1\n skip: movs r2, #2") else {
        return;
    };
    assert_reg!(r, 1, 0);
    assert_reg!(r, 2, 2);
}

// Thumb-2

#[test]
fn modified_immediates_in_every_pattern() {
    let Some(r) = run_thumb(
        "mov.w r0, #0x00AB00AB\n mov.w r1, #0xAB00AB00\n mov.w r2, #0xABABABAB\n mov.w r3, #0x3FC00\n orr r4, r3, #1",
    ) else {
        return;
    };
    assert_reg!(r, 0, 0x00AB_00AB);
    assert_reg!(r, 1, 0xAB00_AB00);
    assert_reg!(r, 2, 0xABAB_ABAB);
    assert_reg!(r, 3, 0x3FC00);
    assert_reg!(r, 4, 0x3FC01);
}

#[test]
fn movw_movt_addw_subw() {
    let Some(r) = run_thumb("movw r0, #0x5678\n movt r0, #0x1234\n addw r1, r0, #0xFFF\n subw r2, r0, #8") else {
        return;
    };
    assert_reg!(r, 0, 0x1234_5678);
    assert_reg!(r, 1, 0x1234_6677);
    assert_reg!(r, 2, 0x1234_5670);
}

#[test]
fn shifted_registers_and_comparisons() {
    let Some(r) = run_thumb_with(
        "add.w r2, r0, r1, lsl #4\n sub.w r3, r0, r1, asr #1\n rsb r4, r1, r0, ror #8\n orn r5, r0, r1\n tst.w r0, r1, lsl #31\n teq.w r0, r0",
        |cpu, _| {
            cpu.regs[0] = 0x100;
            cpu.regs[1] = 0xFFFF_FFF1;
        },
    ) else {
        return;
    };
    assert_reg!(r, 2, 0x100u32.wrapping_add(0xFFFF_FF10));
    assert_reg!(r, 3, 0x108);
    // 0x100 rotated right by 8 is 1
    assert_reg!(r, 4, 1u32.wrapping_sub(0xFFFF_FFF1));
    assert_reg!(r, 5, 0x10E);
    assert_flags!(r, "-Z");
}

#[test]
fn flag_setting_wide_arithmetic() {
    let Some(r) =
        run_thumb_with("adds.w r2, r0, r1\n adcs.w r3, r0, r1\n sbcs.w r4, r0, r1\n cmn.w r0, r1", |cpu, _| {
            cpu.regs[0] = 0xFFFF_FFFF;
            cpu.regs[1] = 1;
        })
    else {
        return;
    };
    assert_reg!(r, 2, 0);
    assert_reg!(r, 3, 1);
    assert_reg!(r, 4, 0xFFFF_FFFE);
    assert_flags!(r, "-ZC");
}

#[test]
fn register_shifts_and_rotates() {
    let Some(r) = run_thumb_with("lsl.w r2, r0, r1\n lsrs.w r3, r0, r1\n ror.w r4, r0, r1", |cpu, _| {
        cpu.regs[0] = 0x8000_0010;
        cpu.regs[1] = 4;
    }) else {
        return;
    };
    assert_reg!(r, 2, 0x100);
    assert_reg!(r, 3, 0x0800_0001);
    assert_reg!(r, 4, 0x0800_0001);
    assert_flags!(r, "--");
}

#[test]
fn bit_fields_and_saturations() {
    let Some(r) = run_thumb_with(
        "ubfx r1, r0, #4, #8\n sbfx r2, r0, #4, #8\n bfi r3, r0, #8, #4\n bfc r4, #0, #16\n ssat r5, #8, r6\n usat r7, #8, r6, asr #4",
        |cpu, _| {
            cpu.regs[0] = 0x0000_0F8A;
            cpu.regs[3] = 0x1111_1111;
            cpu.regs[4] = 0xFFFF_FFFF;
            cpu.regs[6] = 0x7FFF;
        },
    ) else {
        return;
    };
    assert_reg!(r, 1, 0xF8);
    assert_reg!(r, 2, 0xFFFF_FFF8);
    assert_reg!(r, 3, 0x1111_1A11);
    assert_reg!(r, 4, 0xFFFF_0000);
    assert_reg!(r, 5, 127);
    assert_reg!(r, 7, 255);
    assert_flags!(r, "----Q");
}

#[test]
fn wide_multiplies() {
    let Some(r) = run_thumb_with(
        "mul r2, r0, r1\n mla r3, r0, r1, r2\n mls r4, r0, r1, r3\n smull r5, r6, r0, r7\n umlal r8, r9, r0, r1\n smlabb r10, r0, r1, r2",
        |cpu, _| {
            cpu.regs[0] = 6;
            cpu.regs[1] = 7;
            cpu.regs[7] = 0xFFFF_FFFF;
            cpu.regs[8] = 0xFFFF_FFFF;
        },
    ) else {
        return;
    };
    assert_reg!(r, 2, 42);
    assert_reg!(r, 3, 84);
    assert_reg!(r, 4, 42);
    assert_reg!(r, 5, 0xFFFF_FFFA);
    assert_reg!(r, 6, 0xFFFF_FFFF);
    assert_reg!(r, 8, 41);
    assert_reg!(r, 9, 1);
    assert_reg!(r, 10, 84);
}

#[test]
fn wide_misc_operations() {
    let Some(r) = run_thumb_with(
        "rev.w r1, r0\n rbit r2, r0\n clz r3, r0\n uxth.w r4, r0, ror #8\n sxtab r5, r6, r0\n qadd r7, r0, r0\n sel r8, r0, r6",
        |cpu, _| {
            cpu.regs[0] = 0x1122_33F0;
            cpu.regs[6] = 0x10;
            cpu.cpsr.ge = 0b0011;
        },
    ) else {
        return;
    };
    assert_reg!(r, 1, 0xF033_2211);
    assert_reg!(r, 2, 0x0FCC_4488);
    assert_reg!(r, 3, 3);
    assert_reg!(r, 4, 0x2233);
    assert_reg!(r, 5, 0);
    assert_reg!(r, 7, 0x2244_67E0);
    assert_reg!(r, 8, 0x0000_33F0);
}

#[test]
fn wide_loads_and_stores_in_every_addressing_mode() {
    let Some(r) = run_thumb_with(
        "str.w r1, [r0, #0x100]\n ldr r2, [r0, #0x100]\n str r1, [r0, #4]!\n ldr r3, [r0], #-4\n
         ldr.w r4, [r0, r5, lsl #2]\n ldrsh.w r6, [r0, #0x100]\n strb r1, [r0, #-1]\n ldrb r7, [r0, #-1]",
        |cpu, bus| {
            cpu.regs[0] = DATA_BASE + 0x10;
            cpu.regs[1] = 0x8899_AABB;
            cpu.regs[5] = 1;
            bus.write_u32(DATA_BASE + 0x14, 0);
        },
    ) else {
        return;
    };
    assert_reg!(r, 2, 0x8899_AABB);
    assert_reg!(r, 3, 0x8899_AABB);
    assert_reg!(r, 0, DATA_BASE + 0x10);
    assert_reg!(r, 4, 0x8899_AABB);
    assert_reg!(r, 6, 0xFFFF_AABB);
    assert_reg!(r, 7, 0xBB);
}

#[test]
fn ldrd_strd_and_a_literal_pair() {
    let Some(r) = run_thumb_with(
        "strd r1, r2, [r0, #8]\n ldrd r3, r4, [r0, #8]\n ldrd r5, r6, pair\n b over\n .align 3\n pair: .word 5, 6\n over: nop",
        |cpu, _| {
            cpu.regs[0] = DATA_BASE;
            cpu.regs[1] = 1;
            cpu.regs[2] = 2;
        },
    ) else {
        return;
    };
    assert_reg!(r, 3, 1);
    assert_reg!(r, 4, 2);
    assert_reg!(r, 5, 5);
    assert_reg!(r, 6, 6);
}

#[test]
fn push_w_and_pop_w() {
    let Some(r) =
        run_thumb_with("push.w {r1, r8}\n pop.w {r2, r9}\n stmdb r0!, {r1, r2}\n ldmia.w r0!, {r3, r4}", |cpu, _| {
            cpu.regs[0] = DATA_BASE + 0x20;
            cpu.regs[1] = 1;
            cpu.regs[8] = 8;
        })
    else {
        return;
    };
    assert_reg!(r, 2, 1);
    assert_reg!(r, 9, 8);
    assert_reg!(r, 3, 1);
    assert_reg!(r, 4, 1);
    assert_reg!(r, 0, DATA_BASE + 0x20);
}

#[test]
fn table_branches() {
    let Some(r) = run_thumb_with(
        "tbb [pc, r0]\n bytes: .byte (one - bytes) / 2, (two - bytes) / 2\n
         one: movs r1, #1\n b next\n two: movs r1, #2\n
         next: tbh [pc, r2, lsl #1]\n halves: .hword (three - halves) / 2, (four - halves) / 2\n
         three: movs r3, #3\n b end\n four: movs r3, #4\n end: nop",
        |cpu, _| {
            cpu.regs[0] = 1;
            cpu.regs[2] = 0;
        },
    ) else {
        return;
    };
    assert_reg!(r, 1, 2);
    assert_reg!(r, 3, 3);
}

#[test]
fn bl_and_blx_between_the_states() {
    let Some(r) = run_thumb(
        "bl thumb_function\n blx arm_function\n b end\n
         thumb_function: movs r0, #1\n bx lr\n
         .arm\n .align 2\n arm_function: mov r1, #2\n bx lr\n
         .thumb\n end: nop",
    ) else {
        return;
    };
    assert_reg!(r, 0, 1);
    assert_reg!(r, 1, 2);
    assert!(r.cpu.cpsr.thumb);
}

#[test]
fn exclusives_from_thumb() {
    let Some(r) = run_thumb_with(
        "ldrex r1, [r0]\n adds r1, #1\n strex r2, r1, [r0]\n ldrexh r3, [r0]\n strexh r4, r5, [r0]\n ldrexd r6, r7, [r0]\n clrex\n strexd r8, r6, r7, [r0]",
        |cpu, bus| {
            cpu.regs[0] = DATA_BASE;
            cpu.regs[5] = 0xBEEF;
            bus.write_u32(DATA_BASE, 41);
        },
    ) else {
        return;
    };
    assert_reg!(r, 2, 0);
    assert_reg!(r, 3, 42);
    assert_reg!(r, 4, 0);
    assert_reg!(r, 6, 0xBEEF);
    assert_reg!(r, 8, 1);
}

#[test]
fn the_status_register_and_barriers() {
    let Some(r) = run_thumb_with("msr apsr_nzcvq, r0\n mrs r1, apsr\n dmb ish", |cpu, _| cpu.regs[0] = 0xF800_0000)
    else {
        return;
    };
    assert_flags!(r, "NZCVQ");
    // user mode, and the T bit reads as zero
    assert_reg!(r, 1, 0xF800_0010);
    assert_eq!(r.bus.barriers, 1);
}

#[test]
fn svc_and_bkpt() {
    let Some(r) = run_to_exit("svc #7", true, |_, _| {}) else { return };
    assert_eq!(r.exit, Exit::Supervisor(7));
    assert_eq!(r.cpu.regs[15], CODE_BASE + 2);
}

// what Thumb-2 leaves unpredictable, which the CPU refuses

fn undefined(halfwords: &[u16], setup: impl FnOnce(&mut Cpu)) -> bool {
    let mut bus = TestBus::default();
    for (i, &halfword) in halfwords.iter().enumerate() {
        bus.write_bytes(CODE_BASE + 2 * i as u32, &halfword.to_le_bytes());
    }
    let mut cpu = Cpu::new();
    cpu.reset_to(CODE_BASE | 1, STACK_TOP);
    cpu.regs[0] = DATA_BASE;
    setup(&mut cpu);
    for _ in 0..halfwords.len() {
        match cpu.step(&mut bus) {
            Some(Exit::Undefined { .. }) => return true,
            Some(_) => return false,
            None => {}
        }
    }
    false
}

#[test]
fn the_unpredictable_is_undefined() {
    // Z is set, so that the blocks' EQ instructions run rather than being
    // skipped for their condition
    let cases: [(&[u16], &str); 7] = [
        (&[0xBF08, 0xBF08], "an IT inside an IT block"),
        (&[0xBF08, 0xD000], "a conditional branch inside an IT block"),
        (&[0xBF0C, 0xE7FE, 0xBF00], "a B that does not end its IT block"),
        (&[0xE890, 0x2006], "ldm.w r0, {r1, r2, sp}"),
        (&[0xE8B0, 0x0003], "ldmia.w r0!, {r0, r1}"),
        (&[0xFB90, 0xF0F1], "sdiv r0, r0, r1, which the Cortex-A9 lacks"),
        (&[0xDE00], "udf"),
    ];
    for (halfwords, what) in cases {
        assert!(undefined(halfwords, |cpu| cpu.cpsr.z = true), "{what}, {halfwords:04X?}");
    }
}
