//! Pipeline step adapters (combinators).
//!
//! These compose over existing [`Step`](crate::Step) implementations without
//! requiring changes in pass crates.

use leek_diagnostics::Severity;

use crate::{Artifact, Context, Step, StepError};

/// Require that artifact `A` exists before running `inner`.
///
/// This is useful for turning "silent no-op" into a clear pipeline
/// construction error when a prerequisite step is missing.
pub struct RequireArtifact<A: Artifact, S: Step> {
    inner: S,
    _marker: std::marker::PhantomData<fn() -> A>,
}

impl<A: Artifact, S: Step> RequireArtifact<A, S> {
    pub fn new(inner: S) -> Self {
        Self {
            inner,
            _marker: std::marker::PhantomData,
        }
    }
}

impl<A: Artifact, S: Step> Step for RequireArtifact<A, S> {
    fn name(&self) -> &'static str {
        self.inner.name()
    }

    fn run(&self, cx: &mut Context<'_>) -> Result<(), StepError> {
        let _ = cx.require::<A>(self.name())?;
        self.inner.run(cx)
    }
}

/// Run `inner` only if artifact `A` is present.
pub struct IfPresent<A: Artifact, S: Step> {
    inner: S,
    _marker: std::marker::PhantomData<fn() -> A>,
}

impl<A: Artifact, S: Step> IfPresent<A, S> {
    pub fn new(inner: S) -> Self {
        Self {
            inner,
            _marker: std::marker::PhantomData,
        }
    }
}

impl<A: Artifact, S: Step> Step for IfPresent<A, S> {
    fn name(&self) -> &'static str {
        self.inner.name()
    }

    fn run(&self, cx: &mut Context<'_>) -> Result<(), StepError> {
        if cx.get::<A>().is_none() {
            return Ok(());
        }
        self.inner.run(cx)
    }
}

/// Execute a hook function. Useful for debugging/profiling without defining a pass crate.
pub struct Tap<F> {
    name: &'static str,
    f: F,
}

impl<F> Tap<F> {
    pub fn new(name: &'static str, f: F) -> Self {
        Self { name, f }
    }
}

impl<F> Step for Tap<F>
where
    F: Fn(&mut Context<'_>) + 'static,
{
    fn name(&self) -> &'static str {
        self.name
    }

    fn run(&self, cx: &mut Context<'_>) -> Result<(), StepError> {
        (self.f)(cx);
        Ok(())
    }
}

/// After running `inner`, abort the pipeline (or hard-fail) if any *new*
/// diagnostics at or above `min` were emitted.
pub struct StopOnDiagnostics<S: Step> {
    inner: S,
    min: Severity,
    /// If true, aborts the pipeline; if false, returns `StepError`.
    abort: bool,
}

impl<S: Step> StopOnDiagnostics<S> {
    pub fn abort(inner: S, min: Severity) -> Self {
        Self {
            inner,
            min,
            abort: true,
        }
    }

    pub fn error(inner: S, min: Severity) -> Self {
        Self {
            inner,
            min,
            abort: false,
        }
    }
}

impl<S: Step> Step for StopOnDiagnostics<S> {
    fn name(&self) -> &'static str {
        self.inner.name()
    }

    fn run(&self, cx: &mut Context<'_>) -> Result<(), StepError> {
        let before = cx.diagnostics().len();
        self.inner.run(cx)?;
        // Note: `Severity` derives `Ord` with Error < Warning < Info < Hint.
        // We consider "at or above min" as `severity <= min`.
        let has_new = cx.diagnostics()[before..]
            .iter()
            .any(|d| d.severity <= self.min);
        if has_new {
            if self.abort {
                cx.abort();
                return Ok(());
            }
            return Err(StepError {
                step: self.name(),
                message: format!("stopped on diagnostic severity >= {}", self.min.as_str()),
            });
        }
        Ok(())
    }
}

/// Repeat `inner` until the fingerprint of artifact `A` stops changing, or `max_iters` is hit.
///
/// This is a fixpoint runner for iterative transforms.
pub struct RepeatUntilStable<A: Artifact, S: Step, FP> {
    inner: S,
    max_iters: usize,
    fingerprint: FP,
    _marker: std::marker::PhantomData<fn() -> A>,
}

impl<A: Artifact, S: Step, FP> RepeatUntilStable<A, S, FP>
where
    FP: Fn(&A) -> u64 + 'static,
{
    pub fn new(inner: S, max_iters: usize, fingerprint: FP) -> Self {
        Self {
            inner,
            max_iters: max_iters.max(1),
            fingerprint,
            _marker: std::marker::PhantomData,
        }
    }
}

impl<A: Artifact, S: Step, FP> Step for RepeatUntilStable<A, S, FP>
where
    FP: Fn(&A) -> u64 + 'static,
{
    fn name(&self) -> &'static str {
        self.inner.name()
    }

    fn run(&self, cx: &mut Context<'_>) -> Result<(), StepError> {
        let mut prev: Option<u64> = None;
        for _ in 0..self.max_iters {
            self.inner.run(cx)?;
            let art = cx.require::<A>(self.name())?;
            let fp = (self.fingerprint)(art);
            if prev == Some(fp) {
                return Ok(());
            }
            prev = Some(fp);
        }
        Ok(())
    }
}
