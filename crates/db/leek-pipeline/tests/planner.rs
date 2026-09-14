//! Recipe planning over a synthetic artifact graph.
//!
//! The real recipes (`leek-session`) only exercise a handful of graph
//! shapes; these fakes pin the planner's contract directly — ordering,
//! deduplication, multi-artifact producers, `Optional` gating, and cycle
//! reporting — so a planner change that silently drops or duplicates a
//! pass fails here rather than in a backend snapshot.

use std::any::TypeId;

use leek_pipeline::{
    Artifact, Context, Optional, RecipeArtifact, RecipeParams, RecipePlan, RecipeStepStopOnError,
    Step, StepError, plan_for,
};

/// Declare an artifact, its producer step (named `$name`), and the
/// `RecipeArtifact` wiring in one go. `Produces` defaults to `(Self,)`.
macro_rules! node {
    ($art:ident / $step:ident = $name:literal, requires: $req:ty) => {
        node!($art / $step = $name, requires: $req, produces: ($art,));
    };
    ($art:ident / $step:ident = $name:literal, requires: $req:ty, produces: $prod:ty) => {
        struct $art;
        impl Artifact for $art {}

        struct $step;
        impl Step for $step {
            fn name(&self) -> &'static str {
                $name
            }
            fn run(&self, _cx: &mut Context<'_>) -> Result<(), StepError> {
                Ok(())
            }
        }
        impl RecipeStepStopOnError for $step {
            fn build_inner(_: &RecipeParams) -> Self {
                $step
            }
        }

        impl RecipeArtifact for $art {
            type Producer = $step;
            type Requires = $req;
            type Produces = $prod;
        }
    };
}

// ---- linear chain: C <- B <- A ----
node!(ChainA / ChainAStep = "a", requires: ());
node!(ChainB / ChainBStep = "b", requires: (ChainA,));
node!(ChainC / ChainCStep = "c", requires: (ChainB,));

#[test]
fn linear_chain_plans_prerequisites_first() {
    let plan = plan_for::<ChainC>(&RecipeParams::default()).expect("plan");
    assert_eq!(plan.step_names(), ["a", "b", "c"]);
    assert_eq!(plan.len(), 3);
    assert!(!plan.is_empty());
    assert!(plan.has_produced::<ChainA>());
    assert!(plan.has_produced::<ChainC>());
}

#[test]
fn built_pipeline_keeps_the_planned_order() {
    let pipeline = plan_for::<ChainC>(&RecipeParams::default())
        .expect("plan")
        .build();
    assert_eq!(pipeline.step_names(), ["a", "b", "c"]);
}

// ---- diamond: D <- (B, C) <- A ----
node!(DiaA / DiaAStep = "dia-a", requires: ());
node!(DiaB / DiaBStep = "dia-b", requires: (DiaA,));
node!(DiaC / DiaCStep = "dia-c", requires: (DiaA,));
node!(DiaD / DiaDStep = "dia-d", requires: (DiaB, DiaC));

#[test]
fn diamond_plans_the_shared_prerequisite_once() {
    let plan = plan_for::<DiaD>(&RecipeParams::default()).expect("plan");
    let names = plan.step_names();
    assert_eq!(
        names.iter().filter(|n| **n == "dia-a").count(),
        1,
        "{names:?}"
    );
    assert_eq!(names, ["dia-a", "dia-b", "dia-c", "dia-d"]);
}

// ---- one step, two artifacts (the parse step's green-tree + AST shape) ----
node!(PairX / PairStep = "pair", requires: (), produces: (PairX, PairY));
struct PairY;
impl Artifact for PairY {}
impl RecipeArtifact for PairY {
    type Producer = PairStep;
    type Requires = ();
    type Produces = (PairX, PairY);
}

#[test]
fn multi_artifact_producer_is_planned_once_per_plan() {
    let params = RecipeParams::default();
    let mut plan = RecipePlan::new();
    plan.need::<PairX>(&params).expect("need x");
    plan.need::<PairY>(&params).expect("need y");
    assert_eq!(plan.step_names(), ["pair"]);
}

// ---- side product: needing A pulls B, whose step also yields A ----
node!(SideB / SideBStep = "side-b", requires: (), produces: (SideA, SideB));
struct SideA;
impl Artifact for SideA {}
struct SideAStep;
impl Step for SideAStep {
    fn name(&self) -> &'static str {
        "side-a"
    }
    fn run(&self, _cx: &mut Context<'_>) -> Result<(), StepError> {
        Ok(())
    }
}
impl RecipeStepStopOnError for SideAStep {
    fn build_inner(_: &RecipeParams) -> Self {
        SideAStep
    }
}
impl RecipeArtifact for SideA {
    type Producer = SideAStep;
    type Requires = (SideB,);
    type Produces = (SideA,);
}

#[test]
fn a_prerequisite_that_also_yields_the_target_suppresses_its_producer() {
    // Regression: `need` only consulted `produced` *before* expanding the
    // requirement list, so a requirement whose producer lists the target
    // among its `Produces` got the target's own step appended anyway.
    let plan = plan_for::<SideA>(&RecipeParams::default()).expect("plan");
    assert_eq!(plan.step_names(), ["side-b"]);
}

// ---- cycles ----
node!(CycA / CycAStep = "cyc-a", requires: (CycB,));
node!(CycB / CycBStep = "cyc-b", requires: (CycA,));

#[test]
fn cycle_is_reported_with_the_offending_artifact() {
    let err = plan_for::<CycA>(&RecipeParams::default()).expect_err("cycle");
    assert!(err.message.contains("cycle detected"), "{}", err.message);
    assert!(err.message.contains("CycA"), "{}", err.message);
}

// ---- a cycle reachable only when the optional branch is wanted ----
node!(GateBad / GateBadStep = "gate-bad", requires: (GateMid,));
node!(GateMid / GateMidStep = "gate-mid", requires: Optional<GateBad>);

#[test]
fn a_failed_plan_leaves_no_phantom_cycle_behind() {
    // Regression: `need` returned early on the error path without clearing
    // its in-progress marker, so re-planning the same artifact on the same
    // `RecipePlan` (with params that dodge the cycle) reported a cycle that
    // no longer existed.
    let mut plan = RecipePlan::new();
    let greedy = RecipeParams::default();
    plan.need::<GateMid>(&greedy)
        .expect_err("cycle via GateBad");

    let gated = RecipeParams::default().with_want(|id| id != TypeId::of::<GateBad>());
    plan.need::<GateMid>(&gated)
        .expect("optional branch skipped");
    assert_eq!(plan.step_names(), ["gate-mid"]);
}

// ---- Optional gating ----
node!(OptExtra / OptExtraStep = "opt-extra", requires: ());
node!(OptMain / OptMainStep = "opt-main", requires: Optional<OptExtra>);

#[test]
fn optional_requirements_follow_the_want_predicate() {
    let all = plan_for::<OptMain>(&RecipeParams::default()).expect("plan");
    assert_eq!(all.step_names(), ["opt-extra", "opt-main"]);

    let params = RecipeParams::default().with_want(|id| id != TypeId::of::<OptExtra>());
    let gated = plan_for::<OptMain>(&params).expect("plan");
    assert_eq!(gated.step_names(), ["opt-main"]);
    assert!(!gated.has_produced::<OptExtra>());
}

#[test]
fn provide_marks_an_artifact_as_externally_supplied() {
    let params = RecipeParams::default();
    let mut plan = RecipePlan::new();
    plan.provide::<ChainB>();
    plan.need::<ChainC>(&params).expect("plan");
    // `b` (and therefore `a`) came from outside the plan.
    assert_eq!(plan.step_names(), ["c"]);
}
