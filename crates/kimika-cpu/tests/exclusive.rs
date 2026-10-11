//! the exclusive monitor with cores running at once, each on a thread of its
//! own over the same memory, the way guest threads run: LDREX and STREX, a
//! compare and swap against what LDREX read, lose no update, and the forms
//! for bytes and halfwords leave the rest of their word alone.

mod common;

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

use common::{assemble, CODE_BASE};
use kimika_cpu::{Bus, Cpu, Exit};

/// where the counters are.
const COUNTERS: u32 = 0x0004_0000;

/// memory every core shares, words that are atomics.
#[derive(Clone)]
struct Shared(Arc<Vec<AtomicU32>>);

impl Shared {
    fn new(words: usize) -> Shared {
        Shared(Arc::new((0..words).map(|_| AtomicU32::new(0)).collect()))
    }

    fn word(&self, addr: u32) -> &AtomicU32 {
        &self.0[(addr / 4) as usize]
    }

    /// size bytes at addr within their word: how far up and which bits.
    fn part(addr: u32, size: u32) -> (u32, u32) {
        let shift = addr % 4 * 8;
        (shift, (u64::MAX >> (64 - 8 * size)) as u32)
    }

    fn read(&self, addr: u32, size: u32) -> u32 {
        let (shift, mask) = Shared::part(addr, size);
        (self.word(addr).load(Ordering::SeqCst) >> shift) & mask
    }

    /// a word changed by f as one step against the other cores, unless f
    /// declines, which says whether it changed.
    fn update(&self, addr: u32, f: impl Fn(u32) -> Option<u32>) -> bool {
        let word = self.word(addr);
        let mut old = word.load(Ordering::SeqCst);
        loop {
            let Some(new) = f(old) else { return false };
            match word.compare_exchange_weak(old, new, Ordering::SeqCst, Ordering::SeqCst) {
                Ok(_) => return true,
                Err(now) => old = now,
            }
        }
    }

    /// a part of a word written alone, with the rest of it as it is then.
    fn write(&self, addr: u32, size: u32, value: u32) {
        let (shift, mask) = Shared::part(addr, size);
        self.update(addr, |word| Some(word & !(mask << shift) | (value & mask) << shift));
    }
}

impl Bus for Shared {
    fn read8(&mut self, addr: u32) -> u8 {
        self.read(addr, 1) as u8
    }
    fn read16(&mut self, addr: u32) -> u16 {
        self.read(addr, 2) as u16
    }
    fn read32(&mut self, addr: u32) -> u32 {
        self.word(addr).load(Ordering::SeqCst)
    }
    fn write8(&mut self, addr: u32, value: u8) {
        self.write(addr, 1, value as u32);
    }
    fn write16(&mut self, addr: u32, value: u16) {
        self.write(addr, 2, value as u32);
    }
    fn write32(&mut self, addr: u32, value: u32) {
        self.word(addr).store(value, Ordering::SeqCst);
    }

    fn compare_exchange(&mut self, addr: u32, size: u32, expected: u64, value: u64) -> bool {
        assert!(size <= 4, "the doubleword forms are not tested here");
        let (shift, mask) = Shared::part(addr, size);
        self.update(addr, |word| {
            ((word >> shift) & mask == expected as u32)
                .then_some(word & !(mask << shift) | (value as u32 & mask) << shift)
        })
    }
}

/// runs a program on one core per setup at once, each until its bkpt.
fn run_cores(source: &str, setups: Vec<Box<dyn FnOnce(&mut Cpu) + Send>>) -> Option<Shared> {
    if std::process::Command::new("llvm-mc").arg("--version").output().is_err() {
        return None;
    }
    let code = assemble(&format!("{source}\n    bkpt #0\n"));
    let memory = Shared::new(0x2_0000);
    for (i, chunk) in code.chunks(4).enumerate() {
        let mut word = [0; 4];
        word[..chunk.len()].copy_from_slice(chunk);
        memory.0[(CODE_BASE / 4) as usize + i].store(u32::from_le_bytes(word), Ordering::SeqCst);
    }
    let cores: Vec<_> = setups
        .into_iter()
        .map(|setup| {
            let mut bus = memory.clone();
            std::thread::spawn(move || {
                let mut cpu = Cpu::new();
                cpu.reset_to(CODE_BASE, 0);
                setup(&mut cpu);
                loop {
                    match cpu.run(&mut bus, 1 << 20) {
                        Exit::Breakpoint { .. } => return,
                        Exit::Timeout => {}
                        exit => panic!("stopped with {exit:?}\n{cpu:?}"),
                    }
                }
            })
        })
        .collect();
    for core in cores {
        core.join().unwrap();
    }
    Some(memory)
}

/// adds 1 to the counter at r0 r5 times, through LDREX, LDREXH or LDREXB
/// and the STREX that goes with it.
fn counting(suffix: &str) -> String {
    format!(
        "    mov r4, #0
         1:
         ldrex{suffix} r1, [r0]
         add r1, r1, #1
         strex{suffix} r2, r1, [r0]
         cmp r2, #0
         bne 1b
         add r4, r4, #1
         cmp r4, r5
         bne 1b"
    )
}

#[test]
fn cores_counting_at_once_lose_no_update() {
    const COUNT: u32 = 50_000;
    let setups = (0..4)
        .map(|_| {
            Box::new(|cpu: &mut Cpu| {
                cpu.regs[0] = COUNTERS;
                cpu.regs[5] = COUNT;
            }) as Box<dyn FnOnce(&mut Cpu) + Send>
        })
        .collect();
    let Some(memory) = run_cores(&counting(""), setups) else { return };
    assert_eq!(memory.read(COUNTERS, 4), 4 * COUNT);
}

/// two cores on each half of one word, and two on each byte of another: a
/// halfword's or a byte's compare and swap must lose no update and keep the
/// bytes beside it.
#[test]
fn exclusives_of_halfwords_and_bytes_keep_their_neighbours() {
    const HALVES: u32 = 15_000;
    const BYTES: u32 = 100;
    let cores = |offsets: &[u32], count: u32| -> Vec<Box<dyn FnOnce(&mut Cpu) + Send>> {
        offsets
            .iter()
            .flat_map(|&offset| [offset, offset])
            .map(|offset| {
                Box::new(move |cpu: &mut Cpu| {
                    cpu.regs[0] = COUNTERS + offset;
                    cpu.regs[5] = count;
                }) as Box<dyn FnOnce(&mut Cpu) + Send>
            })
            .collect()
    };
    let Some(halves) = run_cores(&counting("h"), cores(&[0, 2], HALVES)) else { return };
    assert_eq!(halves.read(COUNTERS, 4), (2 * HALVES) << 16 | 2 * HALVES);
    let bytes = run_cores(&counting("b"), cores(&[0, 1, 2, 3], BYTES)).unwrap();
    assert_eq!(bytes.read(COUNTERS, 4), u32::from_le_bytes([2 * BYTES as u8; 4]));
}
