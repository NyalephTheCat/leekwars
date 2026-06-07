//! The scenario serde schema — one set of types for both TOML (canonical) and
//! the official generator's JSON.
//!
//! Most fields are `Option`/`Vec` so the *same* [`EntitySpec`] serves as a base
//! entity, a sparse profile patch, and a reusable `leek` template: a present
//! field overrides, an absent one inherits. Unknown keys are ignored (not an
//! error) so the official JSON's extra entity fields — `type`, `cores`,
//! `frequency`, … — load without complaint.

use std::collections::HashMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// A complete fight description. Scalar settings are `Option` so a scenario can
/// also act as an overlay (used by `extends`).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(from = "RawScenario")]
pub struct Scenario {
    /// Path to a base scenario this one inherits from (resolved at load time;
    /// `None` after resolution).
    pub extends: Option<String>,
    /// Combat RNG seed (`random_seed` in the official JSON).
    pub seed: Option<u64>,
    /// Turn limit (default 64 at build time).
    pub max_turns: Option<u32>,
    /// Language version the AIs compile under (default 4).
    pub version: Option<u8>,
    /// Strict-mode compile flag (default false).
    pub strict: Option<bool>,
    /// The arena.
    pub map: Option<MapSpec>,
    /// Farmer metadata (owners that group leeks; labels only).
    pub farmers: Vec<FarmerSpec>,
    /// Team metadata (labels only).
    pub teams: Vec<TeamSpec>,
    /// The combatants, flattened to one list with an explicit `team` per entity.
    pub entities: Vec<EntitySpec>,
    /// Named override blocks applied with [`Scenario::apply_profile`].
    pub profiles: HashMap<String, ScenarioPatch>,
    /// File-driven testing configuration (matrix / tournament / random).
    pub testing: Option<TestingSpec>,
}

/// The arena: a `width × height` grid with blocking cells.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MapSpec {
    pub width: i64,
    pub height: i64,
    #[serde(default)]
    pub obstacles: Vec<i64>,
}

/// A farmer — owns/groups leeks. Metadata only (the engine has no farmer
/// concept); mirrors the official `FarmerInfo`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FarmerSpec {
    pub id: i64,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub country: Option<String>,
}

/// Team metadata (label for reporting).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TeamSpec {
    pub id: i64,
    #[serde(default)]
    pub name: Option<String>,
}

/// One combatant. Every field is optional so the type doubles as a sparse patch
/// (in profiles) and a reusable template (in `leek` files); `id` identifies the
/// entity when merging.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct EntitySpec {
    #[serde(default)]
    pub id: Option<i64>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub cell: Option<i64>,
    #[serde(default)]
    pub team: Option<i64>,
    /// Owning farmer id (links to a [`FarmerSpec`]).
    #[serde(default)]
    pub farmer: Option<i64>,
    /// Path to this entity's `.leek` AI, relative to the scenario file.
    #[serde(default)]
    pub ai: Option<PathBuf>,
    /// Path to a reusable leek-settings file whose fields this entity inherits
    /// (this entity's own present fields win). Resolved away at load time.
    #[serde(default)]
    pub leek: Option<PathBuf>,
    #[serde(default)]
    pub life: Option<i64>,
    #[serde(default)]
    pub mp: Option<i64>,
    #[serde(default)]
    pub tp: Option<i64>,
    #[serde(default)]
    pub strength: Option<i64>,
    #[serde(default)]
    pub wisdom: Option<i64>,
    #[serde(default)]
    pub agility: Option<i64>,
    #[serde(default)]
    pub resistance: Option<i64>,
    #[serde(default)]
    pub science: Option<i64>,
    #[serde(default)]
    pub magic: Option<i64>,
    #[serde(default)]
    pub power: Option<i64>,
    #[serde(default)]
    pub level: Option<i64>,
    #[serde(default)]
    pub damage_return: Option<i64>,
    /// Owned weapons (`WEAPON_*` ids); the first is equipped.
    #[serde(default)]
    pub weapons: Vec<i64>,
    /// Owned chips (`CHIP_*` ids); validated, invoked by the AI at runtime.
    #[serde(default)]
    pub chips: Vec<i64>,
}

/// A sparse overlay applied to a [`Scenario`] (a `[profiles.<name>]` block or a
/// synthetic patch built from CLI overrides). Entities are matched by `id`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ScenarioPatch {
    #[serde(default, alias = "random_seed")]
    pub seed: Option<u64>,
    #[serde(default)]
    pub max_turns: Option<u32>,
    #[serde(default)]
    pub version: Option<u8>,
    #[serde(default)]
    pub strict: Option<bool>,
    #[serde(default)]
    pub map: Option<MapSpec>,
    #[serde(default)]
    pub entities: Vec<EntitySpec>,
}

/// File-driven testing configuration (the `[testing]` table). CLI flags layer
/// on top of these.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TestingSpec {
    #[serde(default)]
    pub mode: Option<TestMode>,
    /// Team treated as "the AI under test" for win/loss accounting.
    #[serde(default)]
    pub hero_team: Option<i64>,
    #[serde(default)]
    pub seeds: Vec<u64>,
    #[serde(default)]
    pub opponents: Vec<PathBuf>,
    #[serde(default)]
    pub profiles: Vec<String>,
    #[serde(default)]
    pub entrants: Vec<PathBuf>,
    #[serde(default)]
    pub bracket: Option<Bracket>,
    #[serde(default)]
    pub games: Option<u32>,
    #[serde(default)]
    pub random: Option<RandomSpec>,
}

/// Which testing driver to run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TestMode {
    Single,
    Matrix,
    Tournament,
    Random,
}

/// Tournament format.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Bracket {
    #[default]
    RoundRobin,
    SingleElim,
}

/// A combat stat eligible for randomized point-buy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum StatKind {
    Strength,
    Agility,
    Wisdom,
    Resistance,
    Science,
    Magic,
    Power,
}

/// Whose build the randomized point-buy fuzzer varies.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RandomTarget {
    Hero,
    #[default]
    Opponent,
    Both,
}

/// Randomized point-buy build fuzzing. Distributes `capital` stat points across
/// `stats` (seeded, reproducible) for `runs` fights.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RandomSpec {
    #[serde(default = "default_runs")]
    pub runs: u32,
    pub capital: i64,
    #[serde(default = "default_random_stats")]
    pub stats: Vec<StatKind>,
    /// Floor applied to every eligible stat before random distribution.
    #[serde(default)]
    pub min_per_stat: i64,
    #[serde(default)]
    pub target: RandomTarget,
    #[serde(default)]
    pub seed: u64,
}

fn default_runs() -> u32 {
    20
}

fn default_random_stats() -> Vec<StatKind> {
    vec![StatKind::Strength, StatKind::Agility, StatKind::Wisdom]
}

// ---------------------------------------------------------------------------
// Raw deserialization shim: accept `entities` as either a flat list (canonical
// TOML) or a list-of-teams (official JSON), then normalize to a flat list.
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct RawScenario {
    #[serde(default)]
    extends: Option<String>,
    #[serde(default, alias = "random_seed")]
    seed: Option<u64>,
    #[serde(default)]
    max_turns: Option<u32>,
    #[serde(default)]
    version: Option<u8>,
    #[serde(default)]
    strict: Option<bool>,
    #[serde(default)]
    map: Option<MapSpec>,
    #[serde(default)]
    farmers: Vec<FarmerSpec>,
    #[serde(default)]
    teams: Vec<TeamSpec>,
    #[serde(default)]
    entities: EntitiesField,
    #[serde(default)]
    profiles: HashMap<String, ScenarioPatch>,
    #[serde(default)]
    testing: Option<TestingSpec>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum EntitiesField {
    /// Canonical: one flat list, each entity carries its `team`.
    Flat(Vec<EntitySpec>),
    /// Official JSON: one inner list per team (outer index → team id).
    Nested(Vec<Vec<EntitySpec>>),
}

impl Default for EntitiesField {
    fn default() -> Self {
        EntitiesField::Flat(Vec::new())
    }
}

impl From<RawScenario> for Scenario {
    fn from(r: RawScenario) -> Self {
        let entities = match r.entities {
            EntitiesField::Flat(v) => v,
            EntitiesField::Nested(groups) => {
                let mut out = Vec::new();
                for (idx, group) in groups.into_iter().enumerate() {
                    for mut e in group {
                        if e.team.is_none() {
                            e.team = Some(i64::try_from(idx).unwrap_or(0) + 1);
                        }
                        out.push(e);
                    }
                }
                out
            }
        };
        Scenario {
            extends: r.extends,
            seed: r.seed,
            max_turns: r.max_turns,
            version: r.version,
            strict: r.strict,
            map: r.map,
            farmers: r.farmers,
            teams: r.teams,
            entities,
            profiles: r.profiles,
            testing: r.testing,
        }
    }
}
