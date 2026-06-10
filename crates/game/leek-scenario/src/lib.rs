//! Fight-scenario format and runners for Leekscript.
//!
//! A [`Scenario`] describes a leek-wars fight — the map, the seed, and the
//! leeks/farmers on each team — in a single serde schema that is authored in
//! TOML (matching `Miku.toml`) but also reads the official generator's JSON
//! scenarios. The schema is **mergable** along three axes, all powered by one
//! overlay engine ([`merge`]):
//!
//! - **profiles** — named `[profiles.<name>]` patches applied with
//!   [`Scenario::apply_profile`];
//! - **inheritance** — a scenario `extends` a base file ([`include`]);
//! - **reusable files** — an entity pulls its settings from a shared `leek`
//!   file and overrides a few fields inline.
//!
//! [`load`] turns a resolved scenario into a runnable [`leek_generator::Fight`]
//! plus its compiled AIs, and [`testing`] drives an AI against many settings
//! (matrix sweep, tournament, randomized point-buy builds) on top of that one
//! single-fight primitive.

pub mod build_gen;
pub mod include;
pub mod load;
pub mod merge;
pub mod schema;
pub mod testing;

pub use load::{
    LoadedFight, World, build_fight, build_fight_with_cache, build_world, compile_ai,
    compile_ai_source,
};
pub use schema::{
    Bracket, EntitySpec, FarmerSpec, MapSpec, RandomSpec, RandomTarget, Scenario, ScenarioPatch,
    StatKind, TeamSpec, TestMode, TestingSpec,
};
pub use testing::{
    CellResult, FightResult, MatrixAxes, Standing, TestReport, TournamentSpec, run_matrix,
    run_random, run_tournament,
};

use std::path::Path;

impl Scenario {
    /// Load a scenario from a `.toml` or `.json` file, resolving `extends`
    /// inheritance and per-entity `leek` file references into one fully
    /// inlined scenario.
    ///
    /// # Errors
    /// Returns an error if the file can't be read/parsed, a referenced file is
    /// missing, or an `extends` chain contains a cycle.
    pub fn load(path: &Path) -> anyhow::Result<Scenario> {
        include::load(path)
    }

    /// Parse a scenario from a TOML string **without** resolving `extends` or
    /// `leek` file references (those need a base directory — use [`load`]).
    ///
    /// # Errors
    /// Parse errors.
    pub fn from_toml_str(s: &str) -> anyhow::Result<Scenario> {
        Ok(toml::from_str(s)?)
    }

    /// Parse a scenario from a JSON string (the official generator's shape),
    /// without resolving file references.
    ///
    /// # Errors
    /// Parse errors.
    pub fn from_json_str(s: &str) -> anyhow::Result<Scenario> {
        Ok(serde_json::from_str(s)?)
    }
}
