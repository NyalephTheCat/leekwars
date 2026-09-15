//! Artifact-driven recipe planning using associated types.
//!
//! Each artifact declares:
//! - the step that produces it (`Producer`)
//! - the artifacts it requires (`Requires`)
//! - the artifacts that producer step yields (`Produces`) (often `(Self,)`, but
//!   some steps yield multiple artifacts).
//!
//! The planner climbs `Requires` recursively, then emits `Producer`.

use std::any::TypeId;
use std::collections::HashSet;

use leek_diagnostics::Severity;

use crate::{Artifact, Pipeline, Step, TimedBox, TimingSink};

/// How aggressively backend-agnostic optimization passes rewrite the IR.
///
/// Optimization is opt-in per recipe because some consumers need the IR to
/// mirror the source 1:1 — notably the Java backend's *exact* mode, which
/// reproduces the upstream reference compiler's emission shape, and analysis
/// passes (lint, complexity) that report on the code as written. Codegen
/// recipes (`miku run`, `miku build --clean`, native) request [`OptLevel::O1`]
/// to shrink the program's static op budget.
/// `Hash` (and `salsa::Update`) so it can key a tracked query:
/// `leek_db::lower_program` is memoized per optimization level, which is
/// what lets an `O1` caller read an optimized tree out of the cache
/// instead of cloning the `O0` one and optimizing it on every run.
#[cfg_attr(feature = "salsa", derive(salsa::Update))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum OptLevel {
    /// No optimization. The IR mirrors the source structure.
    #[default]
    O0,
    /// Backend-agnostic constant folding / dead-code elimination.
    O1,
}

impl OptLevel {
    /// Whether optimization passes should run at this level.
    #[must_use]
    pub fn optimizes(self) -> bool {
        matches!(self, OptLevel::O1)
    }
}

/// Which opt-in lint groups the lint step should run, on top of the
/// always-on defaults. Populated from CLI flags (`--pedantic`,
/// `--nursery`) and the project manifest's `[lints]` table.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct LintGroups {
    /// Strictness lints — verbose-but-fine code worth tightening.
    pub pedantic: bool,
    /// Teaching lints — point newcomers at the idiomatic construct.
    pub nursery: bool,
}

#[derive(Clone)]
pub struct RecipeParams {
    /// If set, producer steps that opt into [`crate::combinators::RecipeStepStopOnError`]
    /// stop the pipeline when new diagnostics at or above this severity are emitted.
    pub stop_on_diagnostics: Option<Severity>,
    /// How aggressively to optimize the IR. Defaults to [`OptLevel::O0`] so
    /// analysis/diagnostic recipes see the code as written; codegen recipes
    /// raise it via [`RecipeParams::with_opt`].
    pub opt: OptLevel,
    /// Opt-in lint groups for recipes that include the lint step.
    /// Defaults to "none" — only the always-on groups run.
    pub lints: LintGroups,
    /// Per-artifact inclusion gate used by [`crate::combinators::Optional`].
    /// Defaults to "include everything".
    want: Option<std::sync::Arc<dyn Fn(TypeId) -> bool + Send + Sync>>,
}

impl Default for RecipeParams {
    fn default() -> Self {
        Self {
            stop_on_diagnostics: Some(Severity::Error),
            opt: OptLevel::default(),
            lints: LintGroups::default(),
            want: None,
        }
    }
}

impl std::fmt::Debug for RecipeParams {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RecipeParams")
            .field("stop_on_diagnostics", &self.stop_on_diagnostics)
            .field("opt", &self.opt)
            .field("lints", &self.lints)
            .field("want", &self.want.as_ref().map(|_| ".."))
            .finish()
    }
}

impl RecipeParams {
    /// LSP-style defaults: best-effort, include all optional
    /// artifacts. The editor is almost always looking at code that's
    /// mid-edit (a trailing `c.`, an unclosed brace), so a parse error
    /// must NOT stop resolution / type-checking — otherwise hover,
    /// completion, and go-to-def all go dark the moment the buffer
    /// stops parsing cleanly.
    pub fn lsp() -> Self {
        Self::permissive()
    }

    /// No stop-on-error wrapping (e.g. best-effort tooling): every planned
    /// step runs even after an earlier one reported an error.
    pub fn permissive() -> Self {
        Self {
            stop_on_diagnostics: None,
            opt: OptLevel::O0,
            lints: LintGroups::default(),
            want: None,
        }
    }

    /// Request an [`OptLevel`] for this recipe (codegen drivers use
    /// [`OptLevel::O1`]).
    #[must_use]
    pub fn with_opt(mut self, opt: OptLevel) -> Self {
        self.opt = opt;
        self
    }

    /// Enable opt-in lint groups for this recipe's lint step.
    #[must_use]
    pub fn with_lints(mut self, lints: LintGroups) -> Self {
        self.lints = lints;
        self
    }

    /// Restrict which artifact types [`Optional`] combinators will expand.
    pub fn with_want(mut self, want: impl Fn(TypeId) -> bool + Send + Sync + 'static) -> Self {
        self.want = Some(std::sync::Arc::new(want));
        self
    }

    pub fn want(&self, artifact: TypeId) -> bool {
        match &self.want {
            None => true,
            Some(f) => f(artifact),
        }
    }
}

#[derive(Debug, Clone)]
pub struct RecipeError {
    pub message: String,
}

impl std::fmt::Display for RecipeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for RecipeError {}

/// Step types that can be constructed into a boxed [`Step`].
///
/// This is the hook point for "combinatorial" behavior: a step can
/// choose to wrap itself based on `params` (e.g. stop-on-error).
pub trait RecipeStep: Step + 'static {
    fn build(params: &RecipeParams) -> Box<dyn Step>;
}

/// A typelist of artifacts.
pub trait ArtifactList {
    fn expand(plan: &mut RecipePlan, params: &RecipeParams) -> Result<(), RecipeError>;
    fn type_ids(out: &mut Vec<TypeId>);
}

impl ArtifactList for () {
    fn expand(_: &mut RecipePlan, _: &RecipeParams) -> Result<(), RecipeError> {
        Ok(())
    }
    fn type_ids(_: &mut Vec<TypeId>) {}
}

macro_rules! impl_artifact_list_tuple {
    ($($name:ident),+ $(,)?) => {
        impl<$($name: RecipeArtifact),+> ArtifactList for ($($name,)+) {
            fn expand(plan: &mut RecipePlan, params: &RecipeParams) -> Result<(), RecipeError> {
                $(plan.need::<$name>(params)?;)+
                Ok(())
            }
            fn type_ids(out: &mut Vec<TypeId>) {
                $(out.push(TypeId::of::<$name>());)+
            }
        }
    };
}

impl_artifact_list_tuple!(A1);
impl_artifact_list_tuple!(A1, A2);
impl_artifact_list_tuple!(A1, A2, A3);
impl_artifact_list_tuple!(A1, A2, A3, A4);
impl_artifact_list_tuple!(A1, A2, A3, A4, A5);
impl_artifact_list_tuple!(A1, A2, A3, A4, A5, A6);
impl_artifact_list_tuple!(A1, A2, A3, A4, A5, A6, A7);
impl_artifact_list_tuple!(A1, A2, A3, A4, A5, A6, A7, A8);

/// Implement on artifacts that want to be plannable.
pub trait RecipeArtifact: Artifact {
    type Producer: RecipeStep;
    type Requires: ArtifactList;
    type Produces: ArtifactList;
}

pub struct RecipePlan {
    steps: Vec<Box<dyn Step>>,
    produced: HashSet<TypeId>,
    planning: HashSet<TypeId>,
}

impl Default for RecipePlan {
    fn default() -> Self {
        Self::new()
    }
}

// Steps are trait objects, so the derive is unavailable; the step names are
// the part worth seeing in an assertion failure anyway.
impl std::fmt::Debug for RecipePlan {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RecipePlan")
            .field("steps", &self.step_names())
            .field("produced", &self.produced.len())
            .finish_non_exhaustive()
    }
}

impl RecipePlan {
    pub fn new() -> Self {
        Self {
            steps: Vec::new(),
            produced: HashSet::new(),
            planning: HashSet::new(),
        }
    }

    pub fn build(self) -> Pipeline {
        self.build_with(None)
    }

    /// [`build`](Self::build), wrapping each planned step in [`TimedBox`]
    /// when `timing` carries a sink.
    ///
    /// One builder for both shapes on purpose: a timed pipeline is the
    /// untimed one plus a stopwatch, and a second `build` that re-walked the
    /// steps is how the two drift apart.
    pub fn build_with(self, timing: Option<&TimingSink>) -> Pipeline {
        let mut p = Pipeline::new();
        for s in self.steps {
            p = p.with_boxed(match timing {
                Some(sink) => TimedBox::sink(s, sink.clone()),
                None => s,
            });
        }
        p
    }

    pub fn need<A: RecipeArtifact>(&mut self, params: &RecipeParams) -> Result<(), RecipeError> {
        let id = TypeId::of::<A>();
        if self.produced.contains(&id) {
            return Ok(());
        }
        if !self.planning.insert(id) {
            return Err(RecipeError {
                message: format!(
                    "cycle detected while planning recipe at `{}`",
                    std::any::type_name::<A>()
                ),
            });
        }

        let expanded = <A::Requires as ArtifactList>::expand(self, params);
        // Leave `planning` clean on every exit: a failed plan must not
        // poison a later `need` on the same `RecipePlan` with a phantom cycle.
        self.planning.remove(&id);
        expanded?;
        // A prerequisite's producer may list `A` among its `Produces` (the
        // multi-artifact case, e.g. parse yielding both the green tree and
        // the AST). Re-check after expansion, or we plan A's producer twice.
        if self.produced.contains(&id) {
            return Ok(());
        }
        self.push_step(<A::Producer as RecipeStep>::build(params), &[]);

        let mut produced = Vec::new();
        <A::Produces as ArtifactList>::type_ids(&mut produced);
        if produced.is_empty() {
            produced.push(id);
        }
        self.produced.extend(produced);

        Ok(())
    }

    pub fn provide<A: Artifact>(&mut self) {
        self.produced.insert(TypeId::of::<A>());
    }

    /// Append a step and mark the listed artifact types as produced.
    pub fn push_step(&mut self, step: Box<dyn Step>, produces: &[TypeId]) {
        self.steps.push(step);
        self.produced.extend(produces.iter().copied());
    }

    /// The planned steps' names, in execution order. See
    /// [`Pipeline::step_names`] for what the names mean under wrapping.
    #[must_use]
    pub fn step_names(&self) -> Vec<&'static str> {
        self.steps.iter().map(|s| s.name()).collect()
    }

    /// How many steps the plan holds.
    #[must_use]
    pub fn len(&self) -> usize {
        self.steps.len()
    }

    /// Whether the plan is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.steps.is_empty()
    }

    /// Whether some planned step already yields artifact `A` — the
    /// bookkeeping [`need`](Self::need) consults to avoid re-planning.
    #[must_use]
    pub fn has_produced<A: Artifact>(&self) -> bool {
        self.produced.contains(&TypeId::of::<A>())
    }
}

/// Plan (without building) a pipeline that produces artifact `A`.
pub fn plan_for<A: RecipeArtifact>(params: &RecipeParams) -> Result<RecipePlan, RecipeError> {
    let mut plan = RecipePlan::new();
    plan.need::<A>(params)?;
    Ok(plan)
}

/// Build a pipeline that produces artifact `A`.
pub fn pipeline_for<A: RecipeArtifact>(params: &RecipeParams) -> Result<Pipeline, RecipeError> {
    Ok(plan_for::<A>(params)?.build())
}
