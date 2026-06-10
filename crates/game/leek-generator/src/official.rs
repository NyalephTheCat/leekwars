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
pub use leek_game_runtime::state::{
    Fighter, STAT_AGILITY, STAT_FREQUENCY, STAT_LIFE, STAT_MP, STAT_RESISTANCE, STAT_STRENGTH,
    STAT_TP, STAT_WISDOM, State, Stats, Team, WeaponSpec,
};

use leek_backend_native::{NativeError, NativeOptions, ops_used};
use leek_game_runtime::official_builtins::call_official_builtin;
use leek_game_runtime::outcome::build_outcome;
use leek_game_runtime::state::{BeginTurn, MAX_TURNS};
use leek_hir::HirFile;
use leek_runtime::Value;

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

/// Run one entity's AI for its turn: install the runtime, execute the
/// compiled HIR, harvest the op count. Mirrors `Fight.startTurn`'s
/// `entity.getAi().runTurn()`.
fn run_entity_ai(
    state: &Rc<RefCell<State>>,
    fid: usize,
    hir: &HirFile,
    opts: &NativeOptions,
) -> Result<u64, NativeError> {
    leek_backend_native::set_game_runtime(Some(Box::new(OfficialRuntime {
        state: Rc::clone(state),
        current: fid,
    })));
    let result = leek_backend_native::run(hir, opts);
    leek_backend_native::set_game_runtime(None);
    result.map(|_| ops_used())
}

/// `Fight.startFight(true)` + Outcome assembly: run the official turn loop
/// over `state` (already populated with entities and weapon specs, but not
/// yet `init()`ed), executing each entity's compiled AI from `ais` (keyed by
/// fid; an absent entry acts as an idle AI). `farmers` keys the empty
/// per-farmer `logs` object, like the Java harness. Returns the official
/// Outcome JSON document.
///
/// `opts` is expected to carry `with_link_game(true)` and the language
/// version; conformance runs want the release profile.
///
/// # Errors
/// Propagates the first [`NativeError`] from launching an AI — conformance
/// fights are expected not to error, so we fail fast rather than play on.
pub fn run_official_fight(
    state: State,
    ais: &HashMap<usize, std::sync::Arc<HirFile>>,
    farmers: &[i64],
    opts: &NativeOptions,
) -> Result<serde_json::Value, NativeError> {
    let state = Rc::new(RefCell::new(state));
    // Total operations per fid, reported once at the end like
    // `Actions.addOpsAndTimes(state.statistics)`.
    let mut total_ops: HashMap<usize, u64> = HashMap::new();

    {
        let mut s = state.borrow_mut();
        s.init();
        s.record_initial_state();
    }

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
                if let Some(hir) = ais.get(&fid) {
                    let ops = run_entity_ai(&state, fid, hir, opts)?;
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

    let mut s = state.borrow_mut();
    for (&fid, &ops) in &total_ops {
        let fid = i64::try_from(fid).expect("fid fits in i64");
        let ops = i64::try_from(ops).unwrap_or(i64::MAX);
        s.actions.add_ops(fid, ops);
    }
    let winner = s.compute_winner(true);
    Ok(build_outcome(
        &s.leek_snapshots,
        &s.map,
        &s.actions,
        &s.teams,
        &s.fighters,
        farmers,
        winner,
        s.duration(),
    ))
}
