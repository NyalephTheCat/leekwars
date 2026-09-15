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
use leek_project::Input;
use leek_span::{FeatureFlags, SourceId};

use crate::schema::{EntitySpec, Scenario};

/// A scenario built into a fight: the world model, the compiled AIs keyed by
/// entity id, and the run parameters. The caller wraps `fight` with
/// [`leek_generator::shared`] to run it.
pub struct LoadedFight {
    pub fight: Fight,
    pub ais: HashMap<i64, Arc<HirFile>>,
    pub max_turns: u32,
    /// Operation budget of each AI turn.
    pub max_ops_per_turn: u64,
    pub version: u8,
    pub strict: bool,
}

/// The fight world without any AIs attached — the entities, map, and run
/// parameters. Used by the emitted standalone runner, which compiles its AIs
/// from embedded sources rather than from files.
pub struct World {
    pub fight: Fight,
    pub max_turns: u32,
    /// Operation budget of each AI turn.
    pub max_ops_per_turn: u64,
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
    let max_ops_per_turn = scn
        .max_ops_per_turn
        .unwrap_or(leek_generator::DEFAULT_MAX_OPS_PER_TURN);

    let first_id = scn.entities.first().and_then(|e| e.id).unwrap_or(0);
    let mut fight = Fight::new(map.width, map.height, first_id);
    if let Some(seed) = scn.seed {
        fight = fight.with_seed(seed);
    }
    for &cell in &map.obstacles {
        fight = fight.with_obstacle(cell);
    }
    for spec in &scn.entities {
        if strict {
            check_items_supported(spec)?;
        }
        fight = fight.with_entity(build_entity(spec)?);
    }
    Ok(World {
        fight,
        max_turns,
        max_ops_per_turn,
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
        max_ops_per_turn: world.max_ops_per_turn,
        version: world.version,
        strict: world.strict,
    })
}

/// Strict mode: reject entities equipped with items the engine can't fully
/// simulate — an item missing from the catalogs, or one carrying upstream
/// effect types the engine doesn't model yet (which a normal run would skip
/// with a fight-log warning).
fn check_items_supported(spec: &EntitySpec) -> Result<()> {
    let id = spec.id.unwrap_or(0);
    let check = |kind: &str, item: i64, unsupported: Option<Vec<u8>>| match unsupported {
        None => bail!("entity {id}: {kind} {item} is not in the engine catalog (strict mode)"),
        Some(ids) if !ids.is_empty() => bail!(
            "entity {id}: {kind} {item} uses upstream effect type(s) {ids:?} \
             the engine doesn't model (strict mode)"
        ),
        Some(_) => Ok(()),
    };
    for &item in &spec.weapons {
        check(
            "weapon",
            item,
            leek_generator::weapons::unsupported_effects(item),
        )?;
    }
    for &item in &spec.chips {
        check(
            "chip",
            item,
            leek_generator::chips::unsupported_effects(item),
        )?;
    }
    Ok(())
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
    // Chips need no equip step, but the leek only gets to cast the ones its
    // build lists: `useChip` refuses anything outside this set.
    e = e.with_chips(spec.chips.iter().copied());
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

/// Compile one `.leek` file to HIR — the same path the debugger's
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
    // Settle the AI's language settings here at the `Input` boundary: its own
    // `@version` pragma wins over the scenario/world version, and `@strict`
    // turns strict mode on. HIR lowering no longer re-reads pragmas.
    let lang = leek_span::pragma::LanguageSettings::resolve(source, None, version, strict);
    let src_id = SourceId::new(1).expect("source id 1 is non-zero");
    let input = Input {
        source: src_id,
        text: source.into(),
        version_byte: lang.version,
        strict: lang.strict,
        flags: FeatureFlags::from_env(),
    };

    // A database of its own: this compiles one pathless string, so there
    // is no file set for an `include(...)` to resolve against and nothing
    // to share with another AI's compile.
    let db = leek_db::LeekDb::default();
    let file = leek_db::input_file(&db, String::new(), &input);

    // The driver's stop-on-error rule, which is what the pipeline this
    // replaced wrapped its parse in: a file that does not parse reports
    // its syntax errors and nothing the later passes made of the wreckage.
    let parsed = leek_db::queries::parse_query(&db, file, leek_db::ProgramClasses::none(&db));
    let stage = if parsed
        .diagnostics
        .iter()
        .any(|d| d.severity == Severity::Error)
    {
        leek_db::queries::Stage::Parsed
    } else {
        leek_db::queries::Stage::Hir
    };

    let errors: Vec<String> = leek_db::queries::file_diagnostics_upto(&db, file, stage)
        .iter()
        .filter(|d| matches!(d.severity, Severity::Error))
        .map(|d| d.message.clone())
        .collect();
    if !errors.is_empty() {
        bail!("compiling {label}:\n{}", errors.join("\n"));
    }

    Ok(leek_db::queries::lower_hir_query(&db, file).hir)
}

#[cfg(test)]
mod tests {
    use super::{build_entity, build_world, compile_ai_source};
    use crate::schema::{EntitySpec, Scenario};

    /// A spec with nothing but the two fields every entity needs, so each case
    /// below adds exactly the one field it is about.
    fn spec() -> EntitySpec {
        EntitySpec {
            id: Some(7),
            cell: Some(3),
            ..EntitySpec::default()
        }
    }

    /// The spec → [`Entity`](leek_generator::Entity) mapping, field by field: a
    /// field a scenario sets reaches the entity, and one it leaves out keeps
    /// the engine default — including the *other* half of a paired builder
    /// like `with_points`, which takes MP and TP together whether or not the
    /// scenario named both.
    #[test]
    fn build_entity_maps_the_fields_a_spec_sets_and_defaults_the_rest() {
        let bare = build_entity(&spec()).expect("entity");
        assert_eq!((bare.id, bare.cell, bare.team), (7, 3, 0));
        assert_eq!(bare.name, "entity7", "the id names an unnamed entity");
        assert_eq!((bare.life, bare.mp, bare.tp, bare.level), (100, 5, 10, 1));

        // `mp` alone must not zero the TP the engine gave the leek, nor `tp`
        // the MP: each is applied against the current value of the other.
        let mp_only = build_entity(&EntitySpec {
            mp: Some(9),
            ..spec()
        })
        .expect("entity");
        assert_eq!(
            (mp_only.mp, mp_only.max_mp, mp_only.tp, mp_only.max_tp),
            (9, 9, 10, 10)
        );
        let tp_only = build_entity(&EntitySpec {
            tp: Some(21),
            ..spec()
        })
        .expect("entity");
        assert_eq!(
            (tp_only.mp, tp_only.max_mp, tp_only.tp, tp_only.max_tp),
            (5, 5, 21, 21)
        );

        // Weapons are owned but not equipped, exactly like a real leek: the AI
        // has to `setWeapon(getWeapons()[0])` first.
        let armed = build_entity(&EntitySpec {
            weapons: vec![37, 45],
            ..spec()
        })
        .expect("entity");
        assert_eq!(armed.inventory, vec![37, 45]);
        assert_eq!(armed.weapon, None, "a scenario weapon is not pre-equipped");

        // Chips need no equip step; `with_chips` owns them and drops repeats.
        let chipped = build_entity(&EntitySpec {
            chips: vec![2, 2, 3],
            ..spec()
        })
        .expect("entity");
        assert_eq!(chipped.chips, vec![2, 3]);
        assert!(chipped.inventory.is_empty(), "chips are not weapons");

        // The stats with no builder go straight to the public fields.
        let statted = build_entity(&EntitySpec {
            agility: Some(41),
            power: Some(42),
            level: Some(43),
            damage_return: Some(44),
            ..spec()
        })
        .expect("entity");
        assert_eq!(
            (
                statted.agility,
                statted.power,
                statted.level,
                statted.damage_return
            ),
            (41, 42, 43, 44)
        );

        // The two fields that aren't optional at all.
        let no_id =
            build_entity(&EntitySpec { id: None, ..spec() }).expect_err("an entity needs an id");
        assert!(no_id.to_string().contains("missing an `id`"), "{no_id}");
        let no_cell = build_entity(&EntitySpec {
            cell: None,
            ..spec()
        })
        .expect_err("an entity needs a cell");
        assert!(
            no_cell.to_string().contains("missing a `cell`"),
            "{no_cell}"
        );
    }

    /// Regression (#38): the scenario had no op-budget setting and fights ran
    /// unbounded. `max_ops_per_turn` defaults to the official 20M, can be set
    /// at the top level, and overlays from a profile like the other settings.
    #[test]
    fn max_ops_per_turn_defaults_to_official_budget_and_is_configurable() {
        let scenario = |top: &str| {
            Scenario::from_toml_str(&format!(
                "{top}\n\
                 [map]\nwidth = 5\nheight = 5\n\
                 [[entities]]\nid = 1\ncell = 0\n\
                 [profiles.tight]\nmax_ops_per_turn = 1000\n"
            ))
            .expect("parse scenario")
        };

        let default = build_world(&scenario("")).expect("world");
        assert_eq!(
            default.max_ops_per_turn,
            leek_generator::DEFAULT_MAX_OPS_PER_TURN
        );
        assert_eq!(default.max_ops_per_turn, 20_000_000);

        let mut set = scenario("max_ops_per_turn = 5000");
        assert_eq!(build_world(&set).expect("world").max_ops_per_turn, 5000);
        set.apply_profile("tight").expect("profile");
        assert_eq!(build_world(&set).expect("world").max_ops_per_turn, 1000);
    }

    /// `class` is only a keyword from v2 on, so this AI compiles only when its
    /// own `@version:1` pragma wins over the world's default version.
    #[test]
    fn ai_version_pragma_wins_over_world_version() {
        let src = "// @version:1\nvar class = 1\nreturn class\n";
        compile_ai_source(src, "v1-ai", 4, false).expect("v1 AI should compile");
    }
}
