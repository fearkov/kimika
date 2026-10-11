//! loading a game: its eboot.bin, from a folder, a .vpk or the file itself,
//! placed where it was linked, its imports linked to what is done here, and
//! its main thread started at module_start with the stack and priority its
//! process parameters ask for.

use std::path::Path;
use std::sync::Arc;

use vita_module::game::{Game, EBOOT};
use vita_module::{nid, relocation, Module, Reference};

use crate::hle;
use crate::hle::threads::{make_thread, DEFAULT_PRIORITY, DEFAULT_STACK_SIZE};
use crate::kernel::memblock::MAIN;
use crate::kernel::thread::{self, RETURN_SVC, RETURN_TRAP};
use crate::{Config, Shared, System};

#[derive(Debug, thiserror::Error)]
pub enum LoadError {
    #[error(transparent)]
    Module(#[from] vita_module::Error),
    #[error("{0}")]
    Place(String),
}

/// svc #index, in ARM.
const SVC: u32 = 0xEF00_0000;
/// bx lr, in ARM.
const RETURN: u32 = 0xE12F_FF1E;
/// SceLibKernel's __stack_chk_guard, the canary stack protection checks.
const STACK_CHECK_GUARD: (u32, u32) = (0xCAE9_ACE6, 0x93B8_AA67);

/// loads a game and starts it.
pub fn load(path: &Path, config: Config) -> Result<System, LoadError> {
    let bytes = if path.is_dir() || path.extension().is_some_and(|extension| extension.eq_ignore_ascii_case("vpk")) {
        let game = Game::open(path)?;
        log::info!("{} {}", game.title_id().unwrap_or("(no title id)"), game.title().unwrap_or("(no title)"));
        for module in game.modules().iter().skip(1) {
            log::warn!("{module} is not loaded yet, only eboot.bin is");
        }
        game.read(EBOOT)?
    } else {
        std::fs::read(path).map_err(vita_module::Error::Io)?
    };
    let module = Module::parse(&bytes)?;
    let shared = Arc::new(Shared::new(config));
    place(&shared, &module)?;
    link(&shared, &module);
    start(&shared, &module)?;
    Ok(System::new(shared))
}

/// copies the module's segments into memory where it was linked, relocated
/// there, and the kernel's page of code beside them.
fn place(shared: &Shared, module: &Module) -> Result<(), LoadError> {
    log::info!(
        "{} {}.{}, {} segments, {} relocations",
        module.info.name,
        module.info.version.0,
        module.info.version.1,
        module.segments.len(),
        module.relocations.len()
    );
    let bases: Vec<u32> = module.segments.iter().map(|segment| segment.address).collect();
    let images = module.relocated(&bases)?;
    let memory = &shared.memory;
    let mut blocks = shared.kernel.blocks.lock().unwrap();
    for (segment, image) in module.segments.iter().zip(&images) {
        if !segment.is_load() || image.is_empty() {
            continue;
        }
        let start = segment.address & !0xFFF;
        let end = (segment.address as u64 + image.len() as u64).next_multiple_of(0x1000) as u32;
        if !memory.commit(start, end - start) {
            return Err(LoadError::Place(format!("could not commit memory at 0x{start:08X} for the program")));
        }
        memory.write_bytes(segment.address, image);
        blocks.reserve(start, end - start);
    }
    if !memory.commit(RETURN_TRAP, 0x1000) {
        return Err(LoadError::Place("could not commit the kernel's page".to_owned()));
    }
    memory.write32(RETURN_TRAP, SVC | RETURN_SVC);
    if module.info.tls.is_some() {
        log::warn!("{} has thread local storage of its own, which is not set up yet", module.info.name);
    }
    Ok(())
}

/// turns every imported function's stub into an svc for the function done
/// here, the places that take its address into ones that take the stub's,
/// and gives every imported variable a place of its own.
fn link(shared: &Shared, module: &Module) {
    let memory = &shared.memory;
    let patch = |reference: &Reference, target: u32| {
        let Some(segment) = module.segments.get(reference.segment as usize) else { return };
        let place = segment.address.wrapping_add(reference.offset);
        let word = memory.read32(place);
        let value = target.wrapping_add(reference.addend as u32);
        match relocation::patch(reference.kind, word, place, value) {
            Some(patched) => memory.write32(place, patched),
            None => log::warn!("a reference of kind {} at 0x{place:08X} cannot be patched", reference.kind),
        }
    };
    let mut variables = None;
    let (mut done, mut missing) = (0, 0);
    for import in &module.imports {
        for function in &import.functions {
            let index = shared.hle.add_import(import.nid, function.nid);
            memory.write32(function.stub, SVC | index);
            memory.write32(function.stub + 4, RETURN);
            for reference in &function.references {
                patch(reference, function.stub);
            }
            if hle::find(import.nid, function.nid).is_some() {
                done += 1;
            } else {
                missing += 1;
            }
        }
        for variable in &import.variables {
            // a page of the kernel's for them, 0x40 bytes each, zeroed
            let page = *variables.get_or_insert_with(|| {
                let page = shared.kernel.blocks.lock().unwrap().take(MAIN, 0x1000, 0x1000).unwrap_or(0);
                memory.commit(page, 0x1000);
                (page, 0)
            });
            let address = page.0 + page.1 * 0x40;
            variables = Some((page.0, page.1 + 1));
            if (import.nid, variable.nid) == STACK_CHECK_GUARD {
                memory.write32(address, 0x6B69_6D69);
            }
            log::debug!(
                "{} {} at 0x{address:08X}",
                import.name.as_deref().unwrap_or("?"),
                nid::name(import.nid, variable.nid).unwrap_or("?")
            );
            for reference in &variable.references {
                patch(reference, address);
            }
        }
        if !import.tls.is_empty() {
            log::warn!(
                "{} imports thread local variables, which are not linked yet",
                import.name.as_deref().unwrap_or("?")
            );
        }
    }
    log::info!("{done} imported functions done here, {missing} not yet");
}

/// makes the main thread from the process parameters and starts it at
/// module_start, with no arguments, as a plain launch does.
fn start(shared: &Arc<Shared>, module: &Module) -> Result<(), LoadError> {
    let entry = module.start().ok_or_else(|| LoadError::Place(format!("{} has no module_start", module.info.name)))?;
    let param = module.process_param();
    let field = |pick: fn(&vita_module::ProcessParam) -> Option<u32>| param.as_ref().and_then(pick);
    let name =
        param.as_ref().and_then(|param| param.main_thread_name.clone()).unwrap_or_else(|| "main_thread".to_owned());
    let thread = make_thread(
        shared,
        &name,
        entry,
        field(|param| param.main_thread_priority).unwrap_or(DEFAULT_PRIORITY),
        field(|param| param.main_thread_stack_size).unwrap_or(DEFAULT_STACK_SIZE),
        field(|param| param.main_thread_affinity).unwrap_or(0),
        field(|param| param.main_thread_attributes).unwrap_or(0),
        true,
    )
    .map_err(|error| LoadError::Place(format!("no room for the main thread, 0x{error:08X}")))?;
    thread::start(shared, &thread, &[])
        .map_err(|error| LoadError::Place(format!("the main thread did not start, 0x{error:08X}")))
}
