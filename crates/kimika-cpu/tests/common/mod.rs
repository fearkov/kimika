//! the harness: real ARM and Thumb assembled with llvm-mc for a Cortex-A9,
//! run in the interpreter, the result looked at.

#![allow(dead_code, unused_macros, unused_imports)]

use std::io::Write;
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::OnceLock;

use kimika_cpu::{Bus, Cpu, Exit};

/// where test programs are linked and loaded.
pub const CODE_BASE: u32 = 0x0001_0000;
pub const STACK_TOP: u32 = 0x0008_0000;
/// scratch memory tests can load from and store to.
pub const DATA_BASE: u32 = 0x0010_0000;

/// flat memory, wide enough for the code, the stack and the scratch area.
pub struct TestBus {
    pub mem: Vec<u8>,
    /// addresses outside the memory, kept rather than panicking on so a
    /// runaway test says where it went.
    pub faults: Vec<u32>,
    pub barriers: usize,
}

impl Default for TestBus {
    fn default() -> Self {
        TestBus { mem: vec![0; 0x0020_0000], faults: Vec::new(), barriers: 0 }
    }
}

impl TestBus {
    fn slice(&mut self, addr: u32, len: usize) -> Option<&mut [u8]> {
        let start = addr as usize;
        if start.checked_add(len)? > self.mem.len() {
            self.faults.push(addr);
            return None;
        }
        Some(&mut self.mem[start..start + len])
    }

    pub fn write_bytes(&mut self, addr: u32, data: &[u8]) {
        let start = addr as usize;
        self.mem[start..start + data.len()].copy_from_slice(data);
    }

    pub fn write_u32(&mut self, addr: u32, value: u32) {
        self.write_bytes(addr, &value.to_le_bytes());
    }

    pub fn read_u32(&self, addr: u32) -> u32 {
        let i = addr as usize;
        u32::from_le_bytes(self.mem[i..i + 4].try_into().unwrap())
    }
}

impl Bus for TestBus {
    fn read8(&mut self, addr: u32) -> u8 {
        self.slice(addr, 1).map_or(0, |s| s[0])
    }
    fn read16(&mut self, addr: u32) -> u16 {
        self.slice(addr, 2).map_or(0, |s| u16::from_le_bytes([s[0], s[1]]))
    }
    fn read32(&mut self, addr: u32) -> u32 {
        self.slice(addr, 4).map_or(0, |s| u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
    }
    fn write8(&mut self, addr: u32, value: u8) {
        if let Some(s) = self.slice(addr, 1) {
            s[0] = value;
        }
    }
    fn write16(&mut self, addr: u32, value: u16) {
        if let Some(s) = self.slice(addr, 2) {
            s.copy_from_slice(&value.to_le_bytes());
        }
    }
    fn write32(&mut self, addr: u32, value: u32) {
        if let Some(s) = self.slice(addr, 4) {
            s.copy_from_slice(&value.to_le_bytes());
        }
    }
    fn barrier(&mut self) {
        self.barriers += 1;
    }
}

fn toolchain_available() -> bool {
    static AVAILABLE: OnceLock<bool> = OnceLock::new();
    *AVAILABLE.get_or_init(|| {
        let ok = ["llvm-mc", "ld.lld"].iter().all(|tool| Command::new(tool).arg("--version").output().is_ok());
        if !ok {
            eprintln!(
                "note: llvm-mc or ld.lld is missing, so the CPU tests that assemble their programs do not run. \
                 the llvm and lld packages have them"
            );
        }
        ok
    })
}

/// assembles ARM or Thumb source for the Vita's Cortex-A9, NEON and all,
/// and gives the code linked at [CODE_BASE].
pub fn assemble(source: &str) -> Vec<u8> {
    let dir = std::env::temp_dir().join(format!("kimika-asm-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("temp dir");
    // tests run in parallel, so the name has to be unique per call
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let stem = format!("t{}", COUNTER.fetch_add(1, Ordering::Relaxed));
    let source_path = dir.join(format!("{stem}.s"));
    let object_path = dir.join(format!("{stem}.o"));
    let binary_path = dir.join(format!("{stem}.bin"));

    let mut file = std::fs::File::create(&source_path).expect("write asm");
    write!(file, ".syntax unified\n{source}\n").expect("write asm");
    drop(file);

    let out = Command::new("llvm-mc")
        .args(["-triple=armv7a-none-eabi", "-mcpu=cortex-a9", "-mattr=+neon,+vfp3,+fp16", "-filetype=obj", "-o"])
        .arg(&object_path)
        .arg(&source_path)
        .output()
        .expect("run llvm-mc");
    assert!(
        out.status.success(),
        "llvm-mc failed:\n{}\n--- source ---\n{source}",
        String::from_utf8_lossy(&out.stderr)
    );
    let out = Command::new("ld.lld")
        .args([
            "--oformat=binary",
            &format!("--image-base={CODE_BASE:#x}"),
            &format!("-Ttext={CODE_BASE:#x}"),
            "-e",
            &format!("{CODE_BASE:#x}"),
            "-o",
        ])
        .arg(&binary_path)
        .arg(&object_path)
        .output()
        .expect("run ld.lld");
    assert!(out.status.success(), "ld.lld failed:\n{}", String::from_utf8_lossy(&out.stderr));

    let bytes = std::fs::read(&binary_path).expect("read binary");
    let _ = std::fs::remove_file(&source_path);
    let _ = std::fs::remove_file(&object_path);
    let _ = std::fs::remove_file(&binary_path);
    bytes
}

/// a finished test run.
pub struct Run {
    pub cpu: Cpu,
    pub bus: TestBus,
    pub exit: Exit,
}

impl Run {
    pub fn r(&self, index: usize) -> u32 {
        self.cpu.regs[index]
    }

    /// the NZCVQ flags as a string, for readable assertion failures.
    pub fn flags(&self) -> String {
        let p = &self.cpu.cpsr;
        [(p.n, 'N'), (p.z, 'Z'), (p.c, 'C'), (p.v, 'V'), (p.q, 'Q')]
            .iter()
            .map(|&(set, letter)| if set { letter } else { '-' })
            .collect()
    }

    pub fn s(&self, index: usize) -> f32 {
        f32::from_bits(self.cpu.vfp.regs[index])
    }

    pub fn d(&self, index: usize) -> f64 {
        f64::from_bits(self.cpu.vfp.regs[2 * index] as u64 | (self.cpu.vfp.regs[2 * index + 1] as u64) << 32)
    }

    pub fn d_bits(&self, index: usize) -> u64 {
        self.cpu.vfp.regs[2 * index] as u64 | (self.cpu.vfp.regs[2 * index + 1] as u64) << 32
    }
}

/// assembles source, runs it from a clean CPU in ARM state and stops at the
/// first bkpt.
pub fn run(source: &str) -> Option<Run> {
    run_with(source, |_, _| {})
}

/// the same, with setup seeding registers and memory first.
pub fn run_with(source: &str, setup: impl FnOnce(&mut Cpu, &mut TestBus)) -> Option<Run> {
    run_inner(source, false, setup)
}

/// assembles Thumb source and enters it in Thumb state.
pub fn run_thumb(source: &str) -> Option<Run> {
    run_thumb_with(source, |_, _| {})
}

pub fn run_thumb_with(source: &str, setup: impl FnOnce(&mut Cpu, &mut TestBus)) -> Option<Run> {
    run_inner(&format!(".thumb\n{source}"), true, setup)
}

/// runs source to its end and gives back what stopped it, for the tests of
/// what the CPU refuses.
pub fn run_to_exit(source: &str, thumb: bool, setup: impl FnOnce(&mut Cpu, &mut TestBus)) -> Option<Run> {
    if !toolchain_available() {
        return None;
    }
    let source = if thumb { format!(".thumb\n{source}") } else { source.to_owned() };
    let code = assemble(&format!("{source}\n    bkpt #0\n"));
    let mut bus = TestBus::default();
    bus.write_bytes(CODE_BASE, &code);
    let mut cpu = Cpu::new();
    cpu.reset_to(CODE_BASE | thumb as u32, STACK_TOP);
    setup(&mut cpu, &mut bus);
    let exit = cpu.run(&mut bus, 10_000);
    Some(Run { cpu, bus, exit })
}

fn run_inner(source: &str, thumb: bool, setup: impl FnOnce(&mut Cpu, &mut TestBus)) -> Option<Run> {
    if !toolchain_available() {
        return None;
    }
    // every program ends in a breakpoint, so the harness knows when to stop
    let code = assemble(&format!("{source}\n    bkpt #0\n"));
    let mut bus = TestBus::default();
    bus.write_bytes(CODE_BASE, &code);
    let mut cpu = Cpu::new();
    cpu.reset_to(CODE_BASE | thumb as u32, STACK_TOP);
    setup(&mut cpu, &mut bus);

    // generous but finite, a decoding bug that loops should fail the test
    // rather than hang the suite
    let exit = cpu.run(&mut bus, 10_000);
    if let Exit::Undefined { pc, opcode } = exit {
        panic!("undefined instruction 0x{opcode:08X} at 0x{pc:08X}\n{cpu:?}");
    }
    assert!(matches!(exit, Exit::Breakpoint { .. }), "stopped with {exit:?}\n{cpu:?}");
    assert!(bus.faults.is_empty(), "unmapped accesses at {:08X?}", bus.faults);
    Some(Run { cpu, bus, exit })
}

/// asserts a register holds an exact value, both in hex on failure.
macro_rules! assert_reg {
    ($run:expr, $index:expr, $expected:expr) => {{
        let got = $run.r($index);
        let want: u32 = $expected;
        assert_eq!(got, want, "r{} = 0x{:08X}, expected 0x{:08X}\n{:?}", $index, got, want, $run.cpu);
    }};
}

/// asserts the NZCVQ flags match a string like "NZ" or "--C-Q", padded with
/// dashes on the right.
macro_rules! assert_flags {
    ($run:expr, $expected:expr) => {{
        let got = $run.flags();
        let want: &str = $expected;
        let want = format!("{:-<5}", want);
        assert_eq!(got, want, "flags {got}, expected {want}\n{:?}", $run.cpu);
    }};
}

pub(crate) use assert_flags;
pub(crate) use assert_reg;
