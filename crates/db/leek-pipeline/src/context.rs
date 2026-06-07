//! Pipeline context — the typed artifact bag passed through each step.

use std::any::{Any, TypeId};
use std::collections::HashMap;
use std::marker::PhantomData;
use std::sync::Arc;

use leek_diagnostics::Diagnostic;
use leek_span::{FeatureFlags, SourceId};

use crate::pipeline::StepError;

/// Marker trait for values that may be stored in the pipeline's
/// [`Context`].
///
/// `'static` is the only real bound — the bag stores `Box<dyn Any>`.
/// Implement on any newtype to introduce a new pipeline artifact:
///
/// ```ignore
/// pub struct MyLintFindings(pub Vec<Finding>);
/// impl leek_pipeline::Artifact for MyLintFindings {}
/// ```
pub trait Artifact: 'static {}

/// Configuration for a single pipeline run.
///
/// One source file, one version, one strict-mode setting. Multiple
/// files = multiple pipeline runs (the include graph layer composes
/// those itself).
/// The pipeline stores the language version as a byte (1..=4) to
/// avoid taking a dependency on `leek-syntax`'s `Version` enum (which
/// would introduce a cycle, since `leek-syntax` ships its own pipeline
/// step). Pass crates convert to their preferred enum form.
#[derive(Debug, Clone)]
pub struct Input {
    pub source: SourceId,
    pub text: Arc<str>,
    pub version_byte: u8,
    pub strict: bool,
    /// Opt-in experimental language features for this run. Threaded through the
    /// pipeline (and the salsa input) instead of read from process-global env
    /// vars deep inside the passes. Construct with [`FeatureFlags::from_env`] at
    /// the entry boundary (or `FeatureFlags::none()` / explicit flags in tests).
    pub flags: FeatureFlags,
}

/// Mutable state threaded through every step.
///
/// `'db` is the lifetime of an optional borrowed salsa database. For
/// non-salsa users it's effectively `'static` and the field stays
/// `None`; for memoized runs it's the lifetime of the `&dyn Db` the
/// caller passed in.
pub struct Context<'db> {
    input: Input,
    diagnostics: Vec<Diagnostic>,
    artifacts: HashMap<TypeId, Box<dyn Any>>,
    aborted: bool,
    #[cfg(feature = "salsa")]
    salsa: Option<SalsaHandle<'db>>,
    _marker: PhantomData<&'db ()>,
}

#[cfg(feature = "salsa")]
struct SalsaHandle<'db> {
    db: &'db dyn crate::salsa::Db,
    file: crate::salsa::SourceFile,
}

// `'db` is only used by the `#[cfg(feature = "salsa")]` `with_salsa` method below,
// so default-feature clippy thinks it's elidable — but eliding breaks the salsa build.
#[allow(clippy::elidable_lifetime_names)]
impl<'db> Context<'db> {
    pub(crate) fn new(input: Input) -> Self {
        Self {
            input,
            diagnostics: Vec::new(),
            artifacts: HashMap::new(),
            aborted: false,
            #[cfg(feature = "salsa")]
            salsa: None,
            _marker: PhantomData,
        }
    }

    #[cfg(feature = "salsa")]
    pub(crate) fn with_salsa(
        input: Input,
        db: &'db dyn crate::salsa::Db,
        file: crate::salsa::SourceFile,
    ) -> Self {
        Self {
            input,
            diagnostics: Vec::new(),
            artifacts: HashMap::new(),
            aborted: false,
            salsa: Some(SalsaHandle { db, file }),
            _marker: PhantomData,
        }
    }

    pub fn input(&self) -> &Input {
        &self.input
    }

    pub fn source(&self) -> SourceId {
        self.input.source
    }

    pub fn text(&self) -> &str {
        &self.input.text
    }

    pub fn version_byte(&self) -> u8 {
        self.input.version_byte
    }

    pub fn strict(&self) -> bool {
        self.input.strict
    }

    /// The experimental feature flags for this run.
    pub fn flags(&self) -> FeatureFlags {
        self.input.flags
    }

    /// Read an artifact contributed by an earlier step. Returns
    /// `None` if no step has produced one yet.
    pub fn get<A: Artifact>(&self) -> Option<&A> {
        self.artifacts
            .get(&TypeId::of::<A>())
            .and_then(|b| b.downcast_ref::<A>())
    }

    /// Read an artifact that must exist for the current step to proceed.
    ///
    /// Prefer this over `get()` when absence is a pipeline construction
    /// error (missing prerequisite step) rather than a normal condition.
    pub fn require<A: Artifact>(&self, step: &'static str) -> Result<&A, StepError> {
        self.get::<A>().ok_or_else(|| StepError {
            step,
            message: format!("missing required artifact `{}`", std::any::type_name::<A>()),
        })
    }

    /// Insert an artifact. Overwrites any prior value for the same
    /// type. The [`Step`](crate::Step) trait is the canonical caller;
    /// user code can use this for custom artifacts.
    pub fn insert<A: Artifact>(&mut self, value: A) {
        self.artifacts.insert(TypeId::of::<A>(), Box::new(value));
    }

    /// Append a diagnostic. The pipeline does not categorize these
    /// per stage; rendering / severity-config / dedup happens in the
    /// caller (matching today's `leekc` shape).
    pub fn emit(&mut self, diag: Diagnostic) {
        self.diagnostics.push(diag);
    }

    /// Append many diagnostics.
    pub fn emit_all<I: IntoIterator<Item = Diagnostic>>(&mut self, diags: I) {
        self.diagnostics.extend(diags);
    }

    pub fn diagnostics(&self) -> &[Diagnostic] {
        &self.diagnostics
    }

    /// Halt the pipeline after the current step. Remaining steps are
    /// skipped, but already-computed artifacts and diagnostics are
    /// preserved. Use when a step determines downstream work is
    /// impossible (e.g. parse failed catastrophically).
    pub fn abort(&mut self) {
        self.aborted = true;
    }

    pub(crate) fn is_aborted(&self) -> bool {
        self.aborted
    }

    /// If the pipeline run was kicked off through
    /// [`Pipeline::run_memoized`](crate::Pipeline::run_memoized),
    /// returns the underlying salsa database and input handle. Steps
    /// use this to dispatch into tracked queries when memoization is
    /// available; otherwise they fall back to direct computation.
    ///
    /// Only present when the `salsa` feature is enabled.
    #[cfg(feature = "salsa")]
    pub fn salsa(&self) -> Option<(&dyn crate::salsa::Db, crate::salsa::SourceFile)> {
        self.salsa.as_ref().map(|h| (h.db, h.file))
    }
}
