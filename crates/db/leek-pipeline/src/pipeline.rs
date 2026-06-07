//! Pipeline, Step trait, and Run wrapper.

use leek_diagnostics::Diagnostic;

use crate::context::{Artifact, Context, Input};

/// Error returned by a step. Steps may also report problems as
/// diagnostics via [`Context::emit`] — `StepError` is reserved for
/// hard failures the pipeline cannot continue past at all.
#[derive(Debug, Clone)]
pub struct StepError {
    pub step: &'static str,
    pub message: String,
}

impl std::fmt::Display for StepError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "step `{}`: {}", self.step, self.message)
    }
}

impl std::error::Error for StepError {}

/// A unit of work in the pipeline.
///
/// Steps read prior artifacts from the [`Context`], run their pass,
/// then contribute their output via [`Context::insert`] and
/// diagnostics via [`Context::emit`]. Implementing this trait in a
/// third-party crate is the supported extension point.
pub trait Step {
    fn name(&self) -> &'static str;
    fn run(&self, cx: &mut Context<'_>) -> Result<(), StepError>;
}

/// Compose steps into a recipe.
#[derive(Default)]
pub struct Pipeline {
    steps: Vec<Box<dyn Step>>,
}

impl Pipeline {
    pub fn new() -> Self {
        Self { steps: Vec::new() }
    }

    /// Append a step. Order matters — each step sees the artifacts
    /// contributed by earlier steps and contributes its own.
    pub fn with<S: Step + 'static>(mut self, step: S) -> Self {
        self.steps.push(Box::new(step));
        self
    }

    /// Append a boxed step (useful when the step type is chosen at
    /// runtime, e.g. via a CLI flag).
    pub fn with_boxed(mut self, step: Box<dyn Step>) -> Self {
        self.steps.push(step);
        self
    }

    /// Drive the pipeline over `input` without memoization. Each
    /// call recomputes every step's output from scratch.
    pub fn run(&self, input: Input) -> Run<'static> {
        let mut cx = Context::new(input);
        let errors = self.drive(&mut cx);
        Run {
            context: cx,
            errors,
        }
    }

    /// Drive the pipeline over a salsa-tracked input, allowing steps
    /// that implement tracked queries to short-circuit on identical
    /// re-runs. Only available with the `salsa` feature.
    ///
    /// Steps that don't know about salsa fall back to the same
    /// direct computation as [`run`](Self::run); steps that do
    /// dispatch through `cx.salsa()`.
    #[cfg(feature = "salsa")]
    pub fn run_memoized<'db>(
        &self,
        db: &'db dyn crate::salsa::Db,
        file: crate::salsa::SourceFile,
    ) -> Run<'db> {
        let input = Input {
            source: file.source(db),
            text: file.text(db).as_str().into(),
            version_byte: file.version_byte(db),
            strict: file.strict(db),
            flags: leek_span::FeatureFlags::from_bits(file.flags_bits(db)),
        };
        let mut cx = Context::with_salsa(input, db, file);
        let errors = self.drive(&mut cx);
        Run {
            context: cx,
            errors,
        }
    }

    fn drive(&self, cx: &mut Context<'_>) -> Vec<StepError> {
        let mut errors = Vec::new();
        for step in &self.steps {
            if cx.is_aborted() {
                break;
            }
            if let Err(e) = step.run(cx) {
                errors.push(e);
                break;
            }
        }
        errors
    }
}

/// Output of [`Pipeline::run`] / [`Pipeline::run_memoized`].
pub struct Run<'db> {
    context: Context<'db>,
    errors: Vec<StepError>,
}

impl<'db> Run<'db> {
    pub fn get<A: Artifact>(&self) -> Option<&A> {
        self.context.get::<A>()
    }

    pub fn diagnostics(&self) -> &[Diagnostic] {
        self.context.diagnostics()
    }

    pub fn errors(&self) -> &[StepError] {
        &self.errors
    }

    pub fn input(&self) -> &Input {
        self.context.input()
    }

    /// Drop the run wrapper and return the underlying context.
    pub fn into_context(self) -> Context<'db> {
        self.context
    }
}
