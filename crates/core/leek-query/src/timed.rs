//! [`TimingSink`] — where a driver records how long a stage took.
//!
//! A driver that wants timings hands one to whatever computes the stages
//! (`leek_session::DriverConfig::timing`) and reads the entries back
//! afterwards. Nothing here knows what a stage *is*.

use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

/// Recorded duration entry.
///
/// `step` names whatever was measured. It used to be a pipeline step's
/// name; a session records the *stage* it asked the database for
/// (`hir`, `diagnostics`, …), because there are no steps to time once a
/// compilation answers from queries.
///
/// Salsa cannot supply this on its own, which is worth recording since the
/// epic's plan was to make timing "a salsa event hook". It fires
/// `WillExecute` before a query body runs and nothing on completion, so
/// the event stream says *which* queries recomputed and never how long any
/// of them took. That is a genuinely useful signal — `leek_db::testing`
/// is built on it — but it is not this one.
#[derive(Debug, Clone)]
pub struct StepTiming {
    pub step: &'static str,
    pub duration: Duration,
}

impl TimingSink {
    /// Time `f`, record it under `name`, and hand back its value.
    pub fn time<T>(&self, name: &'static str, f: impl FnOnce() -> T) -> T {
        let start = Instant::now();
        let out = f();
        self.push(StepTiming {
            step: name,
            duration: start.elapsed(),
        });
        out
    }
}

/// Collector of stage timings, shareable between the places that record
/// them.
///
/// `Arc<Mutex<_>>` rather than `Rc<RefCell<_>>` so that timing a
/// compilation does not cost its caller `Send`. A poisoned lock is recovered with
/// [`PoisonError::into_inner`]: the entries recorded before the panic are
/// still a valid list of durations, and refusing to hand them back would turn
/// a panic elsewhere into a second one here.
#[derive(Debug, Clone, Default)]
pub struct TimingSink(Arc<Mutex<Vec<StepTiming>>>);

impl TimingSink {
    pub fn new() -> Self {
        Self::default()
    }

    /// Snapshot the recorded timings so far. Returns a fresh `Vec`;
    /// the sink itself keeps accumulating across subsequent runs
    /// until [`TimingSink::clear`] is called.
    pub fn entries(&self) -> Vec<StepTiming> {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// Drop everything recorded so far.
    pub fn clear(&self) {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clear();
    }

    fn push(&self, entry: StepTiming) {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(entry);
    }
}
