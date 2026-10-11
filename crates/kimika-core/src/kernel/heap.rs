//! memory the system libraries done here hand the game, for the objects a
//! real library would keep in its own memory: sync objects, shader
//! programs, render targets and the like. it comes from main memory a
//! chunk at a time, and is given out first fit.

use std::collections::BTreeMap;

use super::memblock::{Blocks, MAIN};
use crate::memory::Memory;

/// how much main memory the heap takes at a time.
const CHUNK: u32 = 0x1_0000;

#[derive(Default)]
pub struct Heap {
    /// the free ranges, by address, with their sizes.
    free: BTreeMap<u32, u32>,
    /// what was given out, by address, with its size.
    used: BTreeMap<u32, u32>,
}

impl Heap {
    /// size bytes aligned to align, zeroed, none when main memory is full.
    pub fn allocate(&mut self, blocks: &mut Blocks, memory: &Memory, size: u32, align: u32) -> Option<u32> {
        let size = size.max(1).next_multiple_of(8);
        let align = align.max(8);
        let address = match self.fit(size, align) {
            Some(address) => address,
            None => {
                let chunk = size.next_multiple_of(0x1000).max(CHUNK);
                let base = blocks.take(MAIN, chunk, 0x1000)?;
                if !memory.commit(base, chunk) {
                    blocks.untake(base);
                    return None;
                }
                self.give(base, chunk);
                self.fit(size, align)?
            }
        };
        memory.fill(address, 0, size);
        Some(address)
    }

    /// gives back what allocate gave.
    pub fn free(&mut self, address: u32) {
        if let Some(size) = self.used.remove(&address) {
            self.give(address, size);
        }
    }

    /// takes size bytes aligned to align from the first free range they
    /// fit in.
    fn fit(&mut self, size: u32, align: u32) -> Option<u32> {
        let (base, length, address) = self.free.iter().find_map(|(&base, &length)| {
            let address = base.next_multiple_of(align);
            (address as u64 + size as u64 <= base as u64 + length as u64).then_some((base, length, address))
        })?;
        self.free.remove(&base);
        if address > base {
            self.free.insert(base, address - base);
        }
        let end = address + size;
        if end < base + length {
            self.free.insert(end, base + length - end);
        }
        self.used.insert(address, size);
        Some(address)
    }

    /// puts a range back among the free ones, merged with its neighbours.
    fn give(&mut self, mut base: u32, mut size: u32) {
        if let Some((&before, &length)) = self.free.range(..base).next_back() {
            if before + length == base {
                self.free.remove(&before);
                base = before;
                size += length;
            }
        }
        if let Some(&after) = self.free.get(&(base + size)) {
            self.free.remove(&(base + size));
            size += after;
        }
        self.free.insert(base, size);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allocations_are_aligned_reused_and_merged() {
        let memory = Memory::new();
        let mut blocks = Blocks::default();
        let mut heap = Heap::default();
        let a = heap.allocate(&mut blocks, &memory, 0x10, 8).unwrap();
        let b = heap.allocate(&mut blocks, &memory, 0x100, 0x100).unwrap();
        assert_eq!(b % 0x100, 0);
        assert!(b >= a + 0x10);
        memory.write32(a, 0xFFFF_FFFF);
        heap.free(a);
        let c = heap.allocate(&mut blocks, &memory, 0x10, 8).unwrap();
        assert_eq!(c, a);
        assert_eq!(memory.read32(c), 0);
        heap.free(b);
        heap.free(c);
        assert_eq!(heap.free.len(), 1);
        // bigger than a chunk takes a chunk of its own
        let big = heap.allocate(&mut blocks, &memory, 0x2_0000, 8).unwrap();
        assert!(memory.is_range_committed(big, 0x2_0000));
    }
}
