//! the kernel's side of the system: the UIDs it hands out, the memory
//! blocks, the threads and the cores they take turns on, and the objects
//! they wait on.
//!
//! every guest thread is a host thread, so a call that blocks really
//! blocks, on a condition variable, after giving up its core.

pub mod errors;
pub mod memblock;
pub mod modules;
pub mod sync;
pub mod thread;

use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Condvar, Mutex};

pub use thread::{Cores, Thread};

/// a SceUID, positive; a negative value is an error code.
pub type Uid = u32;

pub struct Kernel {
    next_uid: AtomicU32,
    pub blocks: Mutex<memblock::Blocks>,
    pub threads: Mutex<HashMap<Uid, Arc<Thread>>>,
    /// the host threads guest threads run on, to wait for at the end.
    handles: Mutex<Vec<std::thread::JoinHandle<()>>>,
    pub sync: Mutex<sync::Objects>,
    pub modules: Mutex<modules::Modules>,
    /// what a thread waiting on any of the objects in sync waits on, woken
    /// whenever one of them changes.
    pub changed: Condvar,
    pub cores: Cores,
}

impl Kernel {
    pub fn new(cores: usize) -> Kernel {
        Kernel {
            // UIDs look like the console's, odd and well away from zero, so
            // that a zeroed variable used as one fails
            next_uid: AtomicU32::new(0x4001_0001),
            blocks: Mutex::new(memblock::Blocks::default()),
            threads: Mutex::new(HashMap::new()),
            handles: Mutex::new(Vec::new()),
            sync: Mutex::new(sync::Objects::default()),
            modules: Mutex::new(modules::Modules::default()),
            changed: Condvar::new(),
            cores: Cores::new(cores),
        }
    }

    /// a new UID, never given before.
    pub fn uid(&self) -> Uid {
        self.next_uid.fetch_add(2, Ordering::Relaxed)
    }

    pub fn thread(&self, uid: Uid) -> Option<Arc<Thread>> {
        self.threads.lock().unwrap().get(&uid).cloned()
    }

    pub fn thread_count(&self) -> usize {
        self.threads.lock().unwrap().values().filter(|thread| !thread.is_dead()).count()
    }

    pub(crate) fn keep_handle(&self, handle: std::thread::JoinHandle<()>) {
        self.handles.lock().unwrap().push(handle);
    }

    /// waits for every guest thread's host thread to end.
    pub fn join_all(&self) {
        loop {
            let handles = std::mem::take(&mut *self.handles.lock().unwrap());
            if handles.is_empty() {
                return;
            }
            for handle in handles {
                let _ = handle.join();
            }
        }
    }

    /// wakes every thread waiting in the kernel, to see the process end.
    pub fn wake_everyone(&self) {
        let _guard = self.sync.lock().unwrap();
        self.changed.notify_all();
        for thread in self.threads.lock().unwrap().values() {
            thread.wake();
        }
        self.cores.wake_everyone();
    }
}
