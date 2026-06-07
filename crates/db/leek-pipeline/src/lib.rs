//! Compiler pipeline orchestration for Leekscript.
//!
//! This crate intentionally knows **nothing** about specific compiler
//! passes. It provides:
//!
//! - [`Pipeline`] — a fluent builder you compose with `.with(step)`.
//! - [`Step`] — the trait every pass implements (in *its own* crate).
//! - [`Context`] — the typed artifact bag + diagnostic sink threaded
//!   through each step.
//! - [`Artifact`] — marker trait for values stored in the context.
//!
//! Each pass crate ships its own `pipeline` module that defines the
//! step and the artifact it produces. Callers usually use the
//! [`leek_recipes`] crate (or [`RecipeArtifact`] + [`plan_for`]) to
//! assemble pipelines; manual `.with(step)` composition remains
//! supported for custom tooling.
//!
//! ## Result reuse within a run
//!
//! Each step's output is stored in the [`Run`]; later steps in the
//! same pipeline pull cached results from [`Context`] rather than
//! recomputing.
//!
//! ## Optional: cross-run memoization with salsa
//!
//! Enable the `salsa` feature to gain a [`mod@salsa`] module that
//! exposes a [`salsa::Db`] trait, [`salsa::LeekDb`] concrete database,
//! and [`salsa::SourceFile`] input. Pass crates can then expose
//! tracked queries and dispatch their [`Step`] impl to the tracked
//! form when [`Context::salsa`] returns `Some` — this is how the LSP
//! and `miku watch` avoid re-parsing unchanged files. See
//! [`Pipeline::run_memoized`].

mod adapters;
mod combinators;
mod context;
mod macros;
mod pipeline;
mod project;
mod recipe;
mod timed;

#[cfg(feature = "salsa")]
pub mod salsa;

pub use adapters::{IfPresent, RepeatUntilStable, RequireArtifact, StopOnDiagnostics, Tap};
pub use combinators::{Chain, Optional, RecipeStepStopOnError, StopOnError};
pub use context::{Artifact, Context, Input};
// Re-exported so Input-constructing crates can set `flags` without a direct
// `leek-span` dependency.
pub use leek_span::FeatureFlags;
pub use pipeline::{Pipeline, Run, Step, StepError};
pub use project::{
    LoadedProjectFile, Project, ProjectError, ProjectIndex, SourceInput, walk_leek_files,
};
pub use recipe::{
    ArtifactList, RecipeArtifact, RecipeError, RecipeParams, RecipePlan, RecipeStep,
    pipeline_for as pipeline_for_recipe, plan_for,
};
pub use timed::{StepTiming, Timed, TimedBox, TimingSink};
