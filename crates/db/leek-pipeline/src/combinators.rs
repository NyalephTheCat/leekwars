//! Combinatorial artifact lists and step wrappers for recipe planning.

use std::any::TypeId;
use std::marker::PhantomData;

use crate::Step;
use crate::recipe::{
    ArtifactList, RecipeArtifact, RecipeError, RecipeParams, RecipePlan, RecipeStep,
};

/// Concatenate two artifact requirement lists (left then right).
pub struct Chain<L: ArtifactList, R: ArtifactList> {
    _marker: PhantomData<(L, R)>,
}

impl<L: ArtifactList, R: ArtifactList> ArtifactList for Chain<L, R> {
    fn expand(plan: &mut RecipePlan, params: &RecipeParams) -> Result<(), RecipeError> {
        L::expand(plan, params)?;
        R::expand(plan, params)
    }

    fn type_ids(out: &mut Vec<TypeId>) {
        L::type_ids(out);
        R::type_ids(out);
    }
}

/// Expand `A` only when [`RecipeParams::want`] returns true for `A`.
pub struct Optional<A: RecipeArtifact> {
    _marker: PhantomData<A>,
}

impl<A: RecipeArtifact> ArtifactList for Optional<A> {
    fn expand(plan: &mut RecipePlan, params: &RecipeParams) -> Result<(), RecipeError> {
        if params.want(TypeId::of::<A>()) {
            plan.need::<A>(params)?;
        }
        Ok(())
    }

    fn type_ids(out: &mut Vec<TypeId>) {
        A::Produces::type_ids(out);
    }
}

/// A producer step wrapped to stop the pipeline on new diagnostics.
pub struct StopOnError<S: Step> {
    inner: S,
    min: leek_diagnostics::Severity,
}

impl<S: Step> StopOnError<S> {
    pub fn wrap(inner: S, min: leek_diagnostics::Severity) -> Self {
        Self { inner, min }
    }
}

impl<S: Step> Step for StopOnError<S> {
    fn name(&self) -> &'static str {
        self.inner.name()
    }

    fn run(&self, cx: &mut crate::Context<'_>) -> Result<(), crate::StepError> {
        let before = cx.diagnostics().len();
        self.inner.run(cx)?;
        if cx.diagnostics()[before..]
            .iter()
            .any(|d| d.severity <= self.min)
        {
            cx.abort();
        }
        Ok(())
    }
}

/// [`RecipeStep`] helper: wrap `build_inner` with [`StopOnError`] when configured.
pub trait RecipeStepStopOnError: Step + Sized + 'static {
    fn build_inner(params: &RecipeParams) -> Self;
}

impl<S: RecipeStepStopOnError> RecipeStep for S {
    fn build(params: &RecipeParams) -> Box<dyn Step> {
        let inner = S::build_inner(params);
        match params.stop_on_diagnostics {
            Some(min) => Box::new(StopOnError::wrap(inner, min)),
            None => Box::new(inner),
        }
    }
}
