//! SceSysmem's memory blocks: which part of the address space each type
//! comes from, the step its size goes in, and the blocks handed out by UID,
//! along with what the kernel takes for itself there, the program, the
//! stacks and the thread local storage.
//!
//! a type's nibbles say where it goes: bits 27:24 the partition, 9 for
//! CDRAM, C for main memory, D for its uncached side and E for the common
//! dialogs; bits 23:20 the pool, 8 for physically contiguous memory; and the
//! low byte the access, 0x60 read and write, 0x40 read only.

use std::collections::{BTreeMap, HashMap};

use super::{errors, Uid};

/// SCE_KERNEL_MEMBLOCK_TYPE_USER_MAIN_RW, what USER_RW names too.
pub const USER_RW: u32 = 0x0C20_D060;
pub const USER_CDRAM_RW: u32 = 0x0940_8060;

/// the partitions, from their first address to the one past their end.
pub const MAIN: (u32, u32) = (0x8100_0000, 0xD000_0000);
pub const CDRAM: (u32, u32) = (0x6000_0000, 0x6800_0000);
pub const UNCACHED: (u32, u32) = (0x7000_0000, 0x8000_0000);
pub const DIALOG: (u32, u32) = (0xD000_0000, 0xE000_0000);

const KIB: u32 = 1024;
const MIB: u32 = 1024 * KIB;

/// where a type's blocks go and the step their sizes go in, none for a type
/// a game cannot ask for.
pub fn partition(kind: u32) -> Option<((u32, u32), u32)> {
    if !matches!(kind & 0xFF, 0x40 | 0x60) {
        return None;
    }
    let contiguous = (kind >> 20) & 0xF == 8;
    match (kind >> 24) & 0xF {
        0x9 => Some((CDRAM, 256 * KIB)),
        0xC => Some((MAIN, if contiguous { MIB } else { 4 * KIB })),
        0xD => Some((UNCACHED, MIB)),
        0xE => Some((DIALOG, 4 * KIB)),
        _ => None,
    }
}

pub struct Block {
    pub uid: Uid,
    pub name: String,
    pub kind: u32,
    pub base: u32,
    pub size: u32,
}

#[derive(Default)]
pub struct Blocks {
    by_uid: HashMap<Uid, Block>,
    /// every range in use, blocks and the kernel's own, by base, with sizes.
    taken: BTreeMap<u32, u32>,
}

impl Blocks {
    /// marks a range the kernel placed something in itself.
    pub fn reserve(&mut self, base: u32, size: u32) {
        self.taken.insert(base, size);
    }

    /// the lowest place size bytes aligned to align fit in a partition, now
    /// taken.
    pub fn take(&mut self, (start, end): (u32, u32), size: u32, align: u32) -> Option<u32> {
        let align = align.max(4 * KIB) as u64;
        let mut candidate = (start as u64).next_multiple_of(align);
        for (&base, &length) in self.taken.range(..end) {
            let (base, length) = (base as u64, length as u64);
            if base + length <= candidate {
                continue;
            }
            if base >= candidate + size as u64 {
                break;
            }
            candidate = (base + length).next_multiple_of(align);
        }
        if candidate + size as u64 > end as u64 {
            return None;
        }
        self.taken.insert(candidate as u32, size);
        Some(candidate as u32)
    }

    /// takes a range at base if nothing there is taken.
    pub fn take_at(&mut self, base: u32, size: u32) -> bool {
        let end = base as u64 + size as u64;
        let overlaps = self
            .taken
            .range(..end.min(u32::MAX as u64 + 1) as u32)
            .next_back()
            .is_some_and(|(&other, &length)| other as u64 + length as u64 > base as u64)
            || self.taken.range(base..).next().is_some_and(|(&other, _)| (other as u64) < end);
        if overlaps || end > u32::MAX as u64 + 1 {
            return false;
        }
        self.taken.insert(base, size);
        true
    }

    /// gives a range the kernel took back, and says how big it was.
    pub fn untake(&mut self, base: u32) -> Option<u32> {
        self.taken.remove(&base)
    }

    /// a new block of a type, its size checked against the type's step and
    /// its base aligned to it, or to alignment when the caller asks for
    /// more, and blocks of a megabyte or more to a megabyte.
    pub fn allocate(
        &mut self,
        uid: Uid,
        name: &str,
        kind: u32,
        size: u32,
        alignment: Option<u32>,
    ) -> Result<&Block, u32> {
        let (area, step) = partition(kind).ok_or(errors::ILLEGAL_MEMBLOCK_TYPE)?;
        if size == 0 || !size.is_multiple_of(step) {
            return Err(errors::ILLEGAL_MEMBLOCK_SIZE);
        }
        let mut align = step.max(alignment.unwrap_or(0));
        if size >= MIB {
            align = align.max(MIB);
        }
        let base = self.take(area, size, align).ok_or(errors::NO_FREE_PHYSICAL_PAGE)?;
        let block = Block { uid, name: name.to_owned(), kind, base, size };
        Ok(self.by_uid.entry(uid).or_insert(block))
    }

    pub fn free(&mut self, uid: Uid) -> Result<Block, u32> {
        let block = self.by_uid.remove(&uid).ok_or(errors::ILLEGAL_BLOCK_ID)?;
        self.taken.remove(&block.base);
        Ok(block)
    }

    pub fn get(&self, uid: Uid) -> Option<&Block> {
        self.by_uid.get(&uid)
    }

    /// the block an address falls in.
    pub fn find(&self, address: u32) -> Option<&Block> {
        self.by_uid.values().find(|block| address.wrapping_sub(block.base) < block.size)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blocks_go_around_what_is_taken_and_keep_their_alignment() {
        let mut blocks = Blocks::default();
        // a program at the start of main memory, as the loader places one
        blocks.reserve(0x8100_0000, 0x5_7000);
        let heap = blocks.allocate(1, "heap", USER_RW, 0x0800_0000, None).unwrap().base;
        assert_eq!(heap, 0x8110_0000);
        let small = blocks.allocate(3, "small", USER_RW, 0x1000, None).unwrap().base;
        assert_eq!(small, 0x8105_7000);
        let screen = blocks.allocate(5, "display", USER_CDRAM_RW, 0x20_0000, None).unwrap().base;
        assert_eq!(screen, 0x6000_0000);
        assert_eq!(blocks.find(0x8110_0004).map(|block| block.uid), Some(1));
        // freed, its place is the first to go again
        blocks.free(1).unwrap();
        assert_eq!(blocks.allocate(7, "again", USER_RW, 0x1000, None).unwrap().base, 0x8105_8000);
    }

    #[test]
    fn a_range_is_taken_where_asked_only_if_it_is_free() {
        let mut blocks = Blocks::default();
        assert!(blocks.take_at(0x8100_0000, 0x2000));
        assert!(!blocks.take_at(0x8100_1000, 0x1000));
        assert!(!blocks.take_at(0x80FF_F000, 0x2000));
        assert!(blocks.take_at(0x8100_2000, 0x1000));
        assert!(blocks.take_at(0x80FF_F000, 0x1000));
        assert_eq!(blocks.untake(0x8100_0000), Some(0x2000));
        assert!(blocks.take_at(0x8100_1000, 0x1000));
    }

    #[test]
    fn sizes_and_types_the_system_does_not_take_are_refused() {
        let mut blocks = Blocks::default();
        assert_eq!(blocks.allocate(1, "odd", USER_RW, 0x800, None).err(), Some(errors::ILLEGAL_MEMBLOCK_SIZE));
        assert_eq!(blocks.allocate(1, "odd", USER_CDRAM_RW, 0x1000, None).err(), Some(errors::ILLEGAL_MEMBLOCK_SIZE));
        assert_eq!(blocks.allocate(1, "kernel", 0x1020_D006, 0x1000, None).err(), Some(errors::ILLEGAL_MEMBLOCK_TYPE));
        assert_eq!(blocks.free(9).err(), Some(errors::ILLEGAL_BLOCK_ID));
    }
}
