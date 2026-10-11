//! the PlayStation Vita as a game sees it, run on kimika-cpu: the guest's
//! memory, its modules loaded and linked, its threads each on a host thread
//! of its own, and the system libraries it imports by NID, done here in
//! Rust.
//!
//! a System is a handle the frontend holds while the game runs by itself:
//! it raises the vertical blanks, reads the screen, hands over the input and
//! learns how the game ended.

pub mod display;
pub mod hle;
pub mod kernel;
pub mod loader;
pub mod memory;

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

pub use display::{FrameBuffer, Screen};
pub use hle::ctrl::CtrlState;
pub use loader::{load, LoadError};

use kernel::Kernel;
use memory::Memory;

/// how a run is set up.
#[derive(Debug, Clone)]
pub struct Config {
    /// where ux0: and the saves live, none to keep nothing.
    pub data_dir: Option<PathBuf>,
    /// how many cores guest threads run on at once, three as on the console.
    pub cores: usize,
}

impl Default for Config {
    fn default() -> Self {
        Config { data_dir: None, cores: 3 }
    }
}

/// how a game ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// sceKernelExitProcess, or the main thread returning, with its status.
    Exited(i32),
    /// something the game did that a console would not survive.
    Faulted(String),
}

/// everything every guest thread shares.
pub struct Shared {
    pub config: Config,
    pub memory: Memory,
    pub kernel: Kernel,
    pub display: display::Display,
    pub hle: hle::State,
    /// when the process started, which its clock counts from.
    pub started: Instant,
    outcome: Mutex<Option<Outcome>>,
    stopping: AtomicBool,
}

impl Shared {
    pub fn new(config: Config) -> Shared {
        Shared {
            kernel: Kernel::new(config.cores),
            config,
            memory: Memory::new(),
            display: display::Display::default(),
            hle: hle::State::default(),
            started: Instant::now(),
            outcome: Mutex::new(None),
            stopping: AtomicBool::new(false),
        }
    }

    /// whether every thread should stop where it is.
    #[inline(always)]
    pub fn is_stopping(&self) -> bool {
        self.stopping.load(Ordering::Relaxed)
    }

    /// ends the process with an outcome, the first one given counting.
    pub fn finish(&self, outcome: Outcome) {
        let mut slot = self.outcome.lock().unwrap();
        if slot.is_none() && !self.is_stopping() {
            match &outcome {
                Outcome::Exited(status) => log::info!("the game exited with {status}"),
                Outcome::Faulted(reason) => log::error!("the game faulted: {reason}"),
            }
            *slot = Some(outcome);
        }
        drop(slot);
        self.halt();
    }

    /// stops every thread where it is, the game's choice or not.
    fn halt(&self) {
        self.stopping.store(true, Ordering::Relaxed);
        // threads waiting in the kernel wake to see it
        self.kernel.wake_everyone();
        self.display.wake_everyone();
    }
}

/// a running game.
pub struct System {
    shared: Arc<Shared>,
}

impl System {
    pub(crate) fn new(shared: Arc<Shared>) -> System {
        System { shared }
    }

    /// what every thread shares, for tests and tools.
    pub fn shared(&self) -> &Arc<Shared> {
        &self.shared
    }

    /// a vertical blank: what sceDisplaySetFrameBuf asked for at the next
    /// frame shows, and threads waiting for it go on. the frontend calls it
    /// about 60 times a second. the count so far.
    pub fn vblank(&self) -> u64 {
        self.shared.display.vblank()
    }

    /// what the screen shows, read from guest memory now, none while it is
    /// off.
    pub fn screen(&self) -> Option<Screen> {
        self.shared.display.screen(&self.shared.memory)
    }

    pub fn set_input(&self, state: CtrlState) {
        *self.shared.hle.ctrl.lock().unwrap() = state;
    }

    /// how the game ended, none while it runs.
    pub fn outcome(&self) -> Option<Outcome> {
        self.shared.outcome.lock().unwrap().clone()
    }

    /// stops every guest thread and waits for them to let go, the outcome
    /// staying none if the game had not ended by itself.
    pub fn stop(&self) {
        self.shared.halt();
        self.shared.kernel.join_all();
    }

    /// the system functions the game called that are not done here, by
    /// library and NID, with how many times each.
    pub fn unimplemented(&self) -> Vec<(u32, u32, u64)> {
        self.shared.hle.unimplemented()
    }

    /// the pages of memory the game touched that were not there.
    pub fn faults(&self) -> Vec<(u32, u64)> {
        self.shared.memory.fault_summary()
    }

    /// a line for the window's title: threads and vertical blanks.
    pub fn status_line(&self) -> String {
        format!("{} threads, {} vblanks", self.shared.kernel.thread_count(), self.shared.display.vblanks())
    }
}

impl Drop for System {
    fn drop(&mut self) {
        self.stop();
    }
}
