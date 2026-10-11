//! the guest's memory: the whole 32-bit address space reserved at once on
//! the host, so that a guest address is an offset from one base, with pages
//! committed as the system hands them out, to the loader and
//! sceKernelAllocMemBlock, and decommitted when it takes them back.
//!
//! guest threads run on host threads of their own and touch the same
//! memory at once, so every access is an atomic one, relaxed, which costs
//! nothing over a plain load or store and keeps a guest's own races from
//! becoming the host's undefined behaviour. LDREX and STREX meet in a real
//! compare and swap.
//!
//! an address no page was committed at reads as zero and drops writes, each
//! page warned about once: one stray pointer should not end the session.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU16, AtomicU32, AtomicU64, AtomicU8, Ordering};
use std::sync::Mutex;

use kimika_cpu::Bus;

pub const PAGE_SIZE: u32 = 0x1000;
const PAGES: usize = 1 << 20;
/// the address space and a page past its end, for an access that runs over
/// the top to land in rather than outside the reservation.
const RESERVED: usize = (1 << 32) + PAGE_SIZE as usize;

pub struct Memory {
    base: *mut u8,
    /// a bit for each page, set while it is committed.
    committed: Box<[AtomicU64]>,
    /// the pages an access missed, and how many times.
    faults: Mutex<BTreeMap<u32, u64>>,
}

// SAFETY: the reservation is only reached through atomics, and through the
// calls that commit and decommit pages, which the kernel makes one at a time.
unsafe impl Send for Memory {}
unsafe impl Sync for Memory {}

impl Default for Memory {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for Memory {
    fn drop(&mut self) {
        // SAFETY: the reservation new made, which nothing uses any more
        unsafe { host::release(self.base, RESERVED) };
    }
}

impl Memory {
    pub fn new() -> Memory {
        Memory {
            base: host::reserve(RESERVED),
            committed: (0..PAGES / 64).map(|_| AtomicU64::new(0)).collect(),
            faults: Mutex::new(BTreeMap::new()),
        }
    }

    // -- pages ---

    /// commits the pages size bytes from address touch, which read as zero
    /// until written. false when the host refuses.
    pub fn commit(&self, address: u32, size: u32) -> bool {
        let Some((first, count)) = pages(address, size) else { return true };
        // SAFETY: whole pages inside the reservation
        if !unsafe { host::commit(self.base.add(first as usize * PAGE_SIZE as usize), count * PAGE_SIZE as usize) } {
            return false;
        }
        for page in first..first + count as u32 {
            self.committed[(page / 64) as usize].fetch_or(1 << (page % 64), Ordering::Release);
        }
        true
    }

    /// gives pages back, which read as zero if committed again.
    pub fn decommit(&self, address: u32, size: u32) {
        let Some((first, count)) = pages(address, size) else { return };
        for page in first..first + count as u32 {
            self.committed[(page / 64) as usize].fetch_and(!(1 << (page % 64)), Ordering::Release);
        }
        // SAFETY: whole pages inside the reservation
        unsafe { host::decommit(self.base.add(first as usize * PAGE_SIZE as usize), count * PAGE_SIZE as usize) };
    }

    #[inline(always)]
    pub fn is_committed(&self, address: u32) -> bool {
        let page = address >> 12;
        self.committed[(page / 64) as usize].load(Ordering::Relaxed) >> (page % 64) & 1 == 1
    }

    /// whether every page of a range is committed.
    pub fn is_range_committed(&self, address: u32, size: u32) -> bool {
        match pages(address, size) {
            Some((first, count)) => (first..first + count as u32).all(|page| self.is_committed(page << 12)),
            None => true,
        }
    }

    /// the pages an access missed, with how many times, for a report.
    pub fn fault_summary(&self) -> Vec<(u32, u64)> {
        self.faults.lock().unwrap().iter().map(|(&page, &count)| (page, count)).collect()
    }

    #[cold]
    fn missed(&self, address: u32, write: bool) -> u32 {
        let first = {
            let mut faults = self.faults.lock().unwrap();
            let count = faults.entry(address & !(PAGE_SIZE - 1)).or_insert(0);
            *count += 1;
            *count == 1
        };
        if first {
            let what = if write { "write to" } else { "read of" };
            log::warn!("{what} unmapped memory at 0x{address:08X}, warned once a page");
        }
        0
    }

    #[inline(always)]
    fn host<T>(&self, address: u32) -> *mut T {
        // SAFETY: every 32-bit address is inside the reservation
        unsafe { self.base.add(address as usize) as *mut T }
    }

    // -- single accesses ---

    #[inline(always)]
    pub fn read8(&self, address: u32) -> u8 {
        if !self.is_committed(address) {
            return self.missed(address, false) as u8;
        }
        // SAFETY: a committed byte of the reservation
        unsafe { AtomicU8::from_ptr(self.host(address)).load(Ordering::Relaxed) }
    }

    #[inline(always)]
    pub fn read16(&self, address: u32) -> u16 {
        if address & 1 != 0 {
            return u16::from_le_bytes([self.read8(address), self.read8(address.wrapping_add(1))]);
        }
        if !self.is_committed(address) {
            return self.missed(address, false) as u16;
        }
        // SAFETY: committed, aligned and inside the reservation
        unsafe { AtomicU16::from_ptr(self.host(address)).load(Ordering::Relaxed) }
    }

    #[inline(always)]
    pub fn read32(&self, address: u32) -> u32 {
        if address & 3 != 0 {
            let bytes = std::array::from_fn(|i| self.read8(address.wrapping_add(i as u32)));
            return u32::from_le_bytes(bytes);
        }
        if !self.is_committed(address) {
            return self.missed(address, false);
        }
        // SAFETY: committed, aligned and inside the reservation
        unsafe { AtomicU32::from_ptr(self.host(address)).load(Ordering::Relaxed) }
    }

    #[inline(always)]
    pub fn write8(&self, address: u32, value: u8) {
        if !self.is_committed(address) {
            self.missed(address, true);
            return;
        }
        // SAFETY: a committed byte of the reservation
        unsafe { AtomicU8::from_ptr(self.host(address)).store(value, Ordering::Relaxed) }
    }

    #[inline(always)]
    pub fn write16(&self, address: u32, value: u16) {
        if address & 1 != 0 {
            let [low, high] = value.to_le_bytes();
            self.write8(address, low);
            self.write8(address.wrapping_add(1), high);
            return;
        }
        if !self.is_committed(address) {
            self.missed(address, true);
            return;
        }
        // SAFETY: committed, aligned and inside the reservation
        unsafe { AtomicU16::from_ptr(self.host(address)).store(value, Ordering::Relaxed) }
    }

    #[inline(always)]
    pub fn write32(&self, address: u32, value: u32) {
        if address & 3 != 0 {
            for (i, byte) in value.to_le_bytes().into_iter().enumerate() {
                self.write8(address.wrapping_add(i as u32), byte);
            }
            return;
        }
        if !self.is_committed(address) {
            self.missed(address, true);
            return;
        }
        // SAFETY: committed, aligned and inside the reservation
        unsafe { AtomicU32::from_ptr(self.host(address)).store(value, Ordering::Relaxed) }
    }

    /// stores value if the size bytes at address still hold expected, as
    /// one step against every other thread. exclusives are aligned, and an
    /// unaligned or unmapped one fails.
    pub fn compare_exchange(&self, address: u32, size: u32, expected: u64, value: u64) -> bool {
        if address & (size - 1) != 0 || !self.is_committed(address) {
            return false;
        }
        let ordering = (Ordering::SeqCst, Ordering::SeqCst);
        // SAFETY: committed, aligned to its size and inside the reservation
        unsafe {
            match size {
                1 => AtomicU8::from_ptr(self.host(address))
                    .compare_exchange(expected as u8, value as u8, ordering.0, ordering.1)
                    .is_ok(),
                2 => AtomicU16::from_ptr(self.host(address))
                    .compare_exchange(expected as u16, value as u16, ordering.0, ordering.1)
                    .is_ok(),
                4 => AtomicU32::from_ptr(self.host(address))
                    .compare_exchange(expected as u32, value as u32, ordering.0, ordering.1)
                    .is_ok(),
                _ => AtomicU64::from_ptr(self.host(address))
                    .compare_exchange(expected, value, ordering.0, ordering.1)
                    .is_ok(),
            }
        }
    }

    // -- bulk access ---

    /// fills buffer from address on, false if any of it was unmapped, which
    /// reads as zero.
    pub fn read_bytes(&self, address: u32, buffer: &mut [u8]) -> bool {
        let mapped = self.is_range_committed(address, buffer.len() as u32);
        let mut at = 0;
        // words where the address allows, bytes around them
        while at < buffer.len() {
            let here = address.wrapping_add(at as u32);
            if here & 3 == 0 && buffer.len() - at >= 4 {
                buffer[at..at + 4].copy_from_slice(&self.read32(here).to_le_bytes());
                at += 4;
            } else {
                buffer[at] = self.read8(here);
                at += 1;
            }
        }
        mapped
    }

    pub fn write_bytes(&self, address: u32, data: &[u8]) -> bool {
        let mapped = self.is_range_committed(address, data.len() as u32);
        let mut at = 0;
        while at < data.len() {
            let here = address.wrapping_add(at as u32);
            if here & 3 == 0 && data.len() - at >= 4 {
                self.write32(here, u32::from_le_bytes(data[at..at + 4].try_into().unwrap()));
                at += 4;
            } else {
                self.write8(here, data[at]);
                at += 1;
            }
        }
        mapped
    }

    /// count bytes from address, or none if any is unmapped.
    pub fn bytes(&self, address: u32, count: usize) -> Option<Vec<u8>> {
        let mut buffer = vec![0; count];
        self.read_bytes(address, &mut buffer).then_some(buffer)
    }

    /// sets count bytes to value.
    pub fn fill(&self, address: u32, value: u8, count: u32) -> bool {
        let mapped = self.is_range_committed(address, count);
        let word = u32::from_ne_bytes([value; 4]);
        let mut at = 0;
        while at < count {
            let here = address.wrapping_add(at);
            if here & 3 == 0 && count - at >= 4 {
                self.write32(here, word);
                at += 4;
            } else {
                self.write8(here, value);
                at += 1;
            }
        }
        mapped
    }

    /// moves count bytes from source to destination, the two overlapping or
    /// not, as memmove does.
    pub fn copy(&self, destination: u32, source: u32, count: u32) -> bool {
        match self.bytes(source, count as usize) {
            Some(bytes) => self.write_bytes(destination, &bytes),
            None => false,
        }
    }

    /// a NUL-terminated string at address, at most max bytes, none if it
    /// runs into unmapped memory.
    pub fn read_cstring(&self, address: u32, max: usize) -> Option<String> {
        let mut bytes = Vec::new();
        for i in 0..max as u32 {
            let at = address.wrapping_add(i);
            if !self.is_committed(at) {
                return None;
            }
            match self.read8(at) {
                0 => break,
                byte => bytes.push(byte),
            }
        }
        Some(String::from_utf8_lossy(&bytes).into_owned())
    }
}

/// the first page of a range and how many it touches, none for an empty one.
fn pages(address: u32, size: u32) -> Option<(u32, usize)> {
    if size == 0 {
        return None;
    }
    let first = address >> 12;
    let last = ((address as u64 + size as u64 - 1) >> 12).min(PAGES as u64 - 1) as u32;
    Some((first, (last - first) as usize + 1))
}

/// a thread's way into the memory, which the CPU reads and writes through.
pub struct GuestBus<'a>(pub &'a Memory);

impl Bus for GuestBus<'_> {
    #[inline(always)]
    fn read8(&mut self, addr: u32) -> u8 {
        self.0.read8(addr)
    }
    #[inline(always)]
    fn read16(&mut self, addr: u32) -> u16 {
        self.0.read16(addr)
    }
    #[inline(always)]
    fn read32(&mut self, addr: u32) -> u32 {
        self.0.read32(addr)
    }
    #[inline(always)]
    fn write8(&mut self, addr: u32, value: u8) {
        self.0.write8(addr, value)
    }
    #[inline(always)]
    fn write16(&mut self, addr: u32, value: u16) {
        self.0.write16(addr, value)
    }
    #[inline(always)]
    fn write32(&mut self, addr: u32, value: u32) {
        self.0.write32(addr, value)
    }
    fn compare_exchange(&mut self, addr: u32, size: u32, expected: u64, value: u64) -> bool {
        self.0.compare_exchange(addr, size, expected, value)
    }
}

#[cfg(unix)]
mod host {
    /// size bytes of address space nothing can touch yet.
    pub fn reserve(size: usize) -> *mut u8 {
        let flags = libc::MAP_PRIVATE | libc::MAP_ANONYMOUS | libc::MAP_NORESERVE;
        // SAFETY: a fresh anonymous mapping wherever the host likes
        let base = unsafe { libc::mmap(std::ptr::null_mut(), size, libc::PROT_NONE, flags, -1, 0) };
        assert!(base != libc::MAP_FAILED, "could not reserve 4 GiB of address space for the guest");
        base as *mut u8
    }

    /// # Safety
    ///
    /// address and size are whole pages inside the reservation.
    pub unsafe fn commit(address: *mut u8, size: usize) -> bool {
        // SAFETY: as the caller promises
        unsafe { libc::mprotect(address as *mut _, size, libc::PROT_READ | libc::PROT_WRITE) == 0 }
    }

    /// # Safety
    ///
    /// address and size are whole pages inside the reservation.
    pub unsafe fn decommit(address: *mut u8, size: usize) {
        // SAFETY: as the caller promises. a private anonymous page dropped
        // this way comes back as zeros
        unsafe {
            libc::madvise(address as *mut _, size, libc::MADV_DONTNEED);
            libc::mprotect(address as *mut _, size, libc::PROT_NONE);
        }
    }

    /// # Safety
    ///
    /// base and size are the reservation's, which nothing uses any more.
    pub unsafe fn release(base: *mut u8, size: usize) {
        // SAFETY: as the caller promises
        unsafe { libc::munmap(base as *mut _, size) };
    }
}

#[cfg(windows)]
mod host {
    use windows_sys::Win32::System::Memory::{
        VirtualAlloc, VirtualFree, MEM_COMMIT, MEM_DECOMMIT, MEM_RELEASE, MEM_RESERVE, PAGE_NOACCESS, PAGE_READWRITE,
    };

    /// size bytes of address space nothing can touch yet.
    pub fn reserve(size: usize) -> *mut u8 {
        // SAFETY: a fresh reservation wherever the host likes
        let base = unsafe { VirtualAlloc(std::ptr::null(), size, MEM_RESERVE, PAGE_NOACCESS) };
        assert!(!base.is_null(), "could not reserve 4 GiB of address space for the guest");
        base as *mut u8
    }

    /// # Safety
    ///
    /// address and size are whole pages inside the reservation.
    pub unsafe fn commit(address: *mut u8, size: usize) -> bool {
        // SAFETY: as the caller promises
        !unsafe { VirtualAlloc(address as *const _, size, MEM_COMMIT, PAGE_READWRITE) }.is_null()
    }

    /// # Safety
    ///
    /// address and size are whole pages inside the reservation.
    pub unsafe fn decommit(address: *mut u8, size: usize) {
        // SAFETY: as the caller promises; decommitted pages come back as zeros
        unsafe { VirtualFree(address as *mut _, size, MEM_DECOMMIT) };
    }

    /// # Safety
    ///
    /// base is the reservation's, which nothing uses any more.
    pub unsafe fn release(base: *mut u8, _size: usize) {
        // SAFETY: as the caller promises
        unsafe { VirtualFree(base as *mut _, 0, MEM_RELEASE) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn committed_pages_read_back_and_the_rest_reads_zero() {
        let memory = Memory::new();
        assert!(memory.commit(0x8100_0000, 0x2000));
        memory.write32(0x8100_0FFE, 0xAABB_CCDD);
        // across the two pages, byte by byte
        assert_eq!(memory.read32(0x8100_0FFE), 0xAABB_CCDD);
        assert_eq!(memory.read16(0x8100_1000), 0xAABB);
        // nothing is there, nothing comes back, and the write goes nowhere
        memory.write32(0x9000_0000, 1);
        assert_eq!(memory.read32(0x9000_0000), 0);
        assert_eq!(memory.fault_summary(), [(0x9000_0000, 2)]);
    }

    #[test]
    fn decommitted_pages_come_back_as_zeros() {
        let memory = Memory::new();
        memory.commit(0x6000_0000, 0x1000);
        memory.fill(0x6000_0000, 0xAB, 0x1000);
        memory.decommit(0x6000_0000, 0x1000);
        assert!(!memory.is_committed(0x6000_0000));
        memory.commit(0x6000_0000, 0x1000);
        assert_eq!(memory.read32(0x6000_0FFC), 0);
    }

    #[test]
    fn copies_overlap_the_way_memmove_does() {
        let memory = Memory::new();
        memory.commit(0x8100_0000, 0x1000);
        memory.write_bytes(0x8100_0000, b"abcdefgh");
        memory.copy(0x8100_0002, 0x8100_0000, 6);
        assert_eq!(memory.bytes(0x8100_0000, 8).unwrap(), b"ababcdef");
        assert_eq!(memory.read_cstring(0x8100_0000, 3).as_deref(), Some("aba"));
    }

    #[test]
    fn exclusives_swap_only_what_they_expect() {
        let memory = Memory::new();
        memory.commit(0x8100_0000, 0x1000);
        memory.write32(0x8100_0010, 5);
        assert!(!memory.compare_exchange(0x8100_0010, 4, 4, 9));
        assert!(memory.compare_exchange(0x8100_0010, 4, 5, 9));
        assert_eq!(memory.read32(0x8100_0010), 9);
        // a byte of the word leaves the others alone
        assert!(memory.compare_exchange(0x8100_0011, 1, 0, 0xFF));
        assert_eq!(memory.read32(0x8100_0010), 0xFF09);
    }
}
