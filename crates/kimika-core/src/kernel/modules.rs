//! the modules in the process, the program and the .suprx files loaded with
//! it or after: where each one sits and what it exports, and every import
//! of theirs, which goes to whichever module exports its library or, when
//! none does, to the functions done here. a module loaded later takes over
//! the imports of the libraries it exports, from every module, and one
//! unloaded gives them back.

use std::collections::HashMap;
use std::ops::Range;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

use vita_module::{relocation, Export, Tls};

use super::Uid;
use crate::memory::Memory;

/// SCE_KERNEL_MODULE_STATE_READY, loaded and not started.
pub const READY: u32 = 2;
/// SCE_KERNEL_MODULE_STATE_STARTED.
pub const STARTED: u32 = 6;
/// SCE_KERNEL_MODULE_STATE_ENDED, stopped.
pub const ENDED: u32 = 9;

/// svc #index, in ARM.
const SVC: u32 = 0xEF00_0000;
/// bx lr, in ARM.
const RETURN: u32 = 0xE12F_FF1E;
/// ldr pc, [pc, #-4], in ARM: a jump to the word after it, to Thumb when
/// the word is odd.
const JUMP: u32 = 0xE51F_F004;

/// a module as the kernel keeps it once loaded.
pub struct LoadedModule {
    pub uid: Uid,
    pub name: String,
    /// where it was loaded from, as the game named it.
    pub path: String,
    pub attributes: u16,
    pub version: (u8, u8),
    pub segments: Vec<LoadedSegment>,
    /// module_start, module_stop and module_exit, odd for Thumb.
    pub start: Option<u32>,
    pub stop: Option<u32>,
    pub exit: Option<u32>,
    pub exidx: Range<u32>,
    pub extab: Range<u32>,
    pub tls: Option<Tls>,
    /// the libraries it exports, its own unnamed one left out.
    pub exports: Vec<Export>,
    /// a program's process parameters, none for a library.
    pub process_param: Option<u32>,
    /// the blocks it took from the kernel, by their base, given back when
    /// it goes.
    pub blocks: Vec<u32>,
    /// READY, STARTED or ENDED.
    pub state: AtomicU32,
}

impl LoadedModule {
    pub fn state(&self) -> u32 {
        self.state.load(Ordering::Relaxed)
    }

    pub fn set_state(&self, state: u32) {
        self.state.store(state, Ordering::Relaxed);
    }

    pub fn contains(&self, address: u32) -> bool {
        self.segments.iter().any(|segment| address.wrapping_sub(segment.address) < segment.memory_size)
    }

    /// the address a library it exports has for a function or a variable.
    fn export(&self, library: u32, nid: u32, variable: bool) -> Option<u32> {
        let export = self.exports.iter().find(|export| export.nid == library)?;
        if variable {
            export.variable(nid)
        } else {
            export.function(nid)
        }
    }
}

/// a loadable segment of a loaded module.
#[derive(Debug, Clone, Copy)]
pub struct LoadedSegment {
    pub address: u32,
    pub memory_size: u32,
    pub file_size: u32,
    /// the program header's flags: 4 readable, 2 writable, 1 executable.
    pub flags: u32,
}

/// an imported function: its stub, which jumps to the module exporting it
/// or calls the function done here through an svc.
struct Stub {
    module: Uid,
    library: u32,
    nid: u32,
    address: u32,
    /// the svc that calls the function done here.
    svc: u32,
}

/// an imported variable: the places that take its address.
struct Variable {
    module: Uid,
    library: u32,
    nid: u32,
    /// each place's address, its relocation kind and its addend.
    places: Vec<(u32, u8, i32)>,
}

#[derive(Default)]
pub struct Modules {
    /// in the order they were loaded, the program first.
    pub loaded: Vec<Arc<LoadedModule>>,
    stubs: Vec<Stub>,
    variables: Vec<Variable>,
    /// where the variables no module exports live, by library and NID.
    kernel_variables: HashMap<(u32, u32), u32>,
    /// the page of the kernel's they are given room in, and how many it
    /// holds so far.
    variable_page: Option<(u32, u32)>,
}

/// how much room each variable the kernel keeps gets, zeroed.
const KERNEL_VARIABLE_SIZE: u32 = 0x40;

impl Modules {
    pub fn get(&self, uid: Uid) -> Option<Arc<LoadedModule>> {
        self.loaded.iter().find(|module| module.uid == uid).cloned()
    }

    /// the module an address is in.
    pub fn by_address(&self, address: u32) -> Option<Arc<LoadedModule>> {
        self.loaded.iter().find(|module| module.contains(address)).cloned()
    }

    /// what a module loaded exports for a function or a variable of a
    /// library, the one loaded first winning.
    fn export(&self, library: u32, nid: u32, variable: bool) -> Option<u32> {
        self.loaded.iter().find_map(|module| module.export(library, nid, variable))
    }

    /// adds a module whose own imports are not linked yet, and gives it the
    /// imports of every other module that it exports.
    pub fn add(&mut self, memory: &Memory, module: Arc<LoadedModule>) {
        let libraries: Vec<u32> = module.exports.iter().map(|export| export.nid).collect();
        self.loaded.push(module);
        self.relink(memory, &libraries);
    }

    /// takes a module out, with its imports, and gives the imports of every
    /// other module that it exported back to whatever else exports them, or
    /// to the functions done here.
    pub fn remove(&mut self, memory: &Memory, uid: Uid) -> Option<Arc<LoadedModule>> {
        let index = self.loaded.iter().position(|module| module.uid == uid)?;
        let module = self.loaded.remove(index);
        self.stubs.retain(|stub| stub.module != uid);
        self.variables.retain(|variable| variable.module != uid);
        let libraries: Vec<u32> = module.exports.iter().map(|export| export.nid).collect();
        self.relink(memory, &libraries);
        Some(module)
    }

    /// links a module's imported function, whose stub is at address, to
    /// the module that exports it or to the svc given.
    pub fn import_function(
        &mut self,
        memory: &Memory,
        module: Uid,
        (library, nid): (u32, u32),
        address: u32,
        svc: u32,
    ) {
        let stub = Stub { module, library, nid, address, svc };
        self.write_stub(memory, &stub);
        self.stubs.push(stub);
    }

    /// links a module's imported variable to the module that exports it or,
    /// when none does, to a place of the kernel's, made the first time the
    /// variable is asked for in a page new_page gives when one fills up,
    /// and given back then for the caller to set.
    pub fn import_variable(
        &mut self,
        memory: &Memory,
        module: Uid,
        (library, nid): (u32, u32),
        places: Vec<(u32, u8, i32)>,
        new_page: impl FnOnce() -> Option<u32>,
    ) -> Option<u32> {
        let mut made = None;
        if !self.kernel_variables.contains_key(&(library, nid)) && self.export(library, nid, true).is_none() {
            let page = match self.variable_page {
                Some((page, count)) if count < 0x1000 / KERNEL_VARIABLE_SIZE => Some((page, count)),
                _ => new_page().map(|page| (page, 0)),
            };
            if let Some((page, count)) = page {
                let address = page + count * KERNEL_VARIABLE_SIZE;
                self.variable_page = Some((page, count + 1));
                self.kernel_variables.insert((library, nid), address);
                made = Some(address);
            }
        }
        let variable = Variable { module, library, nid, places };
        self.patch_variable(memory, &variable);
        self.variables.push(variable);
        made
    }

    /// whether a function an import names is exported by a module loaded.
    pub fn exports(&self, library: u32, nid: u32) -> bool {
        self.export(library, nid, false).is_some()
    }

    /// links the imports of libraries again, after the modules exporting
    /// them changed.
    fn relink(&mut self, memory: &Memory, libraries: &[u32]) {
        if libraries.is_empty() {
            return;
        }
        for stub in self.stubs.iter().filter(|stub| libraries.contains(&stub.library)) {
            self.write_stub(memory, stub);
        }
        for variable in self.variables.iter().filter(|variable| libraries.contains(&variable.library)) {
            self.patch_variable(memory, variable);
        }
    }

    fn write_stub(&self, memory: &Memory, stub: &Stub) {
        match self.export(stub.library, stub.nid, false) {
            Some(target) => {
                memory.write32(stub.address, JUMP);
                memory.write32(stub.address + 4, target);
            }
            None => {
                memory.write32(stub.address, SVC | stub.svc);
                memory.write32(stub.address + 4, RETURN);
            }
        }
    }

    fn patch_variable(&self, memory: &Memory, variable: &Variable) {
        let target = self
            .export(variable.library, variable.nid, true)
            .or_else(|| self.kernel_variables.get(&(variable.library, variable.nid)).copied())
            .unwrap_or(0);
        for &(place, kind, addend) in &variable.places {
            patch(memory, place, kind, target.wrapping_add(addend as u32));
        }
    }
}

/// writes value into the instruction or word at place, as a relocation of
/// kind would.
pub fn patch(memory: &Memory, place: u32, kind: u8, value: u32) {
    let word = memory.read32(place);
    match relocation::patch(kind, word, place, value) {
        Some(patched) => memory.write32(place, patched),
        None => log::warn!("a reference of kind {kind} at 0x{place:08X} cannot be patched"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LIBRARY: u32 = 0x1234_5678;
    const FUNCTION: u32 = 0xAAAA_0001;
    const VARIABLE: u32 = 0xAAAA_0002;
    const STUB: u32 = 0x8100_0000;
    const PLACE: u32 = 0x8100_0100;

    fn exporter(uid: Uid) -> Arc<LoadedModule> {
        Arc::new(LoadedModule {
            uid,
            name: "plugin".to_owned(),
            path: "app0:plugin.suprx".to_owned(),
            attributes: 0,
            version: (1, 0),
            segments: vec![LoadedSegment { address: 0x8200_0000, memory_size: 0x1000, file_size: 0x1000, flags: 5 }],
            start: None,
            stop: None,
            exit: None,
            exidx: 0..0,
            extab: 0..0,
            tls: None,
            exports: vec![Export {
                name: Some("Plugin".to_owned()),
                nid: LIBRARY,
                version: 1,
                attributes: 1,
                functions: vec![(FUNCTION, 0x8200_0011)],
                variables: vec![(VARIABLE, 0x8200_0800)],
                tls: Vec::new(),
            }],
            process_param: None,
            blocks: vec![0x8200_0000],
            state: AtomicU32::new(READY),
        })
    }

    #[test]
    fn imports_follow_the_module_exporting_them_in_and_out() {
        let memory = Memory::new();
        assert!(memory.commit(0x8100_0000, 0x2000));
        let mut modules = Modules::default();
        // nothing exports them yet: an svc, and a place of the kernel's
        modules.import_function(&memory, 1, (LIBRARY, FUNCTION), STUB, 7);
        let made = modules.import_variable(&memory, 1, (LIBRARY, VARIABLE), vec![(PLACE, 2, 4)], || Some(0x8100_1000));
        assert_eq!(made, Some(0x8100_1000));
        assert_eq!((memory.read32(STUB), memory.read32(STUB + 4)), (SVC | 7, RETURN));
        assert_eq!(memory.read32(PLACE), 0x8100_1004);

        // the module comes, and they go to it
        modules.add(&memory, exporter(3));
        assert_eq!((memory.read32(STUB), memory.read32(STUB + 4)), (JUMP, 0x8200_0011));
        assert_eq!(memory.read32(PLACE), 0x8200_0804);
        assert!(modules.exports(LIBRARY, FUNCTION));
        assert_eq!(modules.by_address(0x8200_0010).map(|module| module.uid), Some(3));

        // and goes, and they come back
        assert!(modules.remove(&memory, 3).is_some());
        assert_eq!((memory.read32(STUB), memory.read32(STUB + 4)), (SVC | 7, RETURN));
        assert_eq!(memory.read32(PLACE), 0x8100_1004);
        assert!(!modules.exports(LIBRARY, FUNCTION));
    }

    #[test]
    fn a_module_loaded_first_links_the_imports_of_those_after_it() {
        let memory = Memory::new();
        assert!(memory.commit(0x8100_0000, 0x2000));
        let mut modules = Modules::default();
        modules.add(&memory, exporter(3));
        modules.import_function(&memory, 5, (LIBRARY, FUNCTION), STUB, 9);
        let made = modules.import_variable(&memory, 5, (LIBRARY, VARIABLE), vec![(PLACE, 2, 0)], || None);
        assert_eq!(made, None);
        assert_eq!((memory.read32(STUB), memory.read32(STUB + 4)), (JUMP, 0x8200_0011));
        assert_eq!(memory.read32(PLACE), 0x8200_0800);
    }
}
