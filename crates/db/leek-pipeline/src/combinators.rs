//! Combinatorial artifact lists and step wrappers for recipe planning.

use std::any::TypeId;
use std::marker::PhantomData;

use crate::Step;
use crate::adapters::StopOnDiagnostics;
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

/// The recipe-level spelling of [`StopOnDiagnostics::abort`]: a producer
/// step wrapped to abort the run when it emits a new diagnostic at or above
/// `min`.
///
/// A constructor rather than a wrapper type of its own — the two had
/// identical `run` bodies, and one of them was going to grow a fix the other
/// missed.
pub struct StopOnError;

impl StopOnError {
    /// Wrap `inner` so that a new diagnostic at or above `min` aborts the
    /// pipeline once `inner` has finished.
    pub fn wrap<S: Step>(inner: S, min: leek_diagnostics::Severity) -> StopOnDiagnostics<S> {
        StopOnDiagnostics::abort(inner, min)
    }
}

/// [`RecipeStep`] helper: wrap `build_inner` with [`StopOnError`] when
/// [`RecipeParams::stop_on_diagnostics`] is set.
///
/// Exactly one production step opts in — `leek_parser::pipeline::Parse` — so
/// that a file which does not parse stops the run instead of handing a
/// truncated tree to resolution and type-checking. Every other pass
/// implements [`RecipeStep`] directly and keeps running after diagnostics.
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
