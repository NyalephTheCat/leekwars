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
pub use leek_game_runtime::official_items;
pub use leek_game_runtime::state::{
    BulbTemplate, ChipSpec, FightLoadout, Fighter, STAT_AGILITY, STAT_FREQUENCY, STAT_LIFE,
    STAT_MP, STAT_RESISTANCE, STAT_STRENGTH, STAT_TP, STAT_WISDOM, State, Stats, Team, WeaponSpec,
};

use crate::{AiPrograms, RuntimeGuard};
use leek_backend_native::ids::fn_id;
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
/// escaping. See [`harvest_run`] for which ops count.
fn run_entity_ai(
    state: &Rc<RefCell<State>>,
    programs: &mut AiPrograms,
    fid: usize,
    hir: &HirFile,
    opts: &NativeOptions,
) -> u64 {
    let result = programs.get(hir, opts).and_then(|program| {
        let _guard = RuntimeGuard::install(OfficialRuntime {
            state: Rc::clone(state),
            current: fid,
        });
        program.run(opts)
    });
    harvest_run(state, fid, fid, &result)
}

/// Finish an AI run: log its error, if any (see [`log_ai_error`]), and return
/// the ops it used.
///
/// Only a run that actually executed has ops to report. A runtime error
/// (fault, exhausted budget) ends a program that ran, so the ops it used up to
/// the error count. A compile or unsupported error happens before the backend
/// arms its op counter, which then still holds a previous run's count (often
/// another entity's), so that run reports 0.
fn harvest_run(
    state: &Rc<RefCell<State>>,
    acting: usize,
    log_fid: usize,
    result: &Result<Value, NativeError>,
) -> u64 {
    match result {
        Ok(_) => ops_used(),
        Err(e) => {
            log_ai_error(&mut state.borrow_mut(), acting, log_fid, e);
            if e.runtime_code().is_some() {
                ops_used()
            } else {
                0
            }
        }
    }
}

/// Run a bulb's turn: invoke the AI function stored at `summon()` time inside
/// the *owner's* compiled module, with `current` pointing at the bulb.
/// Mirrors `BulbAI.runIA` (`mOwnerAI.mEntity = mEntity` + `mAIFunction.run`);
/// the owner's `runTurn` resets its entity back at its next turn, which our
/// per-run `current` models for free.
///
/// The run is charged to the owner at both ends: its operations land on the
/// owner's counter, and it is given only what is left of the owner's budget
/// for this turn — `owner_turn_ops` is what the owner has already spent in it.
/// `mAIFunction.run(mOwnerAI, …)` charges `mOwnerAI.mOperations`, and the only
/// `resetCounter()` that clears *that* counter is the owner's own
/// `EntityAI.runTurn`: `BulbAI` inherits `runTurn`, so the counter its reset
/// clears is the bulb's own unused one. A bulb therefore shares one
/// `AI.MAX_OPERATIONS` with its owner instead of being handed a second one.
///
/// An error is contained like [`run_entity_ai`]'s. `BulbAI` shares its
/// owner's `LeekLog`, so the log entry carries the owner's fid while the
/// `ActionAIError` names the bulb.
fn run_bulb_ai(
    state: &Rc<RefCell<State>>,
    programs: &mut AiPrograms,
    fid: usize,
    owner: usize,
    ai_fn: &Value,
    hir: &HirFile,
    opts: &NativeOptions,
    owner_turn_ops: u64,
) -> u64 {
    // `opts.op_limit` already carries the reach-the-budget adjustment of
    // [`crate::fight_op_limit`], so the per-turn budget it was built from is
    // one above it; feeding the owner's remainder back through the same helper
    // reproduces Java's boundary on the counter they share.
    let budget = opts.op_limit.saturating_add(1);
    let run_opts = opts
        .clone()
        .with_op_limit(crate::fight_op_limit(budget.saturating_sub(owner_turn_ops)));
    // The owner's turn module, already compiled — a bulb turn no longer
    // re-JITs the whole owning AI. The lookup stays keyed on the *unnarrowed*
    // `opts`: `op_limit` is armed per run by `run_call` and is deliberately no
    // part of `CodegenKey`, and keying on a budget that shrinks every turn
    // would compile a fresh module per bulb turn again (#112).
    let result = programs.get(hir, opts).and_then(|program| {
        let _guard = RuntimeGuard::install(OfficialRuntime {
            state: Rc::clone(state),
            current: fid,
        });
        program.run_call(&run_opts, ai_fn, Vec::new())
    });
    harvest_run(state, fid, owner, &result)
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
    let (key, params) = match err.runtime_code() {
        Some("TOO_MUCH_OPERATIONS") => (ERROR_TOO_MUCH_OPERATIONS, null_message()),
        Some("ARRAY_OUT_OF_BOUND") => (ERROR_ARRAY_OUT_OF_BOUND, null_message()),
        Some("STACKOVERFLOW") => (ERROR_STACKOVERFLOW, Some(serde_json::json!([]))),
        Some(other) => (ERROR_AI_INTERRUPTED, Some(serde_json::json!([other]))),
        // Compile / unsupported: the whole `Display` form is the one param,
        // as it was when this matched on the enum's other variants.
        None => (
            ERROR_AI_INTERRUPTED,
            Some(serde_json::json!([err.to_string()])),
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
/// index into `HirFile::defs`; [`leek_backend_native::ids::fn_id`] turns it
/// into the runtime handle the native backend resolves through `user_fn_idx`
/// once `hook_roots` has force-compiled it.
fn find_hook(hir: &HirFile, name: &str) -> Option<Value> {
    hir.defs.iter().enumerate().find_map(|(i, def)| match def {
        Def::Function(f) if f.name == name && f.params.is_empty() => u32::try_from(i)
            .ok()
            .map(|id| Value::Function(Function::User(fn_id(DefId(id))))),
        _ => None,
    })
}

/// `Fight.runHooks(name, phase)` — invoke the `name` hook of every entity that
/// defines it, in deterministic turn order. Each hook runs with the fight's
/// [`HookPhase`] set (so `setLoadout` is allowed and combat actions are gated)
/// and the AI's `current` entity installed. Hook operations are NOT charged to
/// the entity (`runHook` doesn't feed `statistics`), matching the reference.
/// A hook gets the turn budget plus `HOOK_OPS_BONUS` (`EntityAI.runHook`): since
/// `opts.op_limit` already carries the reach-the-budget adjustment of
/// [`crate::fight_op_limit`], adding the bonus to it gives the hook the same
/// boundary as Java. An error in a hook is logged like a turn error without
/// stopping the other hooks or the fight.
fn run_hooks(
    state: &Rc<RefCell<State>>,
    programs: &mut AiPrograms,
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
        // A distinct module from the turn one: `hook_roots` is part of the
        // codegen key, so the turn module's code stays byte-identical to what
        // it was before hooks existed as a separate compile.
        let result = programs.get(hir, &hook_opts).and_then(|program| {
            let _guard = RuntimeGuard::install(OfficialRuntime {
                state: Rc::clone(state),
                current: fid,
            });
            program.run_call(&hook_opts, &hook_fn, Vec::new())
        });
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
    // One compiled module per (AI, codegen options) for the whole fight — the
    // turn loop below runs each AI up to `MAX_TURNS` times.
    let mut programs = AiPrograms::default();
    // Total operations per fid, reported once at the end like
    // `Actions.addOpsAndTimes(state.statistics)`.
    let mut total_ops: HashMap<usize, u64> = HashMap::new();
    // Operations each AI has spent *within the turn it is in* — `AI.mOperations`,
    // which `EntityAI.runTurn` resets at the start of its entity's turn. Only
    // the bulbs read it (see [`run_bulb_ai`]): their runs continue their
    // owner's count instead of starting a second budget.
    let mut turn_ops: HashMap<usize, u64> = HashMap::new();

    state.borrow_mut().init();

    // `Fight.startFight`: the `beforeFight()` hooks run after init but before
    // the initial-state snapshot, so any `setLoadout()` they apply is reflected
    // in the report's max-life / displayed stats.
    run_hooks(
        &state,
        &mut programs,
        ais,
        opts,
        HookPhase::BeforeFight,
        "beforeFight",
    );

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
                // mOwnerAI, …)`, and come out of what the owner has left of
                // its budget for this turn.
                let summon = {
                    let s = state.borrow();
                    s.fighters[fid]
                        .summoner
                        .map(|owner| (owner, s.summon_ais.get(&fid).cloned()))
                };
                if let Some((owner, ai_fn)) = summon {
                    if let (Some(ai_fn), Some(hir)) = (ai_fn, ais.get(&owner)) {
                        let spent = turn_ops.get(&owner).copied().unwrap_or(0);
                        let ops = run_bulb_ai(
                            &state,
                            &mut programs,
                            fid,
                            owner,
                            &ai_fn,
                            hir,
                            opts,
                            spent,
                        );
                        *total_ops.entry(owner).or_insert(0) += ops;
                        // The bulb's ops stay on the owner's turn counter: a
                        // second bulb of the same owner gets what these leave.
                        *turn_ops.entry(owner).or_insert(0) += ops;
                    }
                } else {
                    // `EntityAI.runTurn` opens the entity's turn with
                    // `resetCounter()`, whether or not the run then errors —
                    // this is the reset the bulbs above spend against.
                    turn_ops.insert(fid, 0);
                    if let Some(hir) = ais.get(&fid) {
                        let ops = run_entity_ai(&state, &mut programs, fid, hir, opts);
                        *total_ops.entry(fid).or_insert(0) += ops;
                        turn_ops.insert(fid, ops);
                    }
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
    run_hooks(
        &state,
        &mut programs,
        ais,
        opts,
        HookPhase::AfterFight,
        "afterFight",
    );

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
