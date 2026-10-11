//! differential testing against QEMU: random instructions of a class run in
//! a small Linux program under qemu-arm and in the interpreter, each from
//! the same registers and memory, and every case that ends differently is
//! printed.
//!
//!     cargo run --release --example oracle -- <class> [cases] [seed] [mask value]
//!
//! the classes are neon, neon-memory and vfp, each also with -thumb after it
//! for Thumb state, and neon-alias, NEON with m the same register as d. a
//! mask and a value in hex fix those bits of every opcode, in its ARM
//! encoding. qemu-arm is QEMU_ARM or on the path, and llvm-mc and ld.lld
//! assemble and link the program.
//!
//!     cargo run --release --example oracle -- source[-thumb] "<asm; asm>" [d0=0x.. r1=buffer ..]
//!
//! runs a piece of assembly from the registers given under both, and prints
//! what changed.
//!
//! an instruction the CPU refuses raises SIGILL under QEMU, which the
//! program catches and notes, and Exit::Undefined here, which this notes the
//! same way. FPSCR's cumulative exception flags are left out of the
//! comparison, as the interpreter does not keep them.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;

use kimika_cpu::{Bus, Cpu, Exit};

const TEXT: u32 = 0x0001_0000;
const DATA: u32 = 0x0040_0000;
/// r0-r11, APSR, FPSCR and d0-d31.
const STATE_WORDS: usize = 78;
/// a case's input state, then its status and output state, then its buffer
/// at 640, in 704 bytes, a multiple of 64.
const CASE_SIZE: u32 = 704;
const OUTPUT: u32 = 312;
const BUFFER: u32 = 640;
/// the data starts with the resume address and the sigaction.
const HEADER: u32 = 64;

const RAN: u32 = 1;
const SIGILL: u32 = 0x104;
const SIGTRAP: u32 = 0x105;
/// NZCVQ and GE.
const APSR_MASK: u32 = 0xF80F_0000;
/// all but the cumulative exception flags, IOC to IXC and IDC.
const FPSCR_MASK: u32 = !0x9F;

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }

    fn word(&mut self) -> u32 {
        self.next() as u32
    }
}

/// words that find edges: zeros, ones, signs, the extremes of each lane
/// size, and the floats ARM treats specially.
const SPECIAL: &[u32] = &[
    0x0000_0000,
    0x8000_0000,
    0xFFFF_FFFF,
    0x7FFF_FFFF,
    0x0000_0001,
    0x7FFF_8000,
    0x8000_7FFF,
    0x7F80_807F,
    0x8080_8080,
    0x7F7F_7F7F,
    0x0101_0101,
    0x3F80_0000,
    0xBF80_0000,
    0x4000_0000,
    0x7F80_0000,
    0xFF80_0000,
    0x7FC0_0000,
    0xFFC1_2345,
    0x7F80_0001,
    0xFFA0_0000,
    0x0000_0003,
    0x807F_FFFF,
    0x0080_0000,
    0x8080_0000,
    0x7F7F_FFFF,
    0x4B00_0000,
    0x4F00_0000,
    0xCF00_0000,
    0x5F00_0000,
    0x3EAA_AAAB,
    0x7E80_0000,
    0x0100_0000,
];

fn interesting(rng: &mut Rng) -> u32 {
    match rng.below(10) {
        0..=3 => rng.word(),
        4..=7 => SPECIAL[rng.below(SPECIAL.len() as u64) as usize],
        // a small number, by sign
        8 => rng.below(64) as u32,
        _ => (rng.below(64) as u32).wrapping_neg(),
    }
}

struct Case {
    opcode: u32,
    /// an IT that makes a Thumb instruction conditional, before it.
    it: Option<u32>,
    /// assembly to run in place of the opcode.
    source: Option<String>,
    state: [u32; STATE_WORDS],
    buffer: [u8; 64],
}

fn case_base(index: usize) -> u32 {
    DATA + HEADER + index as u32 * CASE_SIZE
}

fn generate(class: &str, index: usize, rng: &mut Rng, pattern: (u32, u32)) -> Case {
    let mut state = [0; STATE_WORDS];
    for word in state.iter_mut() {
        *word = interesting(rng);
    }
    // APSR: NZCVQ and GE; FPSCR: the flags, QC, DN, FZ and the rounding
    // mode, but no short vectors and no exceptions yet
    state[12] &= APSR_MASK;
    state[13] &= 0xFBC0_0000;
    let mut buffer = [0; 64];
    for byte in buffer.iter_mut() {
        *byte = interesting(rng) as u8;
    }
    let opcode = match class {
        "neon" | "neon-alias" => {
            // half the time the registers even, so that Q forms can run
            let even = if rng.below(2) == 0 { !0x0001_1001 } else { u32::MAX };
            let op = (0xF200_0000 | (rng.word() & 0x01FF_FFFF & even)) & !pattern.0 | pattern.1;
            if class == "neon-alias" {
                // M:Vm from D:Vd
                op & !0x2F | (op >> 12 & 0xF) | (op >> 22 & 1) << 5
            } else {
                op
            }
        }
        "neon-memory" => {
            // Rn one of r0-r11 holding the buffer's address, Rm none, SP for
            // the size, or one of the others
            let n = rng.below(12) as u32;
            let m = match rng.below(3) {
                0 => 13,
                1 => 15,
                _ => rng.below(12) as u32,
            };
            state[n as usize] = case_base(index) + BUFFER;
            let op = 0xF400_0000 | (rng.word() & 0x00EF_FFFF & !0x000F_000F) | n << 16 | m;
            op & !(pattern.0 & !0x000F_000F) | (pattern.1 & !0x000F_000F)
        }
        "vfp" => (0xEE00_0A00 | (rng.word() & 0x00FF_F1EF)) & !pattern.0 | pattern.1,
        "arm" => arm_integer(rng) & !pattern.0 | pattern.1,
        "thumb" => thumb_integer(rng),
        _ => panic!("no class {class}: neon, neon-memory and vfp, each with -thumb or not"),
    };
    // a third of the Thumb instructions in an IT block of their own
    let it = (class == "thumb" && rng.below(3) == 0).then(|| 0xBF08 | (rng.below(15) as u32) << 4);
    Case { opcode, it, source: None, state, buffer }
}

/// a register an instruction writes, r0-r11: never r12, which the program
/// keeps its place in, SP or the PC.
fn written(rng: &mut Rng) -> u32 {
    rng.below(12) as u32
}

/// a register an instruction reads, r0-r12: never SP, which differs between
/// the two, nor the PC, which most instructions can't take.
fn read(rng: &mut Rng) -> u32 {
    rng.below(13) as u32
}

/// the low n bits of a random word.
fn field(rng: &mut Rng, n: u32) -> u32 {
    rng.word() & ((1 << n) - 1)
}

fn pick(rng: &mut Rng, choices: &[u32]) -> u32 {
    choices[rng.below(choices.len() as u64) as usize]
}

/// what a field meant to be zero holds: zero, but now and then not, to see
/// that both refuse it.
fn zero(rng: &mut Rng) -> u32 {
    if rng.below(8) == 0 {
        rng.below(12) as u32
    } else {
        0
    }
}

/// an ARM instruction that computes in registers and flags, never touching
/// memory or the PC, mostly with the condition always.
fn arm_integer(rng: &mut Rng) -> u32 {
    let cond = if rng.below(2) == 0 { 0xE } else { rng.below(15) as u32 };
    let body = match rng.below(16) {
        // data processing with an immediate or a register shifted by one,
        // which may be the PC. TST, TEQ, CMP and CMN always set flags and
        // have no destination, and MOV and MVN have no first operand
        0..=5 => {
            let opcode = field(rng, 4);
            let compare = (8..=11).contains(&opcode);
            let s = if compare { 1 } else { field(rng, 1) };
            let d = if compare { zero(rng) } else { written(rng) };
            let n = match opcode {
                13 | 15 => zero(rng),
                _ if rng.below(8) == 0 => 15,
                _ => read(rng),
            };
            let operand = match rng.below(3) {
                0 => 1 << 25 | field(rng, 12),
                1 => {
                    let m = if rng.below(8) == 0 { 15 } else { read(rng) };
                    field(rng, 7) << 5 | m
                }
                // shifted by a register, where nothing may be the PC
                _ => {
                    let n = if opcode == 13 || opcode == 15 { n } else { read(rng) };
                    return cond << 28
                        | opcode << 21
                        | s << 20
                        | n << 16
                        | d << 12
                        | read(rng) << 8
                        | field(rng, 2) << 5
                        | 0x10
                        | read(rng);
                }
            };
            opcode << 21 | s << 20 | n << 16 | d << 12 | operand
        }
        // MUL, MLA, UMAAL, MLS, UMULL, UMLAL, SMULL and SMLAL, and the two
        // encodings between them that are undefined. MUL has no accumulator
        6 => {
            let kind = field(rng, 4);
            let high = written(rng);
            let low = if kind < 2 { zero(rng) } else { (high + 1 + rng.below(11) as u32) % 12 };
            kind << 20 | high << 16 | low << 12 | read(rng) << 8 | 0x90 | read(rng)
        }
        // SMLAxy, SMLAWy, SMULWy, SMLALxy and SMULxy, the saturating adds
        // and subtracts, and CLZ
        7 => match rng.below(3) {
            0 => {
                let (kind, x) = (field(rng, 2), field(rng, 1));
                let high = written(rng);
                // SMUL<x><y> and SMULW<y> have no accumulator
                let plain = kind == 3 || (kind == 1 && x == 1);
                let low = if plain { zero(rng) } else { (high + 1 + rng.below(11) as u32) % 12 };
                0x0100_0080
                    | kind << 21
                    | high << 16
                    | low << 12
                    | read(rng) << 8
                    | field(rng, 1) << 6
                    | x << 5
                    | read(rng)
            }
            1 => 0x0100_0050 | field(rng, 2) << 21 | read(rng) << 16 | written(rng) << 12 | read(rng),
            _ => 0x016F_0F10 | written(rng) << 12 | read(rng),
        },
        // MRS, and MSR of the flags and GE from a register or an immediate
        8 => match rng.below(3) {
            0 => 0x010F_0000 | written(rng) << 12,
            1 => 0x0120_F000 | (1 + rng.below(3) as u32) << 18 | read(rng),
            _ => 0x0320_F000 | (1 + rng.below(3) as u32) << 18 | field(rng, 12),
        },
        // MOVW and MOVT
        9 => 0x0300_0000 | field(rng, 1) << 22 | field(rng, 4) << 16 | written(rng) << 12 | field(rng, 12),
        // the parallel adds and subtracts, signed, saturating, halving and
        // their unsigned forms
        10 => {
            let kind = pick(rng, &[1, 2, 3, 5, 6, 7]);
            let operation = pick(rng, &[0, 1, 2, 3, 4, 7]);
            0x0600_0F10 | kind << 20 | read(rng) << 16 | written(rng) << 12 | operation << 5 | read(rng)
        }
        // packing, saturating, extending, reversing and SEL
        11 => {
            let (d, m) = (written(rng), read(rng));
            // the extends without an addend have 1111 for n
            let n = if rng.below(3) == 0 { 15 } else { read(rng) };
            match rng.below(10) {
                0 => 0x0680_0010 | n.min(12) << 16 | d << 12 | field(rng, 5) << 7 | field(rng, 1) << 6 | m,
                1 => pick(rng, &[0x06A0_0010, 0x06E0_0010]) | field(rng, 5) << 16 | d << 12 | field(rng, 6) << 6 | m,
                2 => pick(rng, &[0x06A0_0F30, 0x06E0_0F30]) | field(rng, 4) << 16 | d << 12 | m,
                3..=6 => {
                    let extend =
                        pick(rng, &[0x0680_0070, 0x06A0_0070, 0x06B0_0070, 0x06C0_0070, 0x06E0_0070, 0x06F0_0070]);
                    extend | n << 16 | d << 12 | field(rng, 2) << 10 | m
                }
                7 => 0x0680_0FB0 | read(rng) << 16 | d << 12 | m,
                _ => pick(rng, &[0x06BF_0F30, 0x06BF_0FB0, 0x06FF_0F30, 0x06FF_0FB0]) | d << 12 | m,
            }
        }
        // the signed multiplies, USAD8 and USADA8, and the divides the A9
        // does not have. SMMLS always accumulates
        12 => {
            let (d, n, m) = (written(rng), read(rng), read(rng));
            let a = if rng.below(3) == 0 { 15 } else { read(rng) };
            match rng.below(7) {
                0 => pick(rng, &[0x0700_0010, 0x0700_0050]) | d << 16 | a << 12 | m << 8 | field(rng, 1) << 5 | n,
                1 => {
                    let low = (d + 1 + rng.below(11) as u32) % 12;
                    pick(rng, &[0x0740_0010, 0x0740_0050]) | d << 16 | low << 12 | m << 8 | field(rng, 1) << 5 | n
                }
                2 => 0x0750_0010 | d << 16 | a << 12 | m << 8 | field(rng, 1) << 5 | n,
                3 => 0x0750_00D0 | d << 16 | read(rng) << 12 | m << 8 | field(rng, 1) << 5 | n,
                4 => 0x0780_0010 | d << 16 | a << 12 | m << 8 | n,
                _ => pick(rng, &[0x0710_F010, 0x0730_F010]) | d << 16 | m << 8 | n,
            }
        }
        // SBFX, UBFX, BFC and BFI, some of them past the register's end
        13 | 14 => {
            let (d, n) = (written(rng), read(rng));
            match rng.below(3) {
                0 => pick(rng, &[0x07A0_0050, 0x07E0_0050]) | field(rng, 5) << 16 | d << 12 | field(rng, 5) << 7 | n,
                1 => 0x07C0_001F | field(rng, 5) << 16 | d << 12 | field(rng, 5) << 7,
                _ => 0x07C0_0010 | field(rng, 5) << 16 | d << 12 | field(rng, 5) << 7 | n,
            }
        }
        // the hints, NOP, YIELD, WFE and SEV, and UDF
        _ => match rng.below(2) {
            0 => 0x0320_F000 | pick(rng, &[0, 1, 2, 4]),
            _ => 0x07F0_00F0 | field(rng, 12) << 8 | field(rng, 4),
        },
    };
    cond << 28 | body
}

/// a Thumb instruction of the same kinds, a halfword below 0x10000 or two
/// with the first on top.
fn thumb_integer(rng: &mut Rng) -> u32 {
    let thumb32 = |first: u32, second: u32| first << 16 | second;
    match rng.below(16) {
        // shifts, adds, subtracts, moves and compares of the low registers,
        // and the register data processing
        0 => field(rng, 14),
        1 => 0x4000 | field(rng, 10),
        // ADD, CMP and MOV of any register but SP, writing r0-r11; CMP
        // takes this form for a high register, one at least, and not the PC
        2 => {
            let kind = pick(rng, &[0, 1, 2]);
            let mut d = written(rng);
            let m = if kind != 1 && rng.below(8) == 0 { 15 } else { read(rng) };
            if kind == 1 && d < 8 && m < 8 {
                d = pick(rng, &[8, 9, 10, 11, 12]);
            }
            0x4400 | kind << 8 | (d >> 3) << 7 | m << 3 | (d & 7)
        }
        // LDR of a literal, ADR, the extends, the reversals and the hints
        3 => match rng.below(5) {
            0 => 0x4800 | field(rng, 11),
            1 => 0xA000 | field(rng, 11),
            2 => 0xB200 | field(rng, 8),
            3 => 0xBA00 | pick(rng, &[0, 1, 3]) << 6 | field(rng, 6),
            _ => 0xBF00 | pick(rng, &[0, 1, 2, 4]) << 4,
        },
        // data processing with a modified immediate or a shifted register:
        // Rd 1111 with S makes TST, TEQ, CMN and CMP, Rn 1111 makes ORR and
        // ORN MOV and MVN, and nothing else takes the PC
        4..=8 => {
            let shifted = rng.below(2) == 0;
            let opcode = if shifted {
                pick(rng, &[0, 1, 2, 3, 4, 6, 8, 10, 11, 13, 14])
            } else {
                pick(rng, &[0, 1, 2, 3, 4, 8, 10, 11, 13, 14])
            };
            let n = if (opcode == 2 || opcode == 3) && rng.below(2) == 0 { 15 } else { read(rng) };
            let compare = [0, 4, 8, 13].contains(&opcode) && rng.below(4) == 0;
            let (d, s) = if compare { (15, 1) } else { (written(rng), field(rng, 1)) };
            if shifted {
                let shift = field(rng, 3) << 12 | field(rng, 4) << 4;
                thumb32(0xEA00 | opcode << 5 | s << 4 | n, shift | d << 8 | read(rng))
            } else {
                thumb32(
                    0xF000 | field(rng, 1) << 10 | opcode << 5 | s << 4 | n,
                    field(rng, 3) << 12 | d << 8 | field(rng, 8),
                )
            }
        }
        // with a plain immediate: ADDW, ADR, MOVW, SUBW, MOVT, SSAT, SSAT16,
        // SBFX, BFI and BFC, USAT, USAT16 and UBFX. ADDW and SUBW of the PC
        // are ADR, BFI of it BFC, and the saturations and bitfields have no
        // i and no bit 5
        9 => {
            let opcode = pick(rng, &[0, 4, 10, 12, 16, 18, 20, 22, 24, 26, 28]);
            let n = if [0, 10, 22].contains(&opcode) && rng.below(4) == 0 { 15 } else { read(rng) };
            let (i, low) = if opcode >= 16 {
                (zero(rng).min(1), field(rng, 8) & !0x30 | zero(rng).min(1) << 5 | zero(rng).min(1) << 4)
            } else {
                (field(rng, 1), field(rng, 8))
            };
            thumb32(0xF200 | i << 10 | opcode << 4 | n, field(rng, 3) << 12 | written(rng) << 8 | low)
        }
        // with registers: shifts by a register, extends, parallel adds and
        // subtracts, the saturating ones, REV, SEL and CLZ. the extends
        // without an addend have 1111 for Rn, and REV, REV16, RBIT, REVSH
        // and CLZ name Rm twice
        10 | 11 => {
            let (kind, operation) = (field(rng, 4), field(rng, 4));
            let m = read(rng);
            let n = if kind < 8 && operation >= 8 && rng.below(3) == 0 {
                15
            } else if (kind == 9 || kind == 11) && operation & 0b1100 == 0b1000 {
                m
            } else {
                read(rng)
            };
            thumb32(0xFA00 | kind << 4 | n, 0xF000 | written(rng) << 8 | operation << 4 | m)
        }
        // multiplies, Ra 1111 leaving the accumulator out but for MLS and
        // SMMLS, which always have one
        12 => {
            let (kind, operation) = (field(rng, 3), field(rng, 2));
            let always = (kind == 0 && operation == 1) || kind == 6;
            let a = if !always && rng.below(3) == 0 { 15 } else { read(rng) };
            thumb32(0xFB00 | kind << 4 | read(rng), a << 12 | written(rng) << 8 | operation << 4 | read(rng))
        }
        // long multiplies and the divides the A9 does not have
        13 => {
            let high = written(rng);
            let low = (high + 1 + rng.below(11) as u32) % 12;
            thumb32(0xFB80 | field(rng, 3) << 4 | read(rng), low << 12 | high << 8 | field(rng, 4) << 4 | read(rng))
        }
        // MRS, MSR of the flags and GE, and the hints
        14 => match rng.below(3) {
            0 => thumb32(0xF3EF, 0x8000 | written(rng) << 8),
            1 => thumb32(0xF380 | read(rng), 0x8000 | (1 + rng.below(3) as u32) << 10),
            _ => thumb32(0xF3AF, 0x8000 | pick(rng, &[0, 1, 2, 4])),
        },
        // UDF, both sizes
        _ => match rng.below(2) {
            0 => 0xDE00 | field(rng, 8),
            _ => thumb32(0xF7F0 | field(rng, 4), 0xA000 | field(rng, 12)),
        },
    }
}

/// the ARM encoding as Thumb has it.
fn thumb_encoding(class: &str, op: u32) -> u32 {
    match class {
        "neon" | "neon-alias" => 0xEF00_0000 | (op >> 24 & 1) << 28 | (op & 0x00FF_FFFF),
        "neon-memory" => 0xF900_0000 | (op & 0x00FF_FFFF),
        _ => op,
    }
}

/// the program: a SIGILL handler, then each case loading its state, running
/// its instruction and storing the state again, then the data written out.
fn program(class: &str, cases: &[Case], thumb: bool) -> String {
    let mut s = String::new();
    let state = if thumb { ".thumb\n.thumb_func" } else { ".arm" };
    let address = |s: &mut String, register: &str, label: &str| {
        let _ = writeln!(s, "    movw {register}, #:lower16:{label}\n    movt {register}, #:upper16:{label}");
    };
    let _ = writeln!(s, ".syntax unified\n.text\n{state}\n.global _start\n_start:");
    for signal in [4, 5, 7, 11] {
        let _ = writeln!(s, "    mov r0, #{signal}");
        address(&mut s, "r1", "action");
        let _ = writeln!(s, "    mov r2, #0\n    mov r3, #8\n    mov r7, #174\n    svc #0");
    }
    for (i, case) in cases.iter().enumerate() {
        let _ = writeln!(s, "case_{i}:");
        address(&mut s, "r0", &format!("next_{i}"));
        address(&mut s, "r1", "resume");
        let _ = writeln!(s, "    str r0, [r1]");
        address(&mut s, "r0", &format!("vin_{i}"));
        let _ = writeln!(s, "    vldmia r0!, {{d0-d15}}\n    vldmia r0!, {{d16-d31}}");
        address(&mut s, "r0", &format!("in_{i}"));
        let _ =
            writeln!(s, "    ldr r1, [r0, #52]\n    vmsr fpscr, r1\n    ldr r1, [r0, #48]\n    msr APSR_nzcvqg, r1");
        address(&mut s, "r12", &format!("out_{i}"));
        let _ = writeln!(s, "    ldm r0, {{r0-r11}}");
        if let Some(it) = case.it {
            let _ = writeln!(s, "    .inst.n 0x{it:04X}");
        }
        if let Some(source) = &case.source {
            let _ = writeln!(s, "{source}");
        } else if class == "thumb" && case.opcode < 0x1_0000 {
            let _ = writeln!(s, "    .inst.n 0x{:04X}", case.opcode);
        } else if thumb {
            let _ = writeln!(s, "    .inst.w 0x{:08X}", thumb_encoding(class, case.opcode));
        } else {
            let _ = writeln!(s, "    .inst 0x{:08X}", case.opcode);
        }
        let _ = writeln!(
            s,
            "    add r12, r12, #4\n    stmia r12!, {{r0-r11}}\n    mrs r0, APSR\n    vmrs r1, fpscr\n    \
             stmia r12!, {{r0, r1}}\n    vstmia r12!, {{d0-d15}}\n    vstmia r12!, {{d16-d31}}"
        );
        address(&mut s, "r12", &format!("out_{i}"));
        let _ = writeln!(s, "    mov r0, #{RAN}\n    str r0, [r12]\nnext_{i}:");
    }
    // write(1, data, size) and exit_group(0)
    let _ = writeln!(s, "    mov r0, #1");
    address(&mut s, "r1", "resume");
    address(&mut s, "r2", "data_end");
    let _ = writeln!(s, "    sub r2, r2, r1\n    mov r7, #4\n    svc #0\n    mov r0, #0\n    mov r7, #248\n    svc #0");
    // the handler notes 0x100 and the signal where r12 points, and goes on
    // at the resume address in the same state
    let _ = writeln!(s, "{}\nhandler:", if thumb { ".thumb_func" } else { "" });
    let _ = writeln!(s, "    ldr r3, [r2, #80]\n    add r0, r0, #0x100\n    str r0, [r3]");
    address(&mut s, "r3", "resume");
    let _ = writeln!(s, "    ldr r3, [r3]\n    str r3, [r2, #92]\n    ldr r0, [r2, #96]");
    // clear the IT bits
    let _ = writeln!(s, "    bic r0, r0, #0x06000000\n    bic r0, r0, #0xFC00\n    str r0, [r2, #96]\n    bx lr");
    let _ = writeln!(s, "{}\nrestorer:\n    mov r7, #173\n    svc #0", if thumb { ".thumb_func" } else { "" });

    let _ = writeln!(s, ".data\n.balign 64\nresume:\n    .word 0\naction:");
    // SA_SIGINFO | SA_RESTORER
    let _ = writeln!(s, "    .word handler, 0x04000004, restorer, 0, 0\n.balign 64");
    for (i, case) in cases.iter().enumerate() {
        let _ = writeln!(s, "in_{i}:");
        for (at, chunk) in case.state.chunks(14).enumerate() {
            if at == 1 {
                let _ = writeln!(s, "vin_{i}:");
            }
            let words: Vec<String> = chunk.iter().map(|w| format!("0x{w:08X}")).collect();
            let _ = writeln!(s, "    .word {}", words.join(", "));
        }
        let _ = writeln!(s, "out_{i}:\n    .space {}\n.balign 64", 4 * (STATE_WORDS + 1));
        let bytes: Vec<String> = case.buffer.iter().map(|b| format!("{b}")).collect();
        let _ = writeln!(s, "    .byte {}", bytes.join(", "));
    }
    let _ = writeln!(s, "data_end:");
    s
}

fn build(source: &str, dir: &Path) -> PathBuf {
    let (asm, object, elf) = (dir.join("oracle.s"), dir.join("oracle.o"), dir.join("oracle.elf"));
    std::fs::write(&asm, source).expect("write the program");
    let out = Command::new("llvm-mc")
        .args(["-triple=armv7a-linux-gnueabihf", "-mcpu=cortex-a9", "-mattr=+neon,+vfp3,+fp16", "-filetype=obj", "-o"])
        .arg(&object)
        .arg(&asm)
        .output()
        .expect("run llvm-mc");
    assert!(out.status.success(), "llvm-mc failed:\n{}", String::from_utf8_lossy(&out.stderr));
    let out = Command::new("ld.lld")
        .args(["-static", "-e", "_start", &format!("-Ttext=0x{TEXT:X}"), &format!("-Tdata=0x{DATA:X}"), "-o"])
        .arg(&elf)
        .arg(&object)
        .output()
        .expect("run ld.lld");
    assert!(out.status.success(), "ld.lld failed:\n{}", String::from_utf8_lossy(&out.stderr));
    elf
}

fn run_qemu(elf: &Path) -> Vec<u8> {
    let qemu = std::env::var("QEMU_ARM").unwrap_or_else(|_| "qemu-arm".into());
    let out = Command::new(&qemu).args(["-cpu", "cortex-a9"]).arg(elf).output().expect("run qemu-arm");
    assert!(out.status.success(), "qemu-arm failed: {}\n{}", out.status, String::from_utf8_lossy(&out.stderr));
    out.stdout
}

struct Memory(Vec<u8>);

impl Memory {
    fn at(&mut self, addr: u32, len: usize) -> &mut [u8] {
        let start = addr as usize;
        &mut self.0[start..start + len]
    }
}

impl Bus for Memory {
    fn read8(&mut self, addr: u32) -> u8 {
        self.at(addr, 1)[0]
    }
    fn read16(&mut self, addr: u32) -> u16 {
        u16::from_le_bytes(self.at(addr, 2).try_into().unwrap())
    }
    fn read32(&mut self, addr: u32) -> u32 {
        u32::from_le_bytes(self.at(addr, 4).try_into().unwrap())
    }
    fn write8(&mut self, addr: u32, value: u8) {
        self.at(addr, 1)[0] = value;
    }
    fn write16(&mut self, addr: u32, value: u16) {
        self.at(addr, 2).copy_from_slice(&value.to_le_bytes());
    }
    fn write32(&mut self, addr: u32, value: u32) {
        self.at(addr, 4).copy_from_slice(&value.to_le_bytes());
    }
}

/// the program's PT_LOAD segments put in place, and its entry.
fn load(elf: &[u8], memory: &mut Memory) -> u32 {
    let u16_at = |at: usize| u16::from_le_bytes([elf[at], elf[at + 1]]) as usize;
    let u32_at = |at: usize| u32::from_le_bytes(elf[at..at + 4].try_into().unwrap());
    let (phoff, phentsize, phnum) = (u32_at(0x1C) as usize, u16_at(0x2A), u16_at(0x2C));
    for i in 0..phnum {
        let header = phoff + i * phentsize;
        if u32_at(header) != 1 {
            continue;
        }
        let (offset, vaddr, filesz) = (u32_at(header + 4) as usize, u32_at(header + 8), u32_at(header + 16) as usize);
        memory.at(vaddr, filesz).copy_from_slice(&elf[offset..offset + filesz]);
    }
    u32_at(0x18)
}

fn run_kimika(elf: &Path) -> Vec<u8> {
    let bytes = std::fs::read(elf).expect("read the program");
    let mut memory = Memory(vec![0; 0x0100_0000]);
    let entry = load(&bytes, &mut memory);
    let mut cpu = Cpu::new();
    cpu.reset_to(entry, 0x00FF_0000);
    let mut output = Vec::new();
    loop {
        match cpu.run(&mut memory, 1 << 24) {
            Exit::Supervisor(_) => match cpu.regs[7] {
                174 => cpu.regs[0] = 0,
                4 => {
                    let (start, len) = (cpu.regs[1], cpu.regs[2] as usize);
                    output.extend_from_slice(memory.at(start, len));
                    cpu.regs[0] = len as u32;
                }
                248 => return output,
                call => panic!("system call {call}"),
            },
            Exit::Undefined { .. } => {
                memory.write32(cpu.regs[12], SIGILL);
                cpu.regs[15] = memory.read32(DATA);
            }
            Exit::Yield => {}
            exit => panic!("stopped with {exit:?}\n{cpu:?}"),
        }
    }
}

fn disassemble(op: u32, thumb: bool) -> String {
    let triple = if thumb { "-triple=thumbv7a-none-eabi" } else { "-triple=armv7a-none-eabi" };
    let bytes = if thumb && op < 0x1_0000 {
        (op as u16).to_le_bytes().to_vec()
    } else if thumb {
        let (first, second) = ((op >> 16) as u16, op as u16);
        [first.to_le_bytes(), second.to_le_bytes()].concat()
    } else {
        op.to_le_bytes().to_vec()
    };
    let input: String = bytes.iter().map(|b| format!("0x{b:02x} ")).collect();
    let mut child = Command::new("llvm-mc")
        .args(["--disassemble", triple, "-mcpu=cortex-a9"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("run llvm-mc");
    use std::io::Write as _;
    child.stdin.take().unwrap().write_all(input.as_bytes()).unwrap();
    let out = child.wait_with_output().unwrap();
    let text = String::from_utf8_lossy(&out.stdout);
    let line = text.lines().map(str::trim).find(|l| !l.is_empty() && !l.starts_with('.'));
    line.map(|l| l.replace('\t', " ")).unwrap_or_else(|| "(not an instruction to LLVM)".into())
}

/// where QEMU is known to part from the interpreter: QEMU runs VFPv4's
/// fused multiply-adds, which the A9's VFPv3 does not have, and lets a list
/// of registers run past d31 and around to d0, which ARM leaves
/// unpredictable and the interpreter refuses. VTRN.32 of a register with
/// itself, whose result ARM leaves unknown, swaps its lanes under QEMU.
fn qemu_deviation(class: &str, op: u32) -> Option<&'static str> {
    let field = |lo: u32, width: u32| (op >> lo) & ((1 << width) - 1);
    let same = field(22, 1) << 4 | field(12, 4) == field(5, 1) << 4 | field(0, 4);
    match class {
        "neon" | "neon-alias" if op & 0xFFB3_0F90 == 0xF3B2_0080 && field(18, 2) == 2 && same => {
            Some("VTRN.32 of one register")
        }
        "neon" | "neon-alias"
            if field(23, 1) == 0
                && field(8, 4) == 0b1100
                && field(4, 1) == 1
                && field(24, 1) == 0
                && field(20, 1) == 0 =>
        {
            Some("a fused multiply-add")
        }
        "vfp" if matches!(field(20, 4) & 0b1011, 0b1001 | 0b1010) => Some("a fused multiply-add"),
        // SSAT16 and USAT16 with the bits above their four meant to be zero
        // set, which QEMU takes as part of the width
        "thumb" if op & 0xFB50_F0C0 == 0xF310_0000 && field(4, 2) != 0 => Some("a field meant to be zero"),
        // a Thumb modified immediate repeating a zero byte, which ARM leaves
        // unpredictable and QEMU takes as zero
        "thumb" if op >= 0x1_0000 && op & 0xFA00_8000 == 0xF000_0000 => {
            let imm12 = field(26, 1) << 11 | field(12, 3) << 8 | field(0, 8);
            (imm12 >> 10 == 0 && imm12 >> 8 & 3 != 0 && imm12 & 0xFF == 0).then_some("an unpredictable immediate")
        }
        "neon-memory" => {
            let d = field(22, 1) << 4 | field(12, 4);
            // the registers a structure reaches: so many elements, so far
            // apart, so deep
            let (structure, spacing, depth) = if field(23, 1) == 0 {
                match field(8, 4) {
                    0b0111 => (1, 1, 1),
                    0b1010 => (1, 1, 2),
                    0b0110 => (1, 1, 3),
                    0b0010 => (1, 1, 4),
                    0b1000 | 0b1001 => (2, field(8, 1) + 1, 1),
                    0b0011 => (2, 2, 2),
                    0b0100 | 0b0101 => (3, field(8, 1) + 1, 1),
                    _ => (4, field(8, 1) + 1, 1),
                }
            } else if field(10, 2) == 3 {
                let t = field(5, 1) + 1;
                if field(8, 2) == 0 {
                    (1, 1, t)
                } else {
                    (field(8, 2) + 1, t, 1)
                }
            } else {
                let apart = match field(10, 2) {
                    0 => 0,
                    1 => field(5, 1),
                    _ => field(6, 1),
                };
                (field(8, 2) + 1, apart + 1, 1)
            };
            (d + (structure - 1) * spacing + depth - 1 > 31).then_some("a list past d31")
        }
        _ => None,
    }
}

fn words(data: &[u8], at: usize, count: usize) -> Vec<u32> {
    (0..count).map(|i| u32::from_le_bytes(data[at + 4 * i..at + 4 * i + 4].try_into().unwrap())).collect()
}

fn describe(status: u32) -> String {
    match status {
        RAN => "ran".into(),
        SIGILL => "undefined".into(),
        0x107 => "SIGBUS".into(),
        0x10B => "SIGSEGV".into(),
        other => format!("status 0x{other:X}"),
    }
}

/// runs one piece of assembly from the registers given, as r3=0x10,
/// d4=0x3F8000003F800000, fpscr=0x03000000 or apsr=0x20000000, with a
/// register holding an address in the buffer as r0=buffer or r0=buffer+8
/// and its bytes as buffer=0011223344, and prints what it changed under
/// each.
fn run_source(source: &str, thumb: bool, assignments: &[String]) {
    let mut case =
        Case { opcode: 0, it: None, source: Some(source.replace(';', "\n")), state: [0; STATE_WORDS], buffer: [0; 64] };
    for assignment in assignments {
        let (name, value) = assignment.split_once('=').expect("name=value");
        if name == "buffer" {
            for (i, byte) in (0..value.len()).step_by(2).enumerate() {
                case.buffer[i] = u8::from_str_radix(&value[byte..byte + 2], 16).expect("hex bytes");
            }
            continue;
        }
        let number = if let Some(offset) = value.strip_prefix("buffer") {
            let offset = offset.strip_prefix('+').map_or(0, |o| o.parse::<u64>().expect("buffer+offset"));
            (case_base(0) + BUFFER) as u64 + offset
        } else {
            u64::from_str_radix(value.trim_start_matches("0x"), 16).expect("a hex value")
        };
        match name {
            "apsr" => case.state[12] = number as u32,
            "fpscr" => case.state[13] = number as u32,
            _ if name.starts_with('r') => case.state[name[1..].parse::<usize>().expect("r0-r11")] = number as u32,
            _ if name.starts_with('d') => {
                let d: usize = name[1..].parse().expect("d0-d31");
                case.state[14 + 2 * d] = number as u32;
                case.state[15 + 2 * d] = (number >> 32) as u32;
            }
            _ => panic!("no register {name}"),
        }
    }
    let dir = std::env::temp_dir().join(format!("kimika-oracle-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("temp dir");
    let elf = build(&program("source", std::slice::from_ref(&case), thumb), &dir);
    let (qemu, kimika) = (run_qemu(&elf), run_kimika(&elf));
    let _ = std::fs::remove_dir_all(&dir);
    let output = (case_base(0) - DATA + OUTPUT) as usize;
    let (q, k) = (words(&qemu, output, STATE_WORDS + 1), words(&kimika, output, STATE_WORDS + 1));
    println!("qemu {}, kimika {}", describe(q[0]), describe(k[0]));
    let mark = |same: bool| if same { "" } else { "   <-- differs" };
    for r in 0..12 {
        if q[1 + r] != case.state[r] || k[1 + r] != case.state[r] {
            println!("r{r}: qemu 0x{:08X} kimika 0x{:08X}{}", q[1 + r], k[1 + r], mark(q[1 + r] == k[1 + r]));
        }
    }
    println!("apsr: qemu 0x{:08X} kimika 0x{:08X}{}", q[13], k[13], mark(q[13] & APSR_MASK == k[13] & APSR_MASK));
    println!("fpscr: qemu 0x{:08X} kimika 0x{:08X}{}", q[14], k[14], mark(q[14] & FPSCR_MASK == k[14] & FPSCR_MASK));
    for d in 0..32 {
        let input = case.state[14 + 2 * d] as u64 | (case.state[15 + 2 * d] as u64) << 32;
        let (qd, kd) =
            (q[15 + 2 * d] as u64 | (q[16 + 2 * d] as u64) << 32, k[15 + 2 * d] as u64 | (k[16 + 2 * d] as u64) << 32);
        if qd != input || kd != input {
            println!("d{d}: qemu 0x{qd:016X} kimika 0x{kd:016X}{}", mark(qd == kd));
        }
    }
    let buffer = (case_base(0) - DATA + BUFFER) as usize;
    if qemu[buffer..buffer + 64] != case.buffer || kimika[buffer..buffer + 64] != case.buffer {
        println!(
            "buffer: qemu {:02X?}\n        kimika {:02X?}",
            &qemu[buffer..buffer + 64],
            &kimika[buffer..buffer + 64]
        );
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).is_some_and(|a| a.starts_with("source")) {
        run_source(&args[2], args[1].ends_with("-thumb"), &args[3..]);
        return;
    }
    let class_arg = args.get(1).map(String::as_str).unwrap_or("neon");
    let count: usize = args.get(2).and_then(|a| a.parse().ok()).unwrap_or(1000);
    let seed: u64 = args.get(3).and_then(|a| a.parse().ok()).unwrap_or(1);
    let hex = |i: usize| args.get(i).and_then(|a| u32::from_str_radix(a.trim_start_matches("0x"), 16).ok());
    let pattern = (hex(4).unwrap_or(0), hex(5).unwrap_or(0));
    let thumb = class_arg.ends_with("-thumb") || class_arg == "thumb";
    let class = class_arg.trim_end_matches("-thumb");

    let mut rng = Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1);
    let cases: Vec<Case> = (0..count).map(|i| generate(class, i, &mut rng, pattern)).collect();
    let dir = std::env::temp_dir().join(format!("kimika-oracle-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("temp dir");
    let elf = build(&program(class, &cases, thumb), &dir);
    let qemu = run_qemu(&elf);
    let kimika = run_kimika(&elf);
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(qemu.len(), kimika.len(), "the two wrote different amounts");

    let (mut ran, mut undefined, mut differing, mut known) = (0, 0, 0, 0);
    let shown: usize = std::env::var("ORACLE_SHOW").ok().and_then(|n| n.parse().ok()).unwrap_or(40);
    for (i, case) in cases.iter().enumerate() {
        let base = (case_base(i) - DATA) as usize;
        let output = base + OUTPUT as usize;
        let (q, k) = (words(&qemu, output, STATE_WORDS + 1), words(&kimika, output, STATE_WORDS + 1));
        let mut report = String::new();
        // Linux takes some UDFs for breakpoints, and QEMU with it
        let q = if q[0] == SIGTRAP { [&[SIGILL][..], &q[1..]].concat() } else { q };
        if q[0] != k[0] {
            if (q[0], k[0]) == (RAN, SIGILL) && qemu_deviation(class, case.opcode).is_some() {
                known += 1;
                continue;
            }
            let _ = writeln!(report, "  qemu {}, kimika {}", describe(q[0]), describe(k[0]));
        } else if q[0] == RAN {
            for r in 0..12 {
                if q[1 + r] != k[1 + r] {
                    let _ = writeln!(report, "  r{r}: qemu 0x{:08X} kimika 0x{:08X}", q[1 + r], k[1 + r]);
                }
            }
            if q[13] & APSR_MASK != k[13] & APSR_MASK {
                let _ = writeln!(report, "  apsr: qemu 0x{:08X} kimika 0x{:08X}", q[13], k[13]);
            }
            if q[14] & FPSCR_MASK != k[14] & FPSCR_MASK {
                let _ = writeln!(report, "  fpscr: qemu 0x{:08X} kimika 0x{:08X}", q[14], k[14]);
            }
            for d in 0..32 {
                let (qd, kd) = (&q[15 + 2 * d..17 + 2 * d], &k[15 + 2 * d..17 + 2 * d]);
                if qd != kd {
                    let input = &case.state[14 + 2 * d..16 + 2 * d];
                    let _ = writeln!(
                        report,
                        "  d{d}: qemu 0x{:08X}{:08X} kimika 0x{:08X}{:08X} (was 0x{:08X}{:08X})",
                        qd[1], qd[0], kd[1], kd[0], input[1], input[0]
                    );
                }
            }
            let buffer = base + BUFFER as usize;
            if qemu[buffer..buffer + 64] != kimika[buffer..buffer + 64] {
                let _ = writeln!(
                    report,
                    "  memory: qemu {:02X?}\n          kimika {:02X?}",
                    &qemu[buffer..buffer + 64],
                    &kimika[buffer..buffer + 64]
                );
            }
        }
        if !report.is_empty() && q[0] == RAN && qemu_deviation(class, case.opcode) == Some("VTRN.32 of one register") {
            known += 1;
            report.clear();
        }
        match q[0] {
            RAN => ran += 1,
            SIGILL => undefined += 1,
            _ => {}
        }
        if !report.is_empty() {
            differing += 1;
            if differing <= shown {
                let shown = if thumb { thumb_encoding(class, case.opcode) } else { case.opcode };
                let it = case.it.map_or(String::new(), |it| format!(" in a block of {}", disassemble(it, true)));
                println!("case {i}: 0x{shown:08X} {}{it}", disassemble(shown, thumb));
                if class == "arm" || class == "thumb" {
                    let registers: Vec<String> = (0..12).map(|r| format!("r{r}=0x{:08X}", case.state[r])).collect();
                    println!("  inputs {} apsr=0x{:08X}", registers.join(" "), case.state[12]);
                } else {
                    let named: Vec<String> = [(12, 22), (16, 7), (0, 5)]
                        .iter()
                        .map(|&(field, extra)| {
                            ((case.opcode >> extra & 1) << 4 | (case.opcode >> field & 0xF)) as usize
                        })
                        .map(|r| format!("d{r}=0x{:08X}{:08X}", case.state[15 + 2 * r], case.state[14 + 2 * r]))
                        .collect();
                    println!("  inputs {} fpscr=0x{:08X}", named.join(" "), case.state[13]);
                }
                print!("{report}");
            }
        }
    }
    println!(
        "{count} cases: {ran} ran and {undefined} were undefined under QEMU; {differing} differ, \
         and {known} more where QEMU is known to"
    );
    if differing > 0 {
        std::process::exit(1);
    }
}
