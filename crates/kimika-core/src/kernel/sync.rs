//! what threads wait on: mutexes, semaphores, event flags, condition
//! variables and message pipes, by UID, and the lightweight mutexes and
//! condition variables that live in a work area of the game's memory, by
//! its address; and the callbacks threads run when told to.
//!
//! all of them sit behind the kernel's one lock, and a thread that has to
//! wait gives up its core and sleeps on the kernel's condition variable,
//! which every change to any of them wakes; the waiter then looks again.

use std::collections::{HashMap, HashSet, VecDeque};
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
    pub semaphores: HashMap<Uid, Semaphore>,
    pub event_flags: HashMap<Uid, EventFlag>,
    pub conds: HashMap<Uid, Cond>,
    /// the lightweight ones, by the address of their work area.
    pub lw_conds: HashMap<u32, Cond>,
    pub pipes: HashMap<Uid, Pipe>,
    pub callbacks: HashMap<Uid, Callback>,
    /// SceGxm's display queue, which waits here like any other object.
    pub display_queue: crate::hle::gxm::DisplayQueue,
}

pub struct Semaphore {
    pub name: String,
    pub attributes: u32,
    pub initial: i32,
    pub count: i32,
    pub max: i32,
    pub waiting: u32,
}

pub struct EventFlag {
    pub name: String,
    pub attributes: u32,
    pub initial: u32,
    pub pattern: u32,
    pub waiting: u32,
}

/// SCE_EVENT_WAITMULTIPLE, more than one thread may wait on an event flag.
pub const WAIT_MULTIPLE: u32 = 0x1000;
/// how a wait on an event flag matches: SCE_EVENT_WAITOR, any of the bits
/// rather than all of them, and SCE_EVENT_WAITCLEAR and _WAITCLEAR_PAT,
/// clearing the bits waited for or every bit once it does.
pub const WAIT_OR: u32 = 1;
pub const WAIT_CLEAR: u32 = 2;
pub const WAIT_CLEAR_PATTERN: u32 = 4;

impl EventFlag {
    /// the pattern if a wait for bits in mode is over now, cleared as the
    /// mode says.
    pub fn take(&mut self, bits: u32, mode: u32) -> Option<u32> {
        let matched = if mode & WAIT_OR != 0 { self.pattern & bits != 0 } else { self.pattern & bits == bits };
        if !matched {
            return None;
        }
        let pattern = self.pattern;
        if mode & WAIT_CLEAR_PATTERN != 0 {
            self.pattern = 0;
        } else if mode & WAIT_CLEAR != 0 {
            self.pattern &= !bits;
        }
        Some(pattern)
    }
}

/// a condition variable, with the mutex it goes with: a UID, or the work
/// area of a lightweight one.
pub struct Cond {
    pub name: String,
    pub attributes: u32,
    pub mutex: u32,
    /// the threads waiting, by ticket, in the order they came.
    pub waiting: VecDeque<(u64, Uid)>,
    /// the tickets signalled, whose threads may go once they have the
    /// mutex back.
    pub signalled: HashSet<u64>,
    pub next_ticket: u64,
}

impl Cond {
    pub fn new(name: &str, attributes: u32, mutex: u32) -> Cond {
        Cond {
            name: name.to_owned(),
            attributes,
            mutex,
            waiting: VecDeque::new(),
            signalled: HashSet::new(),
            next_ticket: 0,
        }
    }

    pub fn enqueue(&mut self, thread: Uid) -> u64 {
        let ticket = self.next_ticket;
        self.next_ticket += 1;
        self.waiting.push_back((ticket, thread));
        ticket
    }

    /// wakes the first thread waiting, or the one given, or all of them.
    pub fn signal(&mut self, which: Signal) -> bool {
        let woken: Vec<u64> = match which {
            Signal::One => self.waiting.pop_front().map(|(ticket, _)| ticket).into_iter().collect(),
            Signal::All => self.waiting.drain(..).map(|(ticket, _)| ticket).collect(),
            Signal::To(thread) => match self.waiting.iter().position(|&(_, waiting)| waiting == thread) {
                Some(index) => self.waiting.remove(index).map(|(ticket, _)| ticket).into_iter().collect(),
                None => Vec::new(),
            },
        };
        let any = !woken.is_empty();
        self.signalled.extend(woken);
        any
    }

    /// forgets a ticket whose wait ran out.
    pub fn forget(&mut self, ticket: u64) {
        self.waiting.retain(|&(waiting, _)| waiting != ticket);
        self.signalled.remove(&ticket);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Signal {
    One,
    All,
    To(Uid),
}

/// a message pipe: a buffer of bytes threads send into and receive from.
pub struct Pipe {
    pub name: String,
    pub attributes: u32,
    pub capacity: usize,
    pub buffer: VecDeque<u8>,
}

/// a function a thread runs when another tells it to and it checks.
pub struct Callback {
    pub name: String,
    pub thread: Uid,
    pub function: u32,
    pub argument: u32,
    /// how many times it was told since it last ran, and the last thing it
    /// was told.
    pub notified: u32,
    pub notify_argument: u32,
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
    fn an_event_flag_matches_all_or_any_bits_and_clears_as_asked() {
        let mut flag = EventFlag { name: String::new(), attributes: 0, initial: 0, pattern: 0b0110, waiting: 0 };
        assert_eq!(flag.take(0b0011, 0), None);
        assert_eq!(flag.take(0b0011, WAIT_OR), Some(0b0110));
        assert_eq!(flag.take(0b0110, WAIT_CLEAR), Some(0b0110));
        assert_eq!(flag.pattern, 0);
        flag.pattern = 0b1001;
        assert_eq!(flag.take(0b1000, WAIT_OR | WAIT_CLEAR_PATTERN), Some(0b1001));
        assert_eq!(flag.pattern, 0);
    }

    #[test]
    fn a_cond_wakes_its_waiters_in_order_one_by_one_or_together() {
        let mut cond = Cond::new("c", 0, 1);
        let (first, second, third) = (cond.enqueue(10), cond.enqueue(11), cond.enqueue(12));
        assert!(cond.signal(Signal::One));
        assert!(cond.signalled.contains(&first) && !cond.signalled.contains(&second));
        assert!(cond.signal(Signal::To(12)));
        assert!(cond.signalled.contains(&third));
        assert!(!cond.signal(Signal::To(99)));
        assert!(cond.signal(Signal::All));
        assert!(cond.signalled.contains(&second) && cond.waiting.is_empty());
        assert!(!cond.signal(Signal::All));
    }

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
