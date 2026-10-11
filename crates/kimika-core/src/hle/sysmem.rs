//! SceSysmem, the memory blocks a game allocates: committed when they are
//! made, zeroed as the console zeroes them, given back when freed.

use super::{Call, Function};
use crate::kernel::errors;

pub const FUNCTIONS: &[(&str, Function)] = &[
    ("sceKernelAllocMemBlock", alloc_mem_block),
    ("sceKernelFreeMemBlock", free_mem_block),
    ("sceKernelGetMemBlockBase", get_mem_block_base),
    ("sceKernelFindMemBlockByAddr", find_mem_block_by_addr),
];

/// SceKernelAllocMemBlockOpt's attribute that asks for an alignment.
const HAS_ALIGNMENT: u32 = 0x4;

/// sceKernelAllocMemBlock(name, type, size, option) -> UID
fn alloc_mem_block(call: &mut Call) {
    let (name, kind, size, option) = (call.string(), call.u32(), call.u32(), call.u32());
    let Some(name) = name else {
        return call.ret(errors::ILLEGAL_ADDR);
    };
    let memory = call.memory();
    let alignment = (option != 0 && memory.read32(option + 4) & HAS_ALIGNMENT != 0).then(|| memory.read32(option + 8));
    let kernel = &call.shared.kernel;
    let uid = kernel.uid();
    let mut blocks = kernel.blocks.lock().unwrap();
    let (base, size) = match blocks.allocate(uid, &name, kind, size, alignment) {
        Ok(block) => (block.base, block.size),
        Err(error) => {
            log::warn!(
                "{}: no memory block \"{name}\" of type 0x{kind:08X} and 0x{size:X} bytes, 0x{error:08X}",
                call.thread.name
            );
            return call.ret(error);
        }
    };
    if !memory.commit(base, size) {
        let _ = blocks.free(uid);
        log::error!("the host would not commit 0x{size:X} bytes for \"{name}\"");
        return call.ret(errors::NO_FREE_PHYSICAL_PAGE);
    }
    log::debug!("{}: memory block \"{name}\" 0x{uid:08X} at 0x{base:08X}, 0x{size:X} bytes", call.thread.name);
    call.ret(uid);
}

/// sceKernelFreeMemBlock(uid)
fn free_mem_block(call: &mut Call) {
    let uid = call.u32();
    let freed = call.shared.kernel.blocks.lock().unwrap().free(uid);
    match freed {
        Ok(block) => {
            call.memory().decommit(block.base, block.size);
            call.ret(0u32);
        }
        Err(error) => call.ret(error),
    }
}

/// sceKernelGetMemBlockBase(uid, &base)
fn get_mem_block_base(call: &mut Call) {
    let (uid, out) = (call.u32(), call.u32());
    let base = call.shared.kernel.blocks.lock().unwrap().get(uid).map(|block| block.base);
    match base {
        Some(base) => {
            call.memory().write32(out, base);
            call.ret(0u32);
        }
        None => call.ret(errors::INVALID_UID),
    }
}

/// sceKernelFindMemBlockByAddr(address, size) -> UID
fn find_mem_block_by_addr(call: &mut Call) {
    let address = call.u32();
    let uid = call.shared.kernel.blocks.lock().unwrap().find(address).map(|block| block.uid);
    call.ret(uid.unwrap_or(errors::INVALID_UID));
}
