//! SceModulemgr and the parts of SceLibKernel built on it: loading .suprx
//! modules, starting and stopping them on the calling thread, and what the
//! kernel knows of every module loaded; and SceSysmodule, whose modules
//! come with the system and are done here, so loading one does nothing.

use std::sync::Arc;

use super::{Call, Function};
use crate::kernel::errors;
use crate::kernel::modules::{LoadedModule, ENDED, STARTED};
use crate::loader;

pub const FUNCTIONS: &[(&str, Function)] = &[
    ("sceKernelLoadModule", load_module),
    ("_sceKernelLoadModule", load_module),
    ("sceKernelStartModule", start_module),
    ("sceKernelLoadStartModule", load_start_module),
    ("_sceKernelLoadStartModule", load_start_module_without_status),
    ("sceKernelStopModule", stop_module),
    ("_sceKernelStopModule", stop_module_without_status),
    ("sceKernelUnloadModule", unload_module),
    ("_sceKernelUnloadModule", unload_module),
    ("sceKernelStopUnloadModule", stop_unload_module),
    ("_sceKernelStopUnloadModule", stop_unload_module_without_status),
    ("sceKernelGetModuleList", get_module_list),
    ("sceKernelGetModuleInfo", get_module_info),
    ("sceKernelGetModuleInfoByAddr", get_module_info_by_address),
    ("sceKernelGetModuleIdByAddr", get_module_id_by_address),
    ("sceKernelGetProcessParam", get_process_param),
    ("sceSysmoduleLoadModule", sysmodule),
    ("sceSysmoduleUnloadModule", sysmodule),
    ("sceSysmoduleIsLoaded", sysmodule),
    ("sceSysmoduleLoadModuleInternal", sysmodule),
    ("sceSysmoduleUnloadModuleInternal", sysmodule),
    ("sceSysmoduleIsLoadedInternal", sysmodule),
    ("sceSysmoduleLoadModuleInternalWithArg", sysmodule),
    ("sceSysmoduleUnloadModuleInternalWithArg", sysmodule),
];

/// what module_start answers: SCE_KERNEL_START_NO_RESIDENT, done and not
/// staying, and SCE_KERNEL_START_FAILED.
const NO_RESIDENT: u32 = 1;
const START_FAILED: u32 = 2;
/// what module_stop answers to stay, SCE_KERNEL_STOP_FAIL.
const STOP_FAIL: u32 = 1;

/// how big a SceKernelModuleInfo is, and a SceKernelSegmentInfo in it.
const MODULE_INFO_SIZE: u32 = 0x1B8;
const SEGMENT_INFO_SIZE: u32 = 0x18;

fn module(call: &Call, uid: u32) -> Option<Arc<LoadedModule>> {
    call.shared.kernel.modules.lock().unwrap().get(uid)
}

/// loads a module from a path of the game's.
fn load(call: &Call, path: &str) -> Result<Arc<LoadedModule>, u32> {
    let bytes = call.shared.vfs.read(path).map_err(|_| errors::MODULEMGR_NOENT)?;
    loader::load_module(call.shared, &bytes, path).map_err(|error| {
        log::warn!("{}: {path} does not load: {error}", call.thread.name);
        error.code()
    })
}

/// runs a module's module_start on the calling thread, giving what it
/// returned. a module that will not stay goes again.
fn start(call: &mut Call, module: &LoadedModule, args: u32, argp: u32) -> Result<u32, u32> {
    if module.state() == STARTED {
        return Err(errors::MODULEMGR_STARTED);
    }
    let Some(entry) = module.start else {
        module.set_state(STARTED);
        return Ok(0);
    };
    log::debug!("{}: starting {} at 0x{entry:08X}", call.thread.name, module.name);
    // a thread that ends inside module_start ends once this call returns
    let result = call.call_guest(entry, &[args, argp]).unwrap_or(0);
    module.set_state(STARTED);
    if result == NO_RESIDENT || result == START_FAILED {
        module.set_state(ENDED);
        unload(call, module.uid).ok();
    }
    if result == START_FAILED {
        return Err(errors::MODULEMGR_START_FAILED);
    }
    Ok(result)
}

/// runs a module's module_stop on the calling thread, giving what it
/// returned.
fn stop(call: &mut Call, module: &LoadedModule, args: u32, argp: u32) -> Result<u32, u32> {
    if module.state() != STARTED {
        return Err(errors::MODULEMGR_NOT_STARTED);
    }
    let result = match module.stop {
        Some(entry) => call.call_guest(entry, &[args, argp]).unwrap_or(0),
        None => 0,
    };
    if result == STOP_FAIL {
        return Err(errors::MODULEMGR_STOP_FAIL);
    }
    module.set_state(ENDED);
    Ok(result)
}

/// takes a module out of the process, its imports and exports unlinked
/// and its memory given back. the program cannot go.
fn unload(call: &Call, uid: u32) -> Result<(), u32> {
    let shared = call.shared;
    let module = {
        let mut modules = shared.kernel.modules.lock().unwrap();
        let module = modules.get(uid).ok_or(errors::MODULEMGR_NO_MOD)?;
        if module.state() == STARTED {
            return Err(errors::MODULEMGR_NOT_STOPPED);
        }
        if module.blocks.is_empty() {
            return Err(errors::MODULEMGR_NO_MOD);
        }
        modules.remove(&shared.memory, uid).ok_or(errors::MODULEMGR_NO_MOD)?
    };
    loader::give_back(shared, &module.blocks);
    log::debug!("{}: {} unloaded", call.thread.name, module.name);
    Ok(())
}

fn write_status(call: &Call, status: u32, value: u32) {
    if status != 0 {
        call.memory().write32(status, value);
    }
}

/// sceKernelLoadModule(path, flags, option) -> UID
fn load_module(call: &mut Call) {
    let path = call.string();
    let result = path.ok_or(errors::ILLEGAL_ADDR).and_then(|path| load(call, &path)).map(|module| module.uid);
    call.result(result);
}

/// sceKernelStartModule(UID, args, argp, flags, option, status)
fn start_module(call: &mut Call) {
    let (uid, args, argp, _flags, _option, status) =
        (call.u32(), call.u32(), call.u32(), call.u32(), call.u32(), call.u32());
    let Some(module) = module(call, uid) else {
        return call.ret(errors::MODULEMGR_NO_MOD);
    };
    let result = start(call, &module, args, argp).map(|result| {
        write_status(call, status, result);
        0
    });
    call.result(result);
}

fn load_and_start(call: &mut Call, path: Option<String>, args: u32, argp: u32, status: u32) -> Result<u32, u32> {
    let module = load(call, &path.ok_or(errors::ILLEGAL_ADDR)?)?;
    let result = start(call, &module, args, argp)?;
    write_status(call, status, result);
    Ok(module.uid)
}

/// sceKernelLoadStartModule(path, args, argp, flags, option, status) -> UID
fn load_start_module(call: &mut Call) {
    let (path, args, argp, _flags, _option, status) =
        (call.string(), call.u32(), call.u32(), call.u32(), call.u32(), call.u32());
    let result = load_and_start(call, path, args, argp, status);
    call.result(result);
}

/// _sceKernelLoadStartModule(path, args, argp, flags) -> UID
fn load_start_module_without_status(call: &mut Call) {
    let (path, args, argp) = (call.string(), call.u32(), call.u32());
    let result = load_and_start(call, path, args, argp, 0);
    call.result(result);
}

fn stop_by_uid(call: &mut Call, uid: u32, args: u32, argp: u32, status: u32) -> Result<u32, u32> {
    let module = module(call, uid).ok_or(errors::MODULEMGR_NO_MOD)?;
    let result = stop(call, &module, args, argp)?;
    write_status(call, status, result);
    Ok(0)
}

/// sceKernelStopModule(UID, args, argp, flags, option, status)
fn stop_module(call: &mut Call) {
    let (uid, args, argp, _flags, _option, status) =
        (call.u32(), call.u32(), call.u32(), call.u32(), call.u32(), call.u32());
    let result = stop_by_uid(call, uid, args, argp, status);
    call.result(result);
}

/// _sceKernelStopModule(UID, args, argp, flags)
fn stop_module_without_status(call: &mut Call) {
    let (uid, args, argp) = (call.u32(), call.u32(), call.u32());
    let result = stop_by_uid(call, uid, args, argp, 0);
    call.result(result);
}

/// sceKernelUnloadModule(UID, flags, option)
fn unload_module(call: &mut Call) {
    let uid = call.u32();
    let result = unload(call, uid).map(|()| 0);
    call.result(result);
}

fn stop_and_unload(call: &mut Call, uid: u32, args: u32, argp: u32, status: u32) -> Result<u32, u32> {
    stop_by_uid(call, uid, args, argp, status)?;
    unload(call, uid)?;
    Ok(0)
}

/// sceKernelStopUnloadModule(UID, args, argp, flags, option, status)
fn stop_unload_module(call: &mut Call) {
    let (uid, args, argp, _flags, _option, status) =
        (call.u32(), call.u32(), call.u32(), call.u32(), call.u32(), call.u32());
    let result = stop_and_unload(call, uid, args, argp, status);
    call.result(result);
}

/// _sceKernelStopUnloadModule(UID, args, argp, flags)
fn stop_unload_module_without_status(call: &mut Call) {
    let (uid, args, argp) = (call.u32(), call.u32(), call.u32());
    let result = stop_and_unload(call, uid, args, argp, 0);
    call.result(result);
}

/// sceKernelGetModuleList(flags, UIDs, count): as many UIDs as count says
/// there is room for, the program's first, and count set to how many.
fn get_module_list(call: &mut Call) {
    let (_flags, uids, count) = (call.u32(), call.u32(), call.u32());
    if uids == 0 || count == 0 {
        return call.ret(errors::ILLEGAL_ADDR);
    }
    let memory = call.memory();
    let room = memory.read32(count) as usize;
    let modules = call.shared.kernel.modules.lock().unwrap();
    let listed: Vec<u32> = modules.loaded.iter().map(|module| module.uid).take(room).collect();
    drop(modules);
    for (index, uid) in listed.iter().enumerate() {
        memory.write32(uids + index as u32 * 4, *uid);
    }
    memory.write32(count, listed.len() as u32);
    call.ret(0u32);
}

/// fills a SceKernelModuleInfo.
fn write_info(call: &Call, module: &LoadedModule, info: u32) {
    let memory = call.memory();
    memory.fill(info, 0, MODULE_INFO_SIZE);
    memory.write32(info, MODULE_INFO_SIZE);
    memory.write32(info + 0x04, module.uid);
    memory.write16(info + 0x08, module.attributes);
    memory.write8(info + 0x0A, module.version.1);
    memory.write8(info + 0x0B, module.version.0);
    let mut name = module.name.as_bytes().to_vec();
    name.truncate(27);
    memory.write_bytes(info + 0x0C, &name);
    let entries = [
        (0x2C, module.start.unwrap_or(0)),
        (0x30, module.stop.unwrap_or(0)),
        (0x34, module.exit.unwrap_or(0)),
        (0x38, module.exidx.start),
        (0x3C, module.exidx.end),
        (0x40, module.extab.start),
        (0x44, module.extab.end),
    ];
    for (offset, value) in entries {
        memory.write32(info + offset, value);
    }
    if let Some(tls) = &module.tls {
        memory.write32(info + 0x48, tls.address);
        memory.write32(info + 0x4C, tls.file_size);
        memory.write32(info + 0x50, tls.memory_size);
    }
    let mut path = module.path.as_bytes().to_vec();
    path.truncate(255);
    memory.write_bytes(info + 0x54, &path);
    for (index, segment) in module.segments.iter().take(4).enumerate() {
        let at = info + 0x154 + index as u32 * SEGMENT_INFO_SIZE;
        memory.write32(at, SEGMENT_INFO_SIZE);
        // the program header's RWX the other way around, as the system has it
        let permissions = (segment.flags & 4) >> 2 | (segment.flags & 2) | (segment.flags & 1) << 2;
        memory.write32(at + 0x04, permissions);
        memory.write32(at + 0x08, segment.address);
        memory.write32(at + 0x0C, segment.memory_size);
        memory.write32(at + 0x10, segment.file_size);
    }
    memory.write32(info + 0x1B4, module.state());
}

/// sceKernelGetModuleInfo(UID, info)
fn get_module_info(call: &mut Call) {
    let (uid, info) = (call.u32(), call.u32());
    let Some(module) = module(call, uid) else {
        return call.ret(errors::MODULEMGR_NO_MOD);
    };
    if info == 0 {
        return call.ret(errors::ILLEGAL_ADDR);
    }
    write_info(call, &module, info);
    call.ret(0u32);
}

/// sceKernelGetModuleInfoByAddr(address, info)
fn get_module_info_by_address(call: &mut Call) {
    let (address, info) = (call.u32(), call.u32());
    let found = call.shared.kernel.modules.lock().unwrap().by_address(address);
    let Some(module) = found else {
        return call.ret(errors::MODULEMGR_NO_MOD);
    };
    write_info(call, &module, info);
    call.ret(0u32);
}

/// sceKernelGetModuleIdByAddr(address) -> UID
fn get_module_id_by_address(call: &mut Call) {
    let address = call.u32();
    let found = call.shared.kernel.modules.lock().unwrap().by_address(address);
    call.ret(found.map_or(errors::MODULEMGR_NO_MOD, |module| module.uid));
}

/// sceKernelGetProcessParam() -> the program's process parameters
fn get_process_param(call: &mut Call) {
    let modules = call.shared.kernel.modules.lock().unwrap();
    let param = modules.loaded.first().and_then(|program| program.process_param).unwrap_or(0);
    drop(modules);
    call.ret(param);
}

/// sceSysmoduleLoadModule(id) and the rest: the system's modules are done
/// here, so every one is there already.
fn sysmodule(call: &mut Call) {
    call.ret(0u32);
}
