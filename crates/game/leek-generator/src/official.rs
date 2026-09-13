//! Official-conformance fight runner — `Fight.startFight` over the reference
//! [`State`], executing real compiled AIs through the native backend.
//!
//! Where the engine-native path ([`crate::run_fight_with`]) drives a
//! [`Fight`](crate::Fight) through [`call_game_builtin`]
//! (`leek_game_runtime::call_game_builtin`), this runner drives the official
//! [`State`] through
//! [`call_official_builtin`](leek_game_runtime::official_builtins) — the
//! reference-semantics dispatch the oracle goldens are verified against. The
//! turn loop is a line-for-line port of `Fight.startFight(true)`, and the
//! return value is the official Outcome JSON (the same document the Java
//! harness emits), ready to diff against a golden.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

// Scenario/bin callers build the official world through this module; export
// the model surface so they don't need a direct `leek-game-runtime` edge.
pub use leek_game_runtime::attack::{
    Area, EffectModifiers, EffectParams, EffectTargets, EffectType,
};
pub use leek_game_runtime::state::{
    BulbTemplate, ChipSpec, FightLoadout, Fighter, STAT_AGILITY, STAT_FREQUENCY, STAT_LIFE,
    STAT_MP, STAT_RESISTANCE, STAT_STRENGTH, STAT_TP, STAT_WISDOM, State, Stats, Team, WeaponSpec,
};

use leek_backend_native::{NativeError, NativeOptions, ops_used};
use leek_game_runtime::actions::Action;
use leek_game_runtime::official_builtins::call_official_builtin;
use leek_game_runtime::outcome::build_outcome;
use leek_game_runtime::state::{
    BeginTurn, ERROR_AI_INTERRUPTED, ERROR_ARRAY_OUT_OF_BOUND, ERROR_HELP_PAGE_LINK,
    ERROR_STACKOVERFLOW, ERROR_TOO_MUCH_OPERATIONS, HookPhase, LOG_SERROR, LOG_SSTANDARD,
    MAX_TURNS,
};
use leek_hir::{Def, DefId, HirFile};
use leek_runtime::{Function, Value};

/// Bridges the native backend's game hook to the official builtins: every
/// fight function the running AI calls is dispatched against the shared
/// [`State`] with `current` as the acting entity (`ai.getEntity()`).
struct OfficialRuntime {
    state: Rc<RefCell<State>>,
    current: usize,
}

impl leek_backend_native::GameRuntime for OfficialRuntime {
    fn call(&mut self, name: &str, args: &[Value]) -> Value {
        call_official_builtin(&mut self.state.borrow_mut(), self.current, name, args)
    }
}

/// `EntityAI.HOOK_OPS_BONUS`: a `beforeFight()` / `afterFight()` hook runs
/// with this many operations on top of the per-turn budget.
const HOOK_OPS_BONUS: u64 = 1_000_000;

/// Run one entity's AI for its turn: install the runtime, execute the
/// compiled HIR, harvest the op count. Mirrors `Fight.startTurn`'s
/// `entity.getAi().runTurn()`, including its catch blocks: an error ends the
/// turn and is logged against the entity (see [`log_ai_error`]) instead of
/// escaping. The ops the AI used up to the error still count.
fn run_entity_ai(
    state: &Rc<RefCell<State>>,
    fid: usize,
    hir: &HirFile,
    opts: &NativeOptions,
) -> u64 {
    leek_backend_native::set_game_runtime(Some(Box::new(OfficialRuntime {
        state: Rc::clone(state),
        current: fid,
    })));
    let result = leek_backend_native::run(hir, opts);
    leek_backend_native::set_game_runtime(None);
    if let Err(e) = result {
        log_ai_error(&mut state.borrow_mut(), fid, fid, &e);
    }
    ops_used()
}

/// Run a bulb's turn: invoke the AI function stored at `summon()` time inside
/// the *owner's* compiled module, with `current` pointing at the bulb.
/// Mirrors `BulbAI.runIA` (`mOwnerAI.mEntity = mEntity` + `mAIFunction.run`);
/// the owner's `runTurn` resets its entity back at its next turn, which our
/// per-run `current` models for free.
///
/// An error is contained like [`run_entity_ai`]'s. `BulbAI` shares its
/// owner's `LeekLog`, so the log entry carries the owner's fid while the
/// `ActionAIError` names the bulb.
fn run_bulb_ai(
    state: &Rc<RefCell<State>>,
    fid: usize,
    owner: usize,
    ai_fn: &Value,
    hir: &HirFile,
    opts: &NativeOptions,
) -> u64 {
    leek_backend_native::set_game_runtime(Some(Box::new(OfficialRuntime {
        state: Rc::clone(state),
        current: fid,
    })));
    let result = leek_backend_native::run_call(hir, opts, ai_fn, Vec::new());
    leek_backend_native::set_game_runtime(None);
    if let Err(e) = result {
        log_ai_error(&mut state.borrow_mut(), fid, owner, &e);
    }
    ops_used()
}

/// Record a contained AI error the way `EntityAI.runTurn` /
/// `handleLeekRunException` do: an `ActionAIError` for `acting` (the entity
/// whose AI was running), then a `SERROR` system log on `log_fid`'s farmer
/// log, plus the `too_much_ops` help link after an exhausted budget.
///
/// Log keys and params follow the Java throw sites: `TOO_MUCH_OPERATIONS` and
/// `ARRAY_OUT_OF_BOUND` are `LeekRunException`s, logged under their own
/// ordinal with `[e.getMessage()]` (`[null]`; the Java out-of-bounds message
/// parameters aren't reproduced), `STACKOVERFLOW` is a JVM
/// `StackOverflowError` logged with no params, and anything else — including
/// code the native backend can't compile — is `AI_INTERRUPTED` with the
/// message as its one param.
fn log_ai_error(state: &mut State, acting: usize, log_fid: usize, err: &NativeError) {
    let entity_id = i64::try_from(acting).expect("fid fits in i64");
    state.actions.log(Action::AiError { entity_id });
    let null_message = || Some(serde_json::json!([null]));
    let (key, params) = match err {
        NativeError::Runtime(code) => match code.as_str() {
            "TOO_MUCH_OPERATIONS" => (ERROR_TOO_MUCH_OPERATIONS, null_message()),
            "ARRAY_OUT_OF_BOUND" => (ERROR_ARRAY_OUT_OF_BOUND, null_message()),
            "STACKOVERFLOW" => (ERROR_STACKOVERFLOW, Some(serde_json::json!([]))),
            other => (ERROR_AI_INTERRUPTED, Some(serde_json::json!([other]))),
        },
        other => (
            ERROR_AI_INTERRUPTED,
            Some(serde_json::json!([other.to_string()])),
        ),
    };
    state.add_system_log_json(log_fid, LOG_SERROR, key, params);
    if key == ERROR_TOO_MUCH_OPERATIONS {
        state.add_system_log(
            log_fid,
            LOG_SSTANDARD,
            ERROR_HELP_PAGE_LINK,
            Some(&["too_much_ops"]),
        );
    }
}

/// A `Function::User` value for the top-level zero-arg function named `name`
/// in `hir`, or `None` when the AI defines no such function — the port of
/// `EntityAI.hasHook(name)` / `findHookMethod`. The `DefId` is the function's
/// index into `HirFile::defs`, which the native backend resolves through
/// `user_fn_idx` once `hook_roots` has force-compiled it.
fn find_hook(hir: &HirFile, name: &str) -> Option<Value> {
    hir.defs.iter().enumerate().find_map(|(i, def)| match def {
        Def::Function(f) if f.name == name && f.params.is_empty() => u32::try_from(i)
            .ok()
            .map(|id| Value::Function(Function::User(DefId(id)))),
        _ => None,
    })
}

/// `Fight.runHooks(name, phase)` — invoke the `name` hook of every entity that
/// defines it, in deterministic turn order. Each hook runs with the fight's
/// [`HookPhase`] set (so `setLoadout` is allowed and combat actions are gated)
/// and the AI's `current` entity installed. Hook operations are NOT charged to
/// the entity (`runHook` doesn't feed `statistics`), matching the reference.
/// A hook gets the turn budget plus `HOOK_OPS_BONUS`, and an error in it is
/// logged like a turn error without stopping the other hooks or the fight.
fn run_hooks(
    state: &Rc<RefCell<State>>,
    ais: &HashMap<usize, std::sync::Arc<HirFile>>,
    opts: &NativeOptions,
    phase: HookPhase,
    hook_name: &str,
) {
    let fids = state.borrow().order.fids().to_vec();
    let hook_opts = opts
        .clone()
        .with_hook_roots(vec![hook_name.to_string()])
        .with_op_limit(opts.op_limit.saturating_add(HOOK_OPS_BONUS));
    for fid in fids {
        let Some(hir) = ais.get(&fid) else { continue };
        let Some(hook_fn) = find_hook(hir, hook_name) else {
            continue;
        };
        state.borrow_mut().hook_phase = phase;
        leek_backend_native::set_game_runtime(Some(Box::new(OfficialRuntime {
            state: Rc::clone(state),
            current: fid,
        })));
        let result = leek_backend_native::run_call(hir, &hook_opts, &hook_fn, Vec::new());
        leek_backend_native::set_game_runtime(None);
        let mut s = state.borrow_mut();
        s.hook_phase = HookPhase::None;
        if let Err(e) = result {
            log_ai_error(&mut s, fid, fid, &e);
        }
    }
}

/// `Fight.startFight(true)` + Outcome assembly: run the official turn loop
/// over `state` (already populated with entities and weapon specs, but not
/// yet `init()`ed), executing each entity's compiled AI from `ais` (keyed by
/// fid; an absent entry acts as an idle AI). `farmers` keys the empty
/// per-farmer `logs` object, like the Java harness. Returns the official
/// Outcome JSON document.
///
/// `opts` is expected to carry `with_link_game(true)`, the language version,
/// and the per-turn op budget as its `op_limit` (`crate::fight_options` builds
/// all three; conformance runs want the release profile). An AI error never
/// aborts the fight: it ends that entity's turn and lands in the farmer logs
/// and actions, as in the Java generator.
pub fn run_official_fight(
    state: State,
    ais: &HashMap<usize, std::sync::Arc<HirFile>>,
    farmers: &[i64],
    opts: &NativeOptions,
) -> serde_json::Value {
    let state = Rc::new(RefCell::new(state));
    // Total operations per fid, reported once at the end like
    // `Actions.addOpsAndTimes(state.statistics)`.
    let mut total_ops: HashMap<usize, u64> = HashMap::new();

    state.borrow_mut().init();

    // `Fight.startFight`: the `beforeFight()` hooks run after init but before
    // the initial-state snapshot, so any `setLoadout()` they apply is reflected
    // in the report's max-life / displayed stats.
    run_hooks(&state, ais, opts, HookPhase::BeforeFight, "beforeFight");

    state.borrow_mut().record_initial_state();

    loop {
        {
            let s = state.borrow();
            if s.order.turn() > MAX_TURNS || !s.running {
                break;
            }
        }
        let begin = state.borrow_mut().begin_turn();
        match begin {
            BeginTurn::Act(fid) => {
                // A bulb runs the function value captured at `summon()` time
                // through the owner's module (`BulbAI`); one summoned via
                // `useChip` has no entry and idles (BULB_WITHOUT_AI). Ops
                // land on the owner's counter, like `mAIFunction.run(
                // mOwnerAI, …)`.
                let summon = {
                    let s = state.borrow();
                    s.fighters[fid]
                        .summoner
                        .map(|owner| (owner, s.summon_ais.get(&fid).cloned()))
                };
                if let Some((owner, ai_fn)) = summon {
                    if let (Some(ai_fn), Some(hir)) = (ai_fn, ais.get(&owner)) {
                        let ops = run_bulb_ai(&state, fid, owner, &ai_fn, hir, opts);
                        *total_ops.entry(owner).or_insert(0) += ops;
                    }
                } else if let Some(hir) = ais.get(&fid) {
                    let ops = run_entity_ai(&state, fid, hir, opts);
                    *total_ops.entry(fid).or_insert(0) += ops;
                }
                let mut s = state.borrow_mut();
                s.end_entity_turn(fid);
                s.end_turn();
            }
            BeginTurn::Skip => state.borrow_mut().end_turn(),
            BeginTurn::NoCurrent => {}
        }
        let mut s = state.borrow_mut();
        if s.order.current().is_none() {
            s.running = false;
            break;
        }
    }

    {
        let mut s = state.borrow_mut();
        for (&fid, &ops) in &total_ops {
            let fid = i64::try_from(fid).expect("fid fits in i64");
            let ops = i64::try_from(ops).unwrap_or(i64::MAX);
            s.actions.add_ops(fid, ops);
        }
        // `Fight.java` removes every invocation from its team *before*
        // `computeWinner` and `getDeadReport` — summons never appear in either.
        s.remove_all_invocations();
        // Store the winner so `getWinner()` reads the result inside `afterFight`.
        let winner = s.compute_winner(true);
        s.win_team = winner;
    }

    // `afterFight()` hooks run after the winner is computed.
    run_hooks(&state, ais, opts, HookPhase::AfterFight, "afterFight");

    let s = state.borrow();
    build_outcome(
        &s.leek_snapshots,
        &s.map,
        &s.actions,
        &s.teams,
        &s.fighters,
        farmers,
        &s.farmer_logs,
        s.win_team,
        s.duration(),
    )
}
