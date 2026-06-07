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

#[derive(Clone)]
pub struct RecipeParams {
    /// If set, producer steps that opt into [`crate::combinators::RecipeStepStopOnError`]
    /// stop the pipeline when new diagnostics at or above this severity are emitted.
    pub stop_on_diagnostics: Option<Severity>,
    /// Per-artifact inclusion gate used by [`crate::combinators::Optional`].
    /// Defaults to "include everything".
    want: Option<std::sync::Arc<dyn Fn(TypeId) -> bool + Send + Sync>>,
}

impl Default for RecipeParams {
    fn default() -> Self {
        Self {
            stop_on_diagnostics: Some(Severity::Error),
            want: None,
        }
    }
}

impl std::fmt::Debug for RecipeParams {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RecipeParams")
            .field("stop_on_diagnostics", &self.stop_on_diagnostics)
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
        Self {
            stop_on_diagnostics: None,
            want: None,
        }
    }

    /// No stop-on-error wrapping (e.g. best-effort tooling).
    pub fn permissive() -> Self {
        Self {
            stop_on_diagnostics: None,
            want: None,
        }
    }

    pub fn without_stop_on_error(mut self) -> Self {
        self.stop_on_diagnostics = None;
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

impl RecipePlan {
    pub fn new() -> Self {
        Self {
            steps: Vec::new(),
            produced: HashSet::new(),
            planning: HashSet::new(),
        }
    }

    pub fn build(self) -> Pipeline {
        let mut p = Pipeline::new();
        for s in self.steps {
            p = p.with_boxed(s);
        }
        p
    }

    /// Like [`build`](Self::build), but wraps each planned step in [`TimedBox`].
    pub fn build_timed(self, sink: &TimingSink) -> Pipeline {
        let mut p = Pipeline::new();
        for s in self.steps {
            p = p.with_boxed(TimedBox::sink(s, sink.clone()));
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
                message: "cycle detected while planning recipe".into(),
            });
        }

        <A::Requires as ArtifactList>::expand(self, params)?;
        self.push_step(<A::Producer as RecipeStep>::build(params), &[]);

        let mut produced = Vec::new();
        <A::Produces as ArtifactList>::type_ids(&mut produced);
        if produced.is_empty() {
            produced.push(id);
        }
        self.produced.extend(produced);

        self.planning.remove(&id);
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
