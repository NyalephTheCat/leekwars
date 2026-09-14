//! The adapter's one lock-poisoning policy (#176).
//!
//! Every lock leek-dap owns guards plain session state — the shadow call
//! stack, the breakpoint table, the hit counts, a test's output buffer — and
//! a panic while one is held says nothing about whether the *next* access is
//! safe to make. Panicking a second time on `expect("… lock poisoned")` turns
//! one lost handler into a dead debug adapter, and a dead adapter takes the
//! editor's whole debug session with it, so every site recovers the guard
//! with [`PoisonError::into_inner`] instead — the policy `leek-prelude` and
//! `leek-resolver` already settled on.
//!
//! Recovering is only half of the answer. What a panicking thread left under
//! the lock may be half-written, so the debug controller *records* that it
//! took a poisoned lock ([`unpoisoned_seen`]) and the request loop ends the
//! session with an `output`/`terminated` pair instead of answering
//! `stackTrace` out of a half-built stack — see
//! [`crate::debug::NativeDebugSession::poisoned`] and
//! [`crate::handlers::dispatch`].
//!
//! The transport's output mutex is deliberately not covered: it belongs to
//! the `dap` crate, whose `Server::send` maps a poisoned one to
//! `ServerError::OutputLockError` before this crate ever sees it.
//!
//! [`PoisonError::into_inner`]: std::sync::PoisonError::into_inner

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{LockResult, Mutex, MutexGuard, PoisonError};

/// Take `mutex`, recovering the guard when a panic has poisoned it.
pub(crate) fn lock_unpoisoned<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    unpoisoned(mutex.lock())
}

/// [`lock_unpoisoned`] for any lock result: an `RwLock`'s two guards and the
/// guard a `Condvar::wait` hands back carry poisoning the same way a
/// `Mutex`'s does.
fn unpoisoned<G>(result: LockResult<G>) -> G {
    result.unwrap_or_else(PoisonError::into_inner)
}

/// [`unpoisoned`], recording in `seen` that the lock *was* poisoned.
///
/// The flag is what tells the adapter to end the session: whoever reads it
/// knows the state behind that lock is whatever a panicking thread left
/// there, however cleanly the guard itself came back.
pub(crate) fn unpoisoned_seen<G>(result: LockResult<G>, seen: &AtomicBool) -> G {
    if result.is_err() {
        seen.store(true, Ordering::SeqCst);
    }
    unpoisoned(result)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::{AtomicBool, Mutex, Ordering, lock_unpoisoned, unpoisoned_seen};

    /// Poison `mutex` the way a panicking handler does: write under it, then
    /// unwind out while still holding the guard.
    fn poison(mutex: &Arc<Mutex<Vec<u32>>>) {
        let held = Arc::clone(mutex);
        let outcome = std::thread::spawn(move || {
            // The guard has to be alive when the unwind starts — that is
            // what poisons the lock.
            let mut guard = held.lock().expect("a fresh mutex is not poisoned");
            guard.push(1);
            panic!("deliberate panic: poisoning the lock under test");
        })
        .join();
        assert!(outcome.is_err(), "the thread was supposed to panic");
        assert!(mutex.is_poisoned(), "the panic did not poison the lock");
    }

    #[test]
    fn a_poisoned_lock_is_recovered_rather_than_panicking() {
        let mutex = Arc::new(Mutex::new(Vec::new()));
        poison(&mutex);

        // The whole point: this call is the one that used to `expect`.
        let mut guard = lock_unpoisoned(&mutex);
        assert_eq!(*guard, vec![1], "the write made before the panic was lost");
        guard.push(2);
        drop(guard);

        assert_eq!(
            *lock_unpoisoned(&mutex),
            vec![1, 2],
            "a recovered lock did not keep working"
        );
    }

    #[test]
    fn only_a_poisoned_lock_is_recorded() {
        let mutex = Arc::new(Mutex::new(Vec::new()));
        let seen = AtomicBool::new(false);

        drop(unpoisoned_seen(mutex.lock(), &seen));
        assert!(
            !seen.load(Ordering::SeqCst),
            "a healthy lock was reported poisoned"
        );

        poison(&mutex);
        drop(unpoisoned_seen(mutex.lock(), &seen));
        assert!(
            seen.load(Ordering::SeqCst),
            "taking a poisoned lock went unrecorded"
        );
    }
}
