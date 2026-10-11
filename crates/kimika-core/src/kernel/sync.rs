//! what threads wait on: mutexes, by UID, and the lightweight mutexes that
//! live in a work area of the game's memory, by its address.
//!
//! all of them sit behind the kernel's one lock, and a thread that has to
//! wait gives up its core and sleeps on the kernel's condition variable,
//! which every change to any of them wakes; the waiter then looks again.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use super::{errors, Kernel, Uid};
use crate::Shared;

/// SCE_KERNEL_MUTEX_ATTR_RECURSIVE, locked again by its owner it counts up.
pub const RECURSIVE: u32 = 0x02;

#[derive(Default)]
pub struct Objects {
    pub mutexes: HashMap<Uid, Mutex>,
    /// the lightweight ones, by the address of their work area.
    pub lw_mutexes: HashMap<u32, Mutex>,
}

pub struct Mutex {
    pub uid: Uid,
    pub name: String,
    pub attributes: u32,
    /// the thread holding it and how many times, or none.
    pub owner: Option<Uid>,
    pub count: u32,
}

impl Mutex {
    pub fn new(uid: Uid, name: &str, attributes: u32, owner: Uid, count: u32) -> Mutex {
        Mutex { uid, name: name.to_owned(), attributes, owner: (count > 0).then_some(owner), count }
    }

    /// takes it count times for thread, if it can now: Some(Ok) taken,
    /// Some(Err) refused, none when another thread holds it.
    pub fn try_lock(&mut self, thread: Uid, count: u32, recursive_error: u32) -> Option<Result<(), u32>> {
        if count == 0 {
            return Some(Err(errors::ILLEGAL_ADDR));
        }
        match self.owner {
            None => {
                self.owner = Some(thread);
                self.count = count;
                Some(Ok(()))
            }
            Some(owner) if owner == thread => {
                if self.attributes & RECURSIVE == 0 {
                    return Some(Err(recursive_error));
                }
                match self.count.checked_add(count) {
                    Some(total) => {
                        self.count = total;
                        Some(Ok(()))
                    }
                    None => Some(Err(errors::MUTEX_LOCK_OVF)),
                }
            }
            Some(_) => None,
        }
    }

    /// lets go of it count times for thread.
    pub fn unlock(&mut self, thread: Uid, count: u32, not_owned: u32, underflow: u32) -> Result<(), u32> {
        if self.owner != Some(thread) {
            return Err(not_owned);
        }
        if count == 0 || count > self.count {
            return Err(underflow);
        }
        self.count -= count;
        if self.count == 0 {
            self.owner = None;
        }
        Ok(())
    }
}

impl Kernel {
    /// runs test on the objects until it gives an answer, the calling
    /// thread's core given up and the thread asleep in between, every change
    /// to any object waking it to look again. a timeout in microseconds runs
    /// out with WAIT_TIMEOUT, and the end of the process with WAIT_DELETE.
    pub fn wait_for<T>(
        &self,
        shared: &Shared,
        timeout: Option<Duration>,
        mut test: impl FnMut(&mut Objects) -> Option<Result<T, u32>>,
    ) -> Result<T, u32> {
        let mut objects = self.sync.lock().unwrap();
        if let Some(result) = test(&mut objects) {
            return result;
        }
        let deadline = timeout.map(|timeout| Instant::now() + timeout);
        self.cores.release();
        let result = loop {
            objects = match deadline {
                Some(deadline) => {
                    let now = Instant::now();
                    if now >= deadline {
                        break Err(errors::WAIT_TIMEOUT);
                    }
                    self.changed.wait_timeout(objects, deadline - now).unwrap().0
                }
                None => self.changed.wait(objects).unwrap(),
            };
            if shared.is_stopping() {
                break Err(errors::WAIT_DELETE);
            }
            if let Some(result) = test(&mut objects) {
                break result;
            }
        };
        drop(objects);
        self.cores.acquire(shared);
        result
    }

    /// changes the objects and wakes every thread waiting on them.
    pub fn change<T>(&self, change: impl FnOnce(&mut Objects) -> T) -> T {
        let mut objects = self.sync.lock().unwrap();
        let result = change(&mut objects);
        self.changed.notify_all();
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_recursive_mutex_counts_and_a_plain_one_refuses() {
        let mut recursive = Mutex::new(1, "r", RECURSIVE, 7, 0);
        assert_eq!(recursive.try_lock(7, 1, errors::MUTEX_RECURSIVE), Some(Ok(())));
        assert_eq!(recursive.try_lock(7, 2, errors::MUTEX_RECURSIVE), Some(Ok(())));
        assert_eq!(recursive.try_lock(9, 1, errors::MUTEX_RECURSIVE), None);
        assert_eq!(
            recursive.unlock(9, 1, errors::MUTEX_NOT_OWNED, errors::MUTEX_UNLOCK_UDF),
            Err(errors::MUTEX_NOT_OWNED)
        );
        assert_eq!(
            recursive.unlock(7, 4, errors::MUTEX_NOT_OWNED, errors::MUTEX_UNLOCK_UDF),
            Err(errors::MUTEX_UNLOCK_UDF)
        );
        assert_eq!(recursive.unlock(7, 3, errors::MUTEX_NOT_OWNED, errors::MUTEX_UNLOCK_UDF), Ok(()));
        assert_eq!(recursive.owner, None);

        // created already held, the way newlib makes its fd table's
        let mut plain = Mutex::new(2, "p", 0, 7, 1);
        assert_eq!(plain.owner, Some(7));
        assert_eq!(plain.try_lock(7, 1, errors::LW_MUTEX_RECURSIVE), Some(Err(errors::LW_MUTEX_RECURSIVE)));
    }
}
