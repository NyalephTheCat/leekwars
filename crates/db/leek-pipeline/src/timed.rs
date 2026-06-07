//! [`Timed`] — wrap any [`Step`] to record or print its run duration.
//!
//! Composes orthogonally over the rest of the pipeline. No changes
//! required in [`Pipeline`], [`Step`], or [`Context`]:
//!
//! ```ignore
//! use leek_pipeline::{Pipeline, Timed};
//!
//! let run = Pipeline::new()
//!     .with(Timed::print(Pragma))
//!     .with(Timed::print(Lex))
//!     .with(Timed::print(Parse))
//!     .run(input);
//! ```
//!
//! For programmatic access, use [`Timed::sink`] with a shared
//! [`TimingSink`] — every wrapped step pushes a `(name, duration)`
//! entry into the sink, and the caller reads the recorded values
//! after the run.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::{Duration, Instant};

use crate::context::Context;
use crate::pipeline::{Step, StepError};

/// Recorded step duration entry.
#[derive(Debug, Clone)]
pub struct StepTiming {
    pub step: &'static str,
    pub duration: Duration,
}

/// Collector of step timings, shareable between many [`Timed`]
/// wrappers. Each `Timed` holding a clone of the sink appends its
/// entry on every run.
#[derive(Debug, Clone, Default)]
pub struct TimingSink(Rc<RefCell<Vec<StepTiming>>>);

impl TimingSink {
    pub fn new() -> Self {
        Self::default()
    }

    /// Snapshot the recorded timings so far. Returns a fresh `Vec`;
    /// the sink itself keeps accumulating across subsequent runs
    /// until [`TimingSink::clear`] is called.
    pub fn entries(&self) -> Vec<StepTiming> {
        self.0.borrow().clone()
    }

    /// Drop everything recorded so far.
    pub fn clear(&self) {
        self.0.borrow_mut().clear();
    }

    fn push(&self, entry: StepTiming) {
        self.0.borrow_mut().push(entry);
    }
}

/// What [`Timed`] does with the recorded duration after each call.
enum Out {
    /// Print a one-line summary to stderr.
    Print,
    /// Append into a [`TimingSink`].
    Sink(TimingSink),
}

/// A [`Step`] adapter that times its inner step.
pub struct Timed<S: Step> {
    inner: S,
    out: Out,
}

impl<S: Step> Timed<S> {
    /// Wrap `inner`, printing a `[step] <name>: <duration>` line on
    /// stderr after every invocation.
    pub fn print(inner: S) -> Self {
        Self {
            inner,
            out: Out::Print,
        }
    }

    /// Wrap `inner`, appending each duration into the supplied
    /// sink. Multiple `Timed::sink` wrappers can share the same
    /// sink to collect a whole pipeline's timings into one place.
    pub fn sink(inner: S, sink: TimingSink) -> Self {
        Self {
            inner,
            out: Out::Sink(sink),
        }
    }
}

impl<S: Step> Step for Timed<S> {
    fn name(&self) -> &'static str {
        self.inner.name()
    }
    fn run(&self, cx: &mut Context<'_>) -> Result<(), StepError> {
        let start = Instant::now();
        let res = self.inner.run(cx);
        let elapsed = start.elapsed();
        let entry = StepTiming {
            step: self.inner.name(),
            duration: elapsed,
        };
        match &self.out {
            Out::Print => eprintln!("[step] {:>14}: {:?}", entry.step, entry.duration),
            Out::Sink(s) => s.push(entry),
        }
        res
    }
}

/// Wrap a boxed step (e.g. from recipe planning) with timing.
pub struct TimedBox {
    inner: Box<dyn Step>,
    sink: TimingSink,
}

impl TimedBox {
    pub fn sink(inner: Box<dyn Step>, sink: TimingSink) -> Box<dyn Step> {
        Box::new(Self { inner, sink })
    }
}

impl Step for TimedBox {
    fn name(&self) -> &'static str {
        self.inner.name()
    }
    fn run(&self, cx: &mut Context<'_>) -> Result<(), StepError> {
        let start = Instant::now();
        let res = self.inner.run(cx);
        let entry = StepTiming {
            step: self.inner.name(),
            duration: start.elapsed(),
        };
        self.sink.push(entry);
        res
    }
}
