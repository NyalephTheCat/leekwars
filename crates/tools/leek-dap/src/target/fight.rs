//! The fight debug target: run `program` as one entity's AI inside a fight.
//!
//! Runs on the DAP worker thread (the generator's `FightRef` is `!Send` and the
//! game runtime is installed thread-locally). The process-global debug hook —
//! installed by `configuration_done` before the worker spawns — only fires for
//! the one entity compiled with debug hooks, so breakpoints stay scoped to the
//! AI under test even though the whole fight runs.

use std::path::Path;

use leek_backend_native::NativeOptions;
use leek_scenario::Scenario;

use super::native::Compiled;
use super::{LaunchConfig, RunOutcome};

/// Build the fight from `config.scenario`, swap `program` in for the debugged
/// entity, and run to a conclusion. Returns the fight outcome (or the first
/// failure) as a [`RunOutcome`].
pub(crate) fn run_fight_debug(config: &LaunchConfig, program: &Compiled) -> RunOutcome {
    let Some(scenario_path) = config.scenario.as_ref() else {
        return RunOutcome::failed("fight debug target requires a `scenario`");
    };

    // The fight's AIs use the leek-wars builtins; make them resolvable.
    if let Err(e) = leek_recipes::load_and_register_libraries(["leekwars"]) {
        return RunOutcome::failed(format!("registering the leekwars library: {e}"));
    }

    let mut scn = match Scenario::load(scenario_path) {
        Ok(scn) => scn,
        Err(e) => return RunOutcome::failed(format!("loading scenario: {e}")),
    };
    if let Some(profile) = &config.profile
        && let Err(e) = scn.apply_profile(profile)
    {
        return RunOutcome::failed(format!("applying profile: {e}"));
    }
    if config.seed.is_some() {
        scn.seed = config.seed;
    }
    if config.max_turns.is_some() {
        scn.max_turns = config.max_turns;
    }

    let base_dir = scenario_path.parent().unwrap_or_else(|| Path::new("."));
    let debug_entity = match pick_debug_entity(config, &scn, base_dir) {
        Ok(id) => id,
        Err(message) => return RunOutcome::failed(message),
    };

    let lf = match leek_scenario::build_fight(&scn, base_dir) {
        Ok(lf) => lf,
        Err(e) => return RunOutcome::failed(format!("building fight: {e}")),
    };

    // Swap the editor's compiled program in for the debugged entity so the exact
    // source the breakpoints sit on is what runs.
    let mut ais = lf.ais;
    ais.insert(debug_entity, program.hir.clone());

    let fight = leek_generator::shared(lf.fight);
    let debug_opts = NativeOptions::debug()
        .with_lang(program.version, program.strict)
        .with_link_game(true)
        .with_debug_hooks(true);
    let other_opts = NativeOptions::release()
        .with_lang(lf.version, lf.strict)
        .with_link_game(true);

    match leek_generator::run_fight_debug(
        &fight,
        &ais,
        lf.max_turns,
        debug_entity,
        &debug_opts,
        &other_opts,
    ) {
        Ok(outcome) => {
            let winner = outcome
                .winner_team
                .map_or_else(|| "draw".to_string(), |t| format!("team {t}"));
            RunOutcome {
                output: format!("fight over after {} turns — winner: {winner}\n", outcome.turns),
                exit_code: 0,
            }
        }
        Err(e) => RunOutcome::failed(format!("fight execution error: {e}")),
    }
}

/// Resolve which entity `program` controls: the explicit `fightEntity`, else the
/// entity whose `ai` resolves to `program`, else the first entity.
fn pick_debug_entity(config: &LaunchConfig, scn: &Scenario, base_dir: &Path) -> Result<i64, String> {
    if let Some(id) = config.fight_entity {
        return Ok(id);
    }
    let program = std::fs::canonicalize(&config.program).unwrap_or_else(|_| config.program.clone());
    if let Some(id) = scn.entities.iter().find_map(|e| {
        let ai = e.ai.as_ref()?;
        let joined = std::fs::canonicalize(base_dir.join(ai)).unwrap_or_else(|_| base_dir.join(ai));
        (joined == program).then_some(e.id).flatten()
    }) {
        return Ok(id);
    }
    scn.entities
        .first()
        .and_then(|e| e.id)
        .ok_or_else(|| "scenario has no entities to debug".to_string())
}
