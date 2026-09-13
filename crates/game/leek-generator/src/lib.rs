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
//!
//! **Budget and errors**: every AI turn runs under a per-turn operation budget
//! ([`DEFAULT_MAX_OPS_PER_TURN`] unless the caller configures one). Like the
//! official generator's `EntityAI.runTurn`, an AI that exhausts it or faults
//! loses the rest of that turn, the error is recorded against the entity
//! ([`Outcome::errors`]), and the fight goes on.

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

/// The official per-turn operation budget: `AI.MAX_OPERATIONS` (20M), which
/// `EntityAI.runTurn` resets at the start of every turn. An AI that goes over
/// it gets `TOO_MUCH_OPERATIONS` and loses the rest of its turn.
pub const DEFAULT_MAX_OPS_PER_TURN: u64 = leek_backend_native::DEFAULT_OP_BUDGET;

/// The backend `op_limit` that enforces a per-turn budget of
/// `max_ops_per_turn` the way the official generator does.
///
/// Java's `AI.ops` throws `TOO_MUCH_OPERATIONS` once the count *reaches* the
/// budget (`mOperations >= maxOperations`), while the native backend keeps its
/// interpreter-parity rule of raising only when the count *exceeds* its limit.
/// A limit one below the budget makes the two agree: a turn whose charges land
/// exactly on `max_ops_per_turn` errors, as in Java.
#[must_use]
pub const fn fight_op_limit(max_ops_per_turn: u64) -> u64 {
    max_ops_per_turn.saturating_sub(1)
}

/// The standard fight launch options: release profile, the fight builtins
/// linked, the given language settings, and a per-turn op budget of
/// `max_ops_per_turn` (see [`fight_op_limit`]). Every run is one turn and the
/// backend resets its counter per run, so the budget is per turn.
#[must_use]
pub fn fight_options(version: u8, strict: bool, max_ops_per_turn: u64) -> NativeOptions {
    NativeOptions::release()
        .with_lang(version, strict)
        .with_link_game(true)
        .with_op_limit(fight_op_limit(max_ops_per_turn))
}

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
/// The caller chooses the options — pass [`fight_options`] for a normal fight
/// or `NativeOptions::debug()…with_debug_hooks(true)` to run the AI under the
/// debugger. `opts` is expected to have `with_link_game(true)`.
///
/// # Errors
/// Propagates a [`NativeError`] if the AI isn't in the native subset, faults,
/// or exhausts its op budget. Actions it took before that stay applied.
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

/// Launch one AI with the standard fight options and the official per-turn
/// budget. Convenience wrapper over [`run_ai_with`].
///
/// # Errors
/// Same as [`run_ai_with`].
pub fn run_ai(
    fight: &FightRef,
    hir: &HirFile,
    version: u8,
    strict: bool,
) -> Result<Value, NativeError> {
    run_ai_with(
        fight,
        hir,
        &fight_options(version, strict, DEFAULT_MAX_OPS_PER_TURN),
    )
}

/// An AI turn that ended in an error: the fight kept going, and this records
/// what went wrong for the report (the engine-native counterpart of the
/// official farmer-log entry).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AiError {
    /// Turn the error happened on.
    pub turn: u32,
    /// Entity whose AI errored.
    pub entity: i64,
    /// What went wrong: the runtime error code (`TOO_MUCH_OPERATIONS`,
    /// `STACKOVERFLOW`, …) or the compile/unsupported message.
    pub error: String,
}

impl AiError {
    fn new(turn: u32, entity: i64, err: &NativeError) -> Self {
        let error = match err {
            // The bare code, like the official log key, not "runtime error: …".
            NativeError::Runtime(code) => code.clone(),
            other => other.to_string(),
        };
        Self {
            turn,
            entity,
            error,
        }
    }
}

impl std::fmt::Display for AiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "turn {}: entity {}: {}",
            self.turn, self.entity, self.error
        )
    }
}

/// How a fight ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outcome {
    /// The lone surviving team, or `None` for a draw (no survivors, or the
    /// turn limit was hit with multiple teams alive).
    pub winner_team: Option<i64>,
    /// Turns played.
    pub turns: u32,
    /// Every AI turn that ended in an error, in the order they happened.
    pub errors: Vec<AiError>,
}

/// The turn loop, generic over how an entity's AI **and its run options** are
/// looked up. Each turn, every living entity (in id order) regenerates MP/TP,
/// ticks effects, and runs its AI once under the options `get_ai` returns for
/// it. Stops when at most one team remains or after `max_turns`. Entities for
/// which `get_ai` returns `None` act only as targets. Returning per-entity
/// options lets the debugger run one entity with debug hooks and the rest
/// without (see [`run_fight_debug`]).
///
/// An AI that errors (a runtime fault, an exhausted op budget, or code outside
/// the native subset) ends its own turn, keeping the actions it already took;
/// the error goes into [`Outcome::errors`] and the loop moves on to the next
/// entity, like `EntityAI.runTurn`'s catch blocks. So one faulty AI can't abort
/// the fight or anything driving it.
fn fight_loop<'a>(
    fight: &FightRef,
    max_turns: u32,
    get_ai: impl Fn(i64) -> Option<(&'a HirFile, &'a NativeOptions)>,
) -> Outcome {
    let mut errors = Vec::new();
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
            if let Some((hir, opts)) = get_ai(id)
                && let Err(e) = run_ai_with(fight, hir, opts)
            {
                errors.push(AiError::new(turn, id, &e));
            }
            if fight.borrow().living_teams().len() <= 1 {
                return Outcome {
                    winner_team: fight.borrow().living_teams().first().copied(),
                    turns: turn,
                    errors,
                };
            }
        }
    }
    Outcome {
        winner_team: fight
            .borrow()
            .living_teams()
            .first()
            .copied()
            .filter(|_| fight.borrow().living_teams().len() == 1),
        turns: max_turns,
        errors,
    }
}

/// Run the fight to a conclusion with the standard fight options (see
/// [`fight_options`]; [`fight_loop`] for the turn and error semantics).
/// `max_ops_per_turn` is each AI turn's op budget — pass
/// [`DEFAULT_MAX_OPS_PER_TURN`] for the official one.
pub fn run_fight(
    fight: &FightRef,
    ais: &HashMap<i64, HirFile>,
    max_turns: u32,
    version: u8,
    strict: bool,
    max_ops_per_turn: u64,
) -> Outcome {
    let opts = fight_options(version, strict, max_ops_per_turn);
    fight_loop(fight, max_turns, |id| ais.get(&id).map(|h| (h, &opts)))
}

/// Run the fight to a conclusion under explicit [`NativeOptions`], with AIs
/// shared via [`Arc`] (so callers — the matrix runner, the debugger — can hold
/// the compiled HIR across constructions without cloning it). `opts` is
/// expected to have `with_link_game(true)`, the desired language/version, and
/// a finite per-turn op budget (an unlimited one lets a looping AI hang the
/// fight).
pub fn run_fight_with(
    fight: &FightRef,
    ais: &HashMap<i64, Arc<HirFile>>,
    max_turns: u32,
    opts: &NativeOptions,
) -> Outcome {
    fight_loop(fight, max_turns, |id| {
        ais.get(&id).map(|a| (a.as_ref(), opts))
    })
}

/// Run a fight with the standard fight options and [`Arc`]-shared AIs. The
/// convenience wrapper most callers (the scenario runner, the matrix/tournament
/// drivers) want: it builds [`fight_options`] and delegates to
/// [`run_fight_with`].
pub fn run_fight_release(
    fight: &FightRef,
    ais: &HashMap<i64, Arc<HirFile>>,
    max_turns: u32,
    version: u8,
    strict: bool,
    max_ops_per_turn: u64,
) -> Outcome {
    let opts = fight_options(version, strict, max_ops_per_turn);
    run_fight_with(fight, ais, max_turns, &opts)
}

/// Run a fight where a single entity is debugged: `debug_entity`'s AI runs under
/// `debug_opts` (expected to carry `with_debug_hooks(true)`), every other AI
/// under `other_opts`. Because only the debugged AI is compiled with debug
/// hooks, only it emits safepoints — so the process-global debug hook fires for
/// that entity alone, keeping breakpoints scoped to the AI under test even
/// though all AIs share the loop.
pub fn run_fight_debug(
    fight: &FightRef,
    ais: &HashMap<i64, Arc<HirFile>>,
    max_turns: u32,
    debug_entity: i64,
    debug_opts: &NativeOptions,
    other_opts: &NativeOptions,
) -> Outcome {
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
