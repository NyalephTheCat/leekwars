//! Execution semantics: `Context`, `Pipeline::drive`, and the step adapters.
//!
//! The adapters are the pipeline's control flow — they decide whether a
//! pass runs, whether the run keeps going after diagnostics, and how many
//! times a fixpoint pass repeats. None of that is observable from a
//! backend snapshot, so it is pinned here with recording fake steps.

use std::cell::Cell;
use std::rc::Rc;
use std::sync::Arc;

use leek_diagnostics::{Diagnostic, Severity, codes};
use leek_pipeline::{
    Artifact, Context, FeatureFlags, IfPresent, Input, Pipeline, RepeatUntilStable,
    RequireArtifact, Step, StepError, StopOnDiagnostics, StopOnError, Tap,
};
use leek_span::{SourceId, Span};

fn input() -> Input {
    Input {
        source: SourceId::new(1).unwrap(),
        text: Arc::from(""),
        version_byte: 4,
        strict: false,
        flags: FeatureFlags::none(),
    }
}

fn diag(severity: Severity) -> Diagnostic {
    Diagnostic::new(
        codes::UNEXPECTED_TOKEN,
        severity,
        Span::new(SourceId::new(1).unwrap(), 0, 0),
        "synthetic",
    )
}

/// Marker artifacts. Each step below inserts its own so a test can ask
/// "did this step run?" by looking the artifact up on the finished run.
macro_rules! marker {
    ($($name:ident),+ $(,)?) => {
        $(
            #[derive(Debug, PartialEq, Eq)]
            struct $name(u32);
            impl Artifact for $name {}
        )+
    };
}
marker!(Alpha, Beta, Gamma);

/// A step that emits `diags`, inserts `Alpha`, and counts its own runs.
struct Emitter {
    name: &'static str,
    diags: Vec<Severity>,
    runs: Rc<Cell<u32>>,
}

impl Emitter {
    fn new(name: &'static str, diags: &[Severity]) -> (Self, Rc<Cell<u32>>) {
        let runs = Rc::new(Cell::new(0));
        (
            Self {
                name,
                diags: diags.to_vec(),
                runs: Rc::clone(&runs),
            },
            runs,
        )
    }
}

impl Step for Emitter {
    fn name(&self) -> &'static str {
        self.name
    }
    fn run(&self, cx: &mut Context<'_>) -> Result<(), StepError> {
        self.runs.set(self.runs.get() + 1);
        cx.emit_all(self.diags.iter().copied().map(diag));
        cx.insert(Alpha(self.runs.get()));
        Ok(())
    }
}

/// A step that only records that it ran, by inserting `Beta`.
fn beta_marker() -> impl Step {
    Tap::new("beta", |cx: &mut Context<'_>| cx.insert(Beta(1)))
}

// ---- Context ----

#[test]
fn context_insert_overwrites_and_require_names_the_missing_type() {
    let run = Pipeline::new()
        .with(Tap::new("one", |cx: &mut Context<'_>| cx.insert(Alpha(1))))
        .with(Tap::new("two", |cx: &mut Context<'_>| cx.insert(Alpha(2))))
        .with(Tap::new("three", |cx: &mut Context<'_>| {
            cx.emit_all([diag(Severity::Hint), diag(Severity::Info)]);
        }))
        .run(input());

    assert_eq!(run.get::<Alpha>(), Some(&Alpha(2)));
    assert!(run.get::<Beta>().is_none());
    // `emit_all` preserves order.
    let severities: Vec<Severity> = run.diagnostics().iter().map(|d| d.severity).collect();
    assert_eq!(severities, [Severity::Hint, Severity::Info]);
    assert!(run.errors().is_empty());
    assert_eq!(run.input().version_byte, 4);
}

// ---- RequireArtifact / IfPresent ----

#[test]
fn require_artifact_fails_with_the_missing_type_name() {
    let run = Pipeline::new()
        .with(RequireArtifact::<Gamma, _>::new(beta_marker()))
        .run(input());

    let err = run.errors().first().expect("step error");
    assert_eq!(err.step, "beta");
    assert!(err.message.contains("Gamma"), "{}", err.message);
    // The wrapped step never ran.
    assert!(run.get::<Beta>().is_none());
}

#[test]
fn if_present_is_a_silent_no_op_when_the_artifact_is_absent() {
    let absent = Pipeline::new()
        .with(IfPresent::<Gamma, _>::new(beta_marker()))
        .run(input());
    assert!(absent.errors().is_empty());
    assert!(absent.diagnostics().is_empty());
    assert!(absent.get::<Beta>().is_none());

    let present = Pipeline::new()
        .with(Tap::new("seed", |cx: &mut Context<'_>| cx.insert(Gamma(0))))
        .with(IfPresent::<Gamma, _>::new(beta_marker()))
        .run(input());
    assert_eq!(present.get::<Beta>(), Some(&Beta(1)));
}

// ---- StopOnDiagnostics ----

/// `Severity` is ordered Error < Warning < Info < Hint, and the adapter
/// treats "at or above `min`" as `severity <= min`. Inverting that
/// comparison flips every row of this table.
#[test]
fn stop_on_diagnostics_abort_respects_the_severity_threshold() {
    for (emitted, min, should_stop) in [
        (Severity::Error, Severity::Error, true),
        (Severity::Warning, Severity::Error, false),
        (Severity::Warning, Severity::Warning, true),
        (Severity::Warning, Severity::Info, true),
        (Severity::Warning, Severity::Hint, true),
        (Severity::Hint, Severity::Warning, false),
        (Severity::Hint, Severity::Hint, true),
        (Severity::Info, Severity::Warning, false),
    ] {
        let (emitter, _) = Emitter::new("emit", &[emitted]);
        let run = Pipeline::new()
            .with(StopOnDiagnostics::abort(emitter, min))
            .with(beta_marker())
            .run(input());

        assert!(
            run.errors().is_empty(),
            "abort mode never returns StepError ({emitted:?} vs {min:?})"
        );
        assert_eq!(
            run.get::<Beta>().is_none(),
            should_stop,
            "emitted {emitted:?}, min {min:?}"
        );
    }
}

#[test]
fn stop_on_diagnostics_only_counts_diagnostics_the_wrapped_step_added() {
    let (noisy, _) = Emitter::new("noisy", &[Severity::Error]);
    let (quiet, quiet_runs) = Emitter::new("quiet", &[]);
    let run = Pipeline::new()
        // Not wrapped: seeds an error without stopping anything.
        .with(noisy)
        .with(StopOnDiagnostics::abort(quiet, Severity::Error))
        .with(beta_marker())
        .run(input());

    assert_eq!(quiet_runs.get(), 1);
    assert_eq!(
        run.get::<Beta>(),
        Some(&Beta(1)),
        "pre-existing error stopped the run"
    );
}

#[test]
fn stop_on_diagnostics_error_mode_reports_the_step_and_halts_the_run() {
    let (emitter, _) = Emitter::new("emit", &[Severity::Error]);
    let run = Pipeline::new()
        .with(StopOnDiagnostics::error(emitter, Severity::Error))
        .with(beta_marker())
        .run(input());

    assert_eq!(run.errors().len(), 1);
    let err = &run.errors()[0];
    assert_eq!(err.step, "emit");
    assert!(err.message.contains("error"), "{}", err.message);
    assert!(run.get::<Beta>().is_none(), "later steps must not run");
    // The artifact the failing step did contribute survives.
    assert_eq!(run.get::<Alpha>(), Some(&Alpha(1)));
}

// ---- StopOnError (the recipe-level wrapper) ----

#[test]
fn stop_on_error_aborts_without_reporting_a_step_error() {
    let (emitter, _) = Emitter::new("emit", &[Severity::Error]);
    let run = Pipeline::new()
        .with(StopOnError::wrap(emitter, Severity::Error))
        .with(beta_marker())
        .run(input());

    assert!(run.errors().is_empty(), "abort is not a hard failure");
    assert!(run.get::<Beta>().is_none());
    assert_eq!(run.diagnostics().len(), 1);
}

#[test]
fn stop_on_error_ignores_errors_raised_before_the_wrapped_step() {
    let (noisy, _) = Emitter::new("noisy", &[Severity::Error]);
    let (quiet, _) = Emitter::new("quiet", &[]);
    let run = Pipeline::new()
        .with(noisy)
        .with(StopOnError::wrap(quiet, Severity::Error))
        .with(beta_marker())
        .run(input());

    assert_eq!(run.get::<Beta>(), Some(&Beta(1)));
}

// ---- RepeatUntilStable ----

/// A step whose artifact fingerprint changes for the first `changes`
/// iterations and then holds steady.
struct Settling {
    changes: u32,
    runs: Rc<Cell<u32>>,
}

impl Step for Settling {
    fn name(&self) -> &'static str {
        "settling"
    }
    fn run(&self, cx: &mut Context<'_>) -> Result<(), StepError> {
        let n = self.runs.get() + 1;
        self.runs.set(n);
        cx.insert(Alpha(n.min(self.changes)));
        Ok(())
    }
}

fn settling(changes: u32, max_iters: usize) -> (impl Step, Rc<Cell<u32>>) {
    let runs = Rc::new(Cell::new(0));
    let step = Settling {
        changes,
        runs: Rc::clone(&runs),
    };
    (
        RepeatUntilStable::<Alpha, _, _>::new(step, max_iters, |a: &Alpha| u64::from(a.0)),
        runs,
    )
}

#[test]
fn repeat_until_stable_stops_one_iteration_after_the_fingerprint_settles() {
    // Fingerprints: 1, 2, 3, 3 — the fourth run is what proves stability.
    let (step, runs) = settling(3, 10);
    let run = Pipeline::new().with(step).run(input());
    assert!(run.errors().is_empty());
    assert_eq!(runs.get(), 4);
}

#[test]
fn repeat_until_stable_gives_up_at_max_iters_without_failing() {
    // Never settles within the budget.
    let (step, runs) = settling(u32::MAX, 3);
    let run = Pipeline::new().with(step).run(input());
    assert!(
        run.errors().is_empty(),
        "exhausting the budget is not an error"
    );
    assert_eq!(runs.get(), 3);
}

#[test]
fn repeat_until_stable_clamps_a_zero_iteration_budget_to_one() {
    let (step, runs) = settling(u32::MAX, 0);
    Pipeline::new().with(step).run(input());
    assert_eq!(runs.get(), 1, "max_iters 0 must still run the step once");
}

#[test]
fn repeat_until_stable_fails_when_the_inner_step_never_produces_the_artifact() {
    let step = RepeatUntilStable::<Gamma, _, _>::new(beta_marker(), 4, |g: &Gamma| u64::from(g.0));
    let run = Pipeline::new().with(step).run(input());
    let err = run.errors().first().expect("step error");
    assert!(err.message.contains("Gamma"), "{}", err.message);
}

// ---- Pipeline::drive ----

#[test]
fn drive_stops_at_the_first_step_error_and_records_exactly_one() {
    struct Boom;
    impl Step for Boom {
        fn name(&self) -> &'static str {
            "boom"
        }
        fn run(&self, _cx: &mut Context<'_>) -> Result<(), StepError> {
            Err(StepError {
                step: "boom",
                message: "kaboom".into(),
            })
        }
    }

    let run = Pipeline::new()
        .with(Tap::new("seed", |cx: &mut Context<'_>| cx.insert(Alpha(7))))
        .with(Boom)
        .with(Boom)
        .with(beta_marker())
        .run(input());

    assert_eq!(run.errors().len(), 1);
    assert_eq!(run.errors()[0].step, "boom");
    assert_eq!(
        run.errors()[0].to_string(),
        "step `boom`: kaboom",
        "StepError Display is what the CLIs print"
    );
    assert_eq!(run.get::<Alpha>(), Some(&Alpha(7)), "earlier work survives");
    assert!(run.get::<Beta>().is_none());
}

#[test]
fn an_empty_pipeline_runs_cleanly() {
    let pipeline = Pipeline::new();
    assert!(pipeline.is_empty());
    assert_eq!(pipeline.len(), 0);
    let run = pipeline.run(input());
    assert!(run.errors().is_empty());
    assert!(run.diagnostics().is_empty());
}
