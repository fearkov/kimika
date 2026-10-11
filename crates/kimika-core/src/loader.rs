//! loading a game and the modules it loads: each one's segments placed,
//! where it was linked if it has to sit there and in blocks of the kernel's
//! otherwise, relocated there, and its imports linked, to the modules that
//! export them or to the functions done here.
//!
//! a game starts with its eboot.bin and the libraries the system loads
//! with it from its sce_module folder, whose module_start functions its
//! main thread runs before its own.

use std::path::Path;
use std::sync::atomic::AtomicU32;
use std::sync::Arc;

use vita_module::game::{Game, EBOOT};
use vita_module::{nid, Module};

use crate::hle::threads::{make_thread, DEFAULT_PRIORITY, DEFAULT_STACK_SIZE};
use crate::kernel::errors;
use crate::kernel::memblock::MAIN;
use crate::kernel::modules::{self, LoadedModule, LoadedSegment, READY, STARTED};
use crate::kernel::thread::{self, RETURN_SVC, RETURN_TRAP};
use crate::vfs::{App, Vfs};
use crate::{Config, Shared, System};

#[derive(Debug, thiserror::Error)]
pub enum LoadError {
    #[error(transparent)]
    Module(#[from] vita_module::Error),
    #[error("{0}")]
    Place(String),
}

impl LoadError {
    /// the error the module manager answers a game with.
    pub fn code(&self) -> u32 {
        match self {
            LoadError::Module(vita_module::Error::Io(_) | vita_module::Error::NotFound(_)) => errors::MODULEMGR_NOENT,
            LoadError::Module(_) => errors::ILLEGAL_ELF_HEADER,
            LoadError::Place(_) => errors::MODULEMGR_NOMEM,
        }
    }
}

/// svc #index, in ARM.
const SVC: u32 = 0xEF00_0000;
/// SceLibKernel's __stack_chk_guard, the canary stack protection checks.
const STACK_CHECK_GUARD: (u32, u32) = (0xCAE9_ACE6, 0x93B8_AA67);

/// the libraries the system loads with a program from its sce_module
/// folder, each with the bit of the process parameters' preload inhibit
/// that keeps it from loading. the others it would load come with the
/// system rather than the game, and are done here.
const PRELOADED: &[(&str, u32)] = &[("libc", 0x1_0000), ("libfios2", 0x20_0000)];

/// loads a game, a folder or a .vpk or .zip with an eboot.bin or an
/// eboot.bin by itself, and starts it. a .vpk is a zip with another name,
/// so either opens the same way.
pub fn load(path: &Path, config: Config) -> Result<System, LoadError> {
    let package = path
        .extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("vpk") || extension.eq_ignore_ascii_case("zip"));
    let (app, bytes) = if path.is_dir() || package {
        let game = Game::open(path)?;
        log::info!("{} {}", game.title_id().unwrap_or("(no title id)"), game.title().unwrap_or("(no title)"));
        let bytes = game.read(EBOOT)?;
        (Some(App { game, folder: path.is_dir().then(|| path.to_owned()) }), bytes)
    } else {
        (None, std::fs::read(path).map_err(vita_module::Error::Io)?)
    };
    let vfs = Vfs::new(app, config.data_dir.clone());
    vfs.prepare();
    let shared = Arc::new(Shared::new(config, vfs));
    let memory = &shared.memory;
    if !memory.commit(RETURN_TRAP, 0x1000) {
        return Err(LoadError::Place("could not commit the kernel's page".to_owned()));
    }
    memory.write32(RETURN_TRAP, SVC | RETURN_SVC);
    shared.kernel.blocks.lock().unwrap().reserve(RETURN_TRAP, 0x1000);

    let program = load_module(&shared, &bytes, &format!("app0:{EBOOT}"))?;
    let inhibit = program.process_param.and_then(|param| param_word(&shared, param, 0x24)).unwrap_or(0);
    let mut first = Vec::new();
    for &(name, bit) in PRELOADED {
        let path = format!("app0:sce_module/{name}.suprx");
        if inhibit & bit != 0 {
            continue;
        }
        let Ok(bytes) = shared.vfs.read(&path) else { continue };
        let module = load_module(&shared, &bytes, &path)?;
        module.set_state(STARTED);
        first.extend(module.start);
    }
    start(&shared, &program, first)?;
    Ok(System::new(shared))
}

/// a field of the process parameters that holds the address of a variable
/// of the program's, none when it is zero or past the end of the
/// structure, which older programs have shorter.
fn param_pointer(shared: &Shared, param: u32, field: u32) -> Option<u32> {
    if field + 4 > shared.memory.read32(param) {
        return None;
    }
    Some(shared.memory.read32(param + field)).filter(|&address| address != 0)
}

/// the word such a field leads to.
fn param_word(shared: &Shared, param: u32, field: u32) -> Option<u32> {
    param_pointer(shared, param, field).map(|address| shared.memory.read32(address))
}

/// loads a module from the bytes of its file: places, relocates and links
/// it, and gives it its imports from the other modules. it is not started.
pub fn load_module(shared: &Arc<Shared>, bytes: &[u8], path: &str) -> Result<Arc<LoadedModule>, LoadError> {
    let module = Module::parse(bytes)?;
    log::info!(
        "{path}: {} {}.{}, {} segments, {} relocations",
        module.info.name,
        module.info.version.0,
        module.info.version.1,
        module.segments.len(),
        module.relocations.len()
    );
    let (bases, blocks) = place(shared, &module)?;
    let mut placed = match module.placed(&bases) {
        Ok(placed) => placed,
        Err(error) => {
            give_back(shared, &blocks);
            return Err(error.into());
        }
    };
    for segment in placed.segments.iter().filter(|segment| segment.is_load()) {
        shared.memory.write_bytes(segment.address, &segment.bytes);
    }
    if placed.info.tls.is_some() {
        log::warn!("{} has thread local storage of its own, which is not set up yet", placed.info.name);
    }

    let noname = placed.noname();
    let function = |nid: u32| noname.and_then(|export| export.function(nid));
    let (start, stop, exit) =
        (placed.start(), function(nid::MODULE_STOP).or(placed.info.stop), function(nid::MODULE_EXIT));
    let process_param = noname.and_then(|export| export.variable(nid::PROCESS_PARAM));
    let loaded = Arc::new(LoadedModule {
        uid: shared.kernel.uid(),
        name: placed.info.name.clone(),
        path: path.to_owned(),
        attributes: placed.info.attributes,
        version: placed.info.version,
        segments: placed
            .segments
            .iter()
            .zip(&module.segments)
            .filter(|(segment, _)| segment.is_load())
            .map(|(segment, linked)| LoadedSegment {
                address: segment.address,
                memory_size: segment.memory_size,
                file_size: linked.bytes.len() as u32,
                flags: segment.flags,
            })
            .collect(),
        start,
        stop,
        exit,
        process_param,
        exidx: placed.info.exidx.clone(),
        extab: placed.info.extab.clone(),
        tls: placed.info.tls.take(),
        exports: std::mem::take(&mut placed.exports).into_iter().filter(|export| !export.is_noname()).collect(),
        blocks,
        state: AtomicU32::new(READY),
    });
    shared.kernel.modules.lock().unwrap().add(&shared.memory, Arc::clone(&loaded));
    link(shared, &loaded, &placed);
    Ok(loaded)
}

/// where each segment of a module goes, by index, and the blocks taken
/// for them. a fixed module sits where it was linked; a relocatable one
/// too when that room is free, as the system places a program, and
/// elsewhere otherwise, its segments all moved by the same amount, since
/// the vitasdk links code that reaches its data relative to itself and
/// leaves no relocation for it.
fn place(shared: &Shared, module: &Module) -> Result<(Vec<u32>, Vec<u32>), LoadError> {
    let loadable = || module.segments.iter().filter(|segment| segment.is_load() && segment.memory_size != 0);
    let start = loadable().map(|segment| segment.address & !0xFFF).min();
    let end =
        loadable().map(|segment| (segment.address as u64 + segment.memory_size as u64).next_multiple_of(0x1000)).max();
    let (Some(start), Some(end)) = (start, end) else {
        return Ok((module.segments.iter().map(|segment| segment.address).collect(), Vec::new()));
    };
    let size =
        u32::try_from(end - start as u64).map_err(|_| LoadError::Place("a module bigger than memory".to_owned()))?;
    let mut blocks = shared.kernel.blocks.lock().unwrap();
    let (base, taken) = if module.is_fixed() {
        blocks.reserve(start, size);
        (start, Vec::new())
    } else if blocks.take_at(start, size) {
        (start, vec![start])
    } else {
        let align = loadable().map(|segment| segment.align).max().unwrap_or(0).clamp(0x1000, 0x10_0000);
        let base = blocks
            .take(MAIN, size, align.next_power_of_two())
            .ok_or_else(|| LoadError::Place(format!("no room for a module of 0x{size:X} bytes")))?;
        (base, vec![base])
    };
    drop(blocks);
    if !shared.memory.commit(base, size) {
        give_back(shared, &taken);
        return Err(LoadError::Place(format!("could not commit memory at 0x{base:08X}")));
    }
    let delta = base.wrapping_sub(start);
    Ok((module.segments.iter().map(|segment| segment.address.wrapping_add(delta)).collect(), taken))
}

/// gives the kernel back the blocks a module took, and their memory.
pub fn give_back(shared: &Shared, taken: &[u32]) {
    let mut blocks = shared.kernel.blocks.lock().unwrap();
    for &base in taken {
        if let Some(size) = blocks.untake(base) {
            shared.memory.decommit(base, size);
        }
    }
}

/// links every function a module imports, through its stub, and every
/// variable, at the places that take its address.
fn link(shared: &Shared, loaded: &LoadedModule, placed: &Module) {
    let memory = &shared.memory;
    let place = |reference: &vita_module::Reference| {
        placed.segments.get(reference.segment as usize).map(|segment| segment.address.wrapping_add(reference.offset))
    };
    let mut modules = shared.kernel.modules.lock().unwrap();
    let (mut to_modules, mut done, mut missing) = (0, 0, 0);
    for import in &placed.imports {
        for function in &import.functions {
            let svc = shared.hle.add_import(import.nid, function.nid);
            modules.import_function(memory, loaded.uid, (import.nid, function.nid), function.stub, svc);
            for reference in &function.references {
                if let Some(place) = place(reference) {
                    modules::patch(memory, place, reference.kind, function.stub.wrapping_add(reference.addend as u32));
                }
            }
            if modules.exports(import.nid, function.nid) {
                to_modules += 1;
            } else if crate::hle::find(import.nid, function.nid).is_some() {
                done += 1;
            } else {
                missing += 1;
            }
        }
        for variable in &import.variables {
            let places = variable
                .references
                .iter()
                .filter_map(|reference| Some((place(reference)?, reference.kind, reference.addend)))
                .collect();
            let ids = (import.nid, variable.nid);
            let new_page = || {
                let address = shared.kernel.blocks.lock().unwrap().take(MAIN, 0x1000, 0x1000)?;
                memory.commit(address, 0x1000).then_some(address)
            };
            if let Some(address) = modules.import_variable(memory, loaded.uid, ids, places, new_page) {
                if ids == STACK_CHECK_GUARD {
                    memory.write32(address, 0x6B69_6D69);
                }
                log::debug!(
                    "{} {} at 0x{address:08X}, kept by the kernel",
                    import.name.as_deref().unwrap_or("?"),
                    nid::name(import.nid, variable.nid).unwrap_or("?")
                );
            }
        }
        if !import.tls.is_empty() {
            log::warn!(
                "{} imports thread local variables, which are not linked yet",
                import.name.as_deref().unwrap_or("?")
            );
        }
    }
    log::info!(
        "{}: {to_modules} imported functions linked to modules, {done} done here, {missing} not yet",
        loaded.name
    );
}

/// makes the main thread from the process parameters and starts it at the
/// program's module_start, with no arguments, as a plain launch does,
/// after the functions in first.
fn start(shared: &Arc<Shared>, program: &LoadedModule, first: Vec<u32>) -> Result<(), LoadError> {
    let entry = program.start.ok_or_else(|| LoadError::Place(format!("{} has no module_start", program.name)))?;
    let param = program.process_param;
    let word = |field: u32| param.and_then(|param| param_word(shared, param, field));
    let name = param
        .and_then(|param| param_pointer(shared, param, 0x10))
        .and_then(|address| shared.memory.read_cstring(address, 32))
        .unwrap_or_else(|| "main_thread".to_owned());
    let thread = make_thread(
        shared,
        &name,
        entry,
        word(0x14).unwrap_or(DEFAULT_PRIORITY),
        word(0x18).unwrap_or(DEFAULT_STACK_SIZE),
        word(0x28).unwrap_or(0),
        word(0x1C).unwrap_or(0),
        true,
    )
    .map_err(|error| LoadError::Place(format!("no room for the main thread, 0x{error:08X}")))?;
    program.set_state(STARTED);
    thread::start_after(shared, &thread, &[], first)
        .map_err(|error| LoadError::Place(format!("the main thread did not start, 0x{error:08X}")))
}
