//! Leek-wars fight orchestrator — the `leek-wars-generator` equivalent.
//!
//! **Launches AIs**: installs itself as the native backend's game runtime,
//! runs each entity's compiled script, and routes the fight builtins those
//! scripts call back to the shared [`Fight`] via
//! [`leek_game_runtime::call_game_builtin`].
//!
//! Layering (world model + fight functions in [`leek_game_runtime`], execution
//! in [`leek_backend_native`], orchestration here) joins at the
//! [`GameHost`](leek_game_runtime::GameHost) (state access) and
//! [`GameRuntime`](leek_backend_native::GameRuntime) (execution) seams.
//!
//! The **turn loop** ([`run_fight`] and friends) runs each living entity's AI
//! once per turn, regenerating MP/TP and ticking effects, until one team
//! remains or `max_turns` elapses.

pub mod official;

use std::collections::HashMap;
use std::sync::Arc;

use leek_game_runtime::{GameHost, call_game_builtin};
use leek_hir::HirFile;
use leek_runtime::Value;

// The world model lives in `leek_game_runtime`; re-export it so fight setup
// (`Fight::new(…).with_entity(…)`) and orchestration come from one place.
// The item catalogs ride along for scenario validation, and the backend's
// run-options type so callers can configure launches without a direct
// `leek-backend-native` edge.
pub use leek_backend_native::{NativeError, NativeOptions};
pub use leek_game_runtime::{ActiveEffect, Entity, Fight, FightRef, chips, shared, weapons};

/// Bridges the native backend's game-runtime hook to the fight functions,
/// dispatching against the shared [`Fight`] as the
/// [`GameHost`](leek_game_runtime::GameHost).
struct FightRuntime(FightRef);

impl leek_backend_native::GameRuntime for FightRuntime {
    fn call(&mut self, name: &str, args: &[Value]) -> Value {
        call_game_builtin(&mut *self.0.borrow_mut(), name, args)
    }
}

/// Launch one AI under explicit [`NativeOptions`]: run its compiled `hir`
/// against `fight` (the fight's current entity is the subject), with the fight
/// builtins linked in. Returns the AI's value.
///
/// The caller chooses the options — pass `NativeOptions::release()…` for a
/// normal fight or `NativeOptions::debug()…with_debug_hooks(true)` to run the
/// AI under the debugger. `opts` is expected to have `with_link_game(true)`.
///
/// # Errors
/// Propagates a [`NativeError`] if the AI isn't in the native subset.
pub fn run_ai_with(
    fight: &FightRef,
    hir: &HirFile,
    opts: &NativeOptions,
) -> Result<Value, NativeError> {
    leek_backend_native::set_game_runtime(Some(Box::new(FightRuntime(fight.clone()))));
    let result = leek_backend_native::run(hir, opts);
    leek_backend_native::set_game_runtime(None);
    result
}

/// Launch one AI with the default release profile. Convenience wrapper over
/// [`run_ai_with`].
///
/// # Errors
/// Propagates a [`NativeError`] if the AI isn't in the native subset.
pub fn run_ai(
    fight: &FightRef,
    hir: &HirFile,
    version: u8,
    strict: bool,
) -> Result<Value, NativeError> {
    let opts = NativeOptions::release()
        .with_lang(version, strict)
        .with_link_game(true);
    run_ai_with(fight, hir, &opts)
}

/// How a fight ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outcome {
    /// The lone surviving team, or `None` for a draw (no survivors, or the
    /// turn limit was hit with multiple teams alive).
    pub winner_team: Option<i64>,
    /// Turns played.
    pub turns: u32,
}

/// The turn loop, generic over how an entity's AI **and its run options** are
/// looked up. Each turn, every living entity (in id order) regenerates MP/TP,
/// ticks effects, and runs its AI once under the options `get_ai` returns for
/// it. Stops when at most one team remains or after `max_turns`. Entities for
/// which `get_ai` returns `None` act only as targets. Returning per-entity
/// options lets the debugger run one entity with debug hooks and the rest
/// without (see [`run_fight_debug`]).
fn fight_loop<'a>(
    fight: &FightRef,
    max_turns: u32,
    get_ai: impl Fn(i64) -> Option<(&'a HirFile, &'a NativeOptions)>,
) -> Result<Outcome, NativeError> {
    for turn in 1..=max_turns {
        fight.borrow_mut().set_turn(i64::from(turn));
        let order: Vec<i64> = {
            let mut ids = fight.borrow().entities(true);
            ids.sort_unstable();
            ids
        };
        for id in order {
            // Skip entities killed earlier this turn.
            if fight.borrow().life(id).is_none_or(|l| l <= 0) {
                continue;
            }
            {
                let mut f = fight.borrow_mut();
                f.set_current(id);
                f.regen(id);
                f.tick_effects(id); // poison damage + expire shields/buffs
            }
            // Poison may have killed the entity before it acts.
            if fight.borrow().life(id).is_none_or(|l| l <= 0) {
                continue;
            }
            if let Some((hir, opts)) = get_ai(id) {
                run_ai_with(fight, hir, opts)?;
            }
            if fight.borrow().living_teams().len() <= 1 {
                return Ok(Outcome {
                    winner_team: fight.borrow().living_teams().first().copied(),
                    turns: turn,
                });
            }
        }
    }
    Ok(Outcome {
        winner_team: fight
            .borrow()
            .living_teams()
            .first()
            .copied()
            .filter(|_| fight.borrow().living_teams().len() == 1),
        turns: max_turns,
    })
}

/// Run the fight to a conclusion with the default release profile. Convenience
/// wrapper over [`run_fight_with`] (see [`fight_loop`] for the turn semantics).
///
/// # Errors
/// Propagates the first [`NativeError`] from launching an AI.
pub fn run_fight(
    fight: &FightRef,
    ais: &HashMap<i64, HirFile>,
    max_turns: u32,
    version: u8,
    strict: bool,
) -> Result<Outcome, NativeError> {
    let opts = NativeOptions::release()
        .with_lang(version, strict)
        .with_link_game(true);
    fight_loop(fight, max_turns, |id| ais.get(&id).map(|h| (h, &opts)))
}

/// Run the fight to a conclusion under explicit [`NativeOptions`], with AIs
/// shared via [`Arc`] (so callers — the matrix runner, the debugger — can hold
/// the compiled HIR across constructions without cloning it). `opts` is
/// expected to have `with_link_game(true)` and the desired language/version.
///
/// # Errors
/// Propagates the first [`NativeError`] from launching an AI.
pub fn run_fight_with(
    fight: &FightRef,
    ais: &HashMap<i64, Arc<HirFile>>,
    max_turns: u32,
    opts: &NativeOptions,
) -> Result<Outcome, NativeError> {
    fight_loop(fight, max_turns, |id| {
        ais.get(&id).map(|a| (a.as_ref(), opts))
    })
}

/// Run a fight with the default release profile and [`Arc`]-shared AIs. The
/// convenience wrapper most callers (the scenario runner, the matrix/tournament
/// drivers) want: it builds the standard release [`NativeOptions`] with the
/// game builtins linked and delegates to [`run_fight_with`].
///
/// # Errors
/// Propagates the first [`NativeError`] from launching an AI.
pub fn run_fight_release(
    fight: &FightRef,
    ais: &HashMap<i64, Arc<HirFile>>,
    max_turns: u32,
    version: u8,
    strict: bool,
) -> Result<Outcome, NativeError> {
    let opts = NativeOptions::release()
        .with_lang(version, strict)
        .with_link_game(true);
    run_fight_with(fight, ais, max_turns, &opts)
}

/// Run a fight where a single entity is debugged: `debug_entity`'s AI runs under
/// `debug_opts` (expected to carry `with_debug_hooks(true)`), every other AI
/// under `other_opts`. Because only the debugged AI is compiled with debug
/// hooks, only it emits safepoints — so the process-global debug hook fires for
/// that entity alone, keeping breakpoints scoped to the AI under test even
/// though all AIs share the loop.
///
/// # Errors
/// Propagates the first [`NativeError`] from launching an AI.
pub fn run_fight_debug(
    fight: &FightRef,
    ais: &HashMap<i64, Arc<HirFile>>,
    max_turns: u32,
    debug_entity: i64,
    debug_opts: &NativeOptions,
    other_opts: &NativeOptions,
) -> Result<Outcome, NativeError> {
    fight_loop(fight, max_turns, |id| {
        ais.get(&id).map(|a| {
            let opts = if id == debug_entity {
                debug_opts
            } else {
                other_opts
            };
            (a.as_ref(), opts)
        })
    })
}
