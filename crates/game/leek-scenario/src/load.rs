//! Turn a resolved [`Scenario`] into a runnable [`leek_generator::Fight`] plus
//! its compiled AIs — the one place the schema meets the engine. Reused by
//! `miku fight`, the [`testing`](crate::testing) drivers, and (later) the
//! debugger.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result, anyhow, bail};
use leek_diagnostics::Severity;
use leek_generator::{Entity, Fight};
use leek_hir::HirFile;
use leek_hir::pipeline::HirArtifact;
use leek_pipeline::{FeatureFlags, Input};
use leek_recipes::Target;
use leek_span::SourceId;

use crate::schema::{EntitySpec, Scenario};

/// A scenario built into a fight: the world model, the compiled AIs keyed by
/// entity id, and the run parameters. The caller wraps `fight` with
/// [`leek_generator::shared`] to run it.
pub struct LoadedFight {
    pub fight: Fight,
    pub ais: HashMap<i64, Arc<HirFile>>,
    pub max_turns: u32,
    pub version: u8,
    pub strict: bool,
}

/// The fight world without any AIs attached — the entities, map, and run
/// parameters. Used by the emitted standalone runner, which compiles its AIs
/// from embedded sources rather than from files.
pub struct World {
    pub fight: Fight,
    pub max_turns: u32,
    pub version: u8,
    pub strict: bool,
}

/// Build the fight world (entities + map + seed) from a resolved scenario,
/// compiling no AIs. The caller attaches AIs and runs it.
///
/// # Errors
/// Missing `[map]` or an entity without an `id`/`cell`.
pub fn build_world(scn: &Scenario) -> Result<World> {
    let map = scn
        .map
        .as_ref()
        .ok_or_else(|| anyhow!("scenario has no [map]"))?;
    let version = scn.version.unwrap_or(4);
    let strict = scn.strict.unwrap_or(false);
    let max_turns = scn.max_turns.unwrap_or(64);

    let first_id = scn.entities.first().and_then(|e| e.id).unwrap_or(0);
    let mut fight = Fight::new(map.width, map.height, first_id);
    if let Some(seed) = scn.seed {
        fight = fight.with_seed(seed);
    }
    for &cell in &map.obstacles {
        fight = fight.with_obstacle(cell);
    }
    for spec in &scn.entities {
        fight = fight.with_entity(build_entity(spec)?);
    }
    Ok(World {
        fight,
        max_turns,
        version,
        strict,
    })
}

/// Build a fight from a fully-resolved scenario, compiling each entity's AI.
/// `base_dir` resolves relative `ai` paths (the scenario file's directory).
///
/// # Errors
/// Missing `[map]`, an entity without an `id`/`cell`, or an AI that fails to
/// read/compile.
pub fn build_fight(scn: &Scenario, base_dir: &Path) -> Result<LoadedFight> {
    build_fight_with_cache(scn, base_dir, None)
}

/// Like [`build_fight`] but reuses already-compiled AIs from `cache` (keyed by
/// the joined AI path) — the matrix/tournament/random drivers compile each
/// distinct AI once and replay it across many fights. The returned `Fight` is
/// always fresh (a fight mutates as it runs and can't be shared).
///
/// # Errors
/// Same as [`build_fight`].
pub fn build_fight_with_cache(
    scn: &Scenario,
    base_dir: &Path,
    cache: Option<&HashMap<PathBuf, Arc<HirFile>>>,
) -> Result<LoadedFight> {
    let world = build_world(scn)?;

    let mut ais: HashMap<i64, Arc<HirFile>> = HashMap::new();
    for spec in &scn.entities {
        if let (Some(id), Some(ai_path)) = (spec.id, &spec.ai) {
            let joined = base_dir.join(ai_path);
            let hir = match cache.and_then(|c| c.get(&joined)) {
                Some(hir) => hir.clone(),
                None => compile_ai(&joined, world.version, world.strict)?,
            };
            ais.insert(id, hir);
        }
    }

    Ok(LoadedFight {
        fight: world.fight,
        ais,
        max_turns: world.max_turns,
        version: world.version,
        strict: world.strict,
    })
}

/// Construct a generator [`Entity`] from a spec, applying the present stats via
/// the builders (and the public fields for stats without a builder).
fn build_entity(spec: &EntitySpec) -> Result<Entity> {
    let id = spec
        .id
        .ok_or_else(|| anyhow!("entity is missing an `id`"))?;
    let cell = spec
        .cell
        .ok_or_else(|| anyhow!("entity {id} is missing a `cell`"))?;
    let team = spec.team.unwrap_or(0);
    let name = spec.name.clone().unwrap_or_else(|| format!("entity{id}"));

    let mut e = Entity::new(id, name, cell, team);
    if let Some(life) = spec.life {
        e = e.with_life(life);
    }
    if let Some(strength) = spec.strength {
        e = e.with_strength(strength);
    }
    if spec.mp.is_some() || spec.tp.is_some() {
        let (mp, tp) = (spec.mp.unwrap_or(e.mp), spec.tp.unwrap_or(e.tp));
        e = e.with_points(mp, tp);
    }
    if spec.wisdom.is_some()
        || spec.resistance.is_some()
        || spec.science.is_some()
        || spec.magic.is_some()
    {
        let (wisdom, resistance, science, magic) = (
            spec.wisdom.unwrap_or(e.wisdom),
            spec.resistance.unwrap_or(e.resistance),
            spec.science.unwrap_or(e.science),
            spec.magic.unwrap_or(e.magic),
        );
        e = e.with_magic_stats(wisdom, resistance, science, magic);
    }
    // Give the leek its weapons as owned inventory but leave it **unequipped**,
    // exactly like a real leek-wars fight: the AI must `setWeapon(...)` before
    // it can `useWeapon(...)`. (The first weapon is the conventional primary,
    // so AIs can `setWeapon(getWeapons()[0])`.)
    for &w in &spec.weapons {
        if !e.inventory.contains(&w) {
            e.inventory.push(w);
        }
    }
    // Stats without a builder — set the public fields directly.
    if let Some(agility) = spec.agility {
        e.agility = agility;
    }
    if let Some(power) = spec.power {
        e.power = power;
    }
    if let Some(level) = spec.level {
        e.level = level;
    }
    if let Some(dr) = spec.damage_return {
        e.damage_return = dr;
    }
    Ok(e)
}

/// Compile one `.leek` file to HIR — the same recipe path the debugger's
/// `NativeTarget::compile` uses. The leek-wars game builtins must already be
/// registered process-globally (via `--library leekwars`) for game AIs to
/// resolve.
///
/// # Errors
/// A read failure or any compile-error diagnostic.
pub fn compile_ai(path: &Path, version: u8, strict: bool) -> Result<Arc<HirFile>> {
    let source =
        std::fs::read_to_string(path).with_context(|| format!("reading AI {}", path.display()))?;
    compile_ai_source(&source, &path.display().to_string(), version, strict)
}

/// Compile a `.leek` AI from an in-memory source string (no file read). Used by
/// the emitted standalone runner, which embeds its AI sources. `label` names the
/// AI in error messages.
///
/// # Errors
/// Any compile-error diagnostic.
pub fn compile_ai_source(
    source: &str,
    label: &str,
    version: u8,
    strict: bool,
) -> Result<Arc<HirFile>> {
    let src_id = SourceId::new(1).expect("source id 1 is non-zero");
    let input = Input {
        source: src_id,
        text: source.into(),
        version_byte: version,
        strict,
        flags: FeatureFlags::from_env(),
    };

    let pipeline = leek_recipes::pipeline(Target::Hir, &leek_recipes::driver_params())
        .map_err(|e| anyhow!("building pipeline: {e}"))?;
    let run = pipeline.run(input);

    let errors: Vec<String> = run
        .diagnostics()
        .iter()
        .filter(|d| matches!(d.severity, Severity::Error))
        .map(|d| d.message.clone())
        .collect();
    if !errors.is_empty() {
        bail!("compiling {label}:\n{}", errors.join("\n"));
    }

    let hir = run
        .get::<HirArtifact>()
        .ok_or_else(|| anyhow!("compiling {label}: produced no HIR"))?;
    Ok(hir.0.clone())
}
