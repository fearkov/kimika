//! SceSysmem, the memory blocks a game allocates: committed when they are
//! made, zeroed as the console zeroes them, given back when freed; and
//! what the kernel says of the console itself: its model, its firmware,
//! its free memory, its random numbers.

use super::{Call, Function};
use crate::kernel::errors;

pub const FUNCTIONS: &[(&str, Function)] = &[
    ("sceKernelAllocMemBlock", alloc_mem_block),
    ("sceKernelFreeMemBlock", free_mem_block),
    ("sceKernelGetMemBlockBase", get_mem_block_base),
    ("sceKernelFindMemBlockByAddr", find_mem_block_by_addr),
    ("sceKernelGetMemBlockInfoByAddr", get_mem_block_info_by_addr),
    ("sceKernelGetMemBlockInfoByRange", get_mem_block_info_by_range),
    ("sceKernelGetFreeMemorySize", get_free_memory_size),
    ("sceKernelGetModel", get_model),
    ("sceKernelGetModelForCDialog", get_model),
    ("sceKernelIsPSVitaTV", is_vita_tv),
    ("sceKernelGetSystemSwVersion", get_system_sw_version),
    ("sceKernelGetRandomNumber", get_random_number),
    ("sceKernelGetCpuId", get_cpu_id),
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

/// writes a SceKernelMemBlockInfo for the block an address is in.
fn write_block_info(call: &mut Call, address: u32, info: u32) {
    let found =
        call.shared.kernel.blocks.lock().unwrap().find(address).map(|block| (block.base, block.size, block.kind));
    let Some((base, size, kind)) = found else {
        return call.ret(errors::ILLEGAL_ADDR);
    };
    let memory = call.memory();
    let access = if kind & 0xF0_0000 == 0x60_0000 { 0x6 } else { 0x7 };
    let words = [0x18, base, size, (kind >> 24) & 0xF, access, kind];
    for (index, word) in words.into_iter().enumerate() {
        memory.write32(info + index as u32 * 4, word);
    }
    call.ret(0u32);
}

/// sceKernelGetMemBlockInfoByAddr(address, &info)
fn get_mem_block_info_by_addr(call: &mut Call) {
    let (address, info) = (call.u32(), call.u32());
    write_block_info(call, address, info);
}

/// sceKernelGetMemBlockInfoByRange(address, size, &info)
fn get_mem_block_info_by_range(call: &mut Call) {
    let (address, _size, info) = (call.u32(), call.u32(), call.u32());
    write_block_info(call, address, info);
}

/// sceKernelGetFreeMemorySize(&info): the room left in user memory, video
/// memory and the physically contiguous kind, all of it main memory here.
fn get_free_memory_size(call: &mut Call) {
    let info = call.u32();
    let memory = call.memory();
    memory.write32(info + 4, 0x0C00_0000);
    memory.write32(info + 8, 0x0700_0000);
    memory.write32(info + 12, 0x0100_0000);
    call.ret(0u32);
}

/// sceKernelGetModel(): SCE_KERNEL_MODEL_VITA, a handheld.
fn get_model(call: &mut Call) {
    call.ret(0x1_0000u32);
}

fn is_vita_tv(call: &mut Call) {
    call.ret(0u32);
}

/// sceKernelGetSystemSwVersion(&version): firmware 3.60.
fn get_system_sw_version(call: &mut Call) {
    let version = call.u32();
    let memory = call.memory();
    memory.fill(version + 4, 0, 0x24);
    memory.write_bytes(version + 4, b"3.60");
    memory.write32(version + 0x20, 0x0360_0000);
    call.ret(0u32);
}

/// sceKernelGetRandomNumber(buffer, size), from a generator seeded by the
/// clock.
fn get_random_number(call: &mut Call) {
    let (buffer, size) = (call.u32(), call.u32());
    if size > 64 {
        return call.ret(errors::INVALID_ARGUMENT_SIZE);
    }
    let mut state = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0x9E37_79B9_7F4A_7C15, |now| now.as_nanos() as u64)
        ^ buffer as u64;
    let bytes: Vec<u8> = (0..size)
        .map(|_| {
            // xorshift64*
            state ^= state >> 12;
            state ^= state << 25;
            state ^= state >> 27;
            (state.wrapping_mul(0x2545_F491_4F6C_DD1D) >> 56) as u8
        })
        .collect();
    call.memory().write_bytes(buffer, &bytes);
    call.ret(0u32);
}

/// sceKernelGetCpuId(): the core the thread is on, as far as anyone knows.
fn get_cpu_id(call: &mut Call) {
    call.ret(0u32);
}
