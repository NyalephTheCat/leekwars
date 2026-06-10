//! Conformance: replay the oracle corpus through the official [`State`] and
//! compare the full Outcome JSON against the Java generator's goldens
//! (`tools/fight-harness/goldens/`).
//!
//! The corpus AIs are tiny (idle / walker / chase), so instead of compiling
//! them through the native backend these tests script their exact builtin
//! call sequences through [`call_official_builtin`] — which is the same
//! dispatch surface the real runner uses. Everything else (map generation,
//! start order, turn loop, pathfinding, action log, outcome assembly) is the
//! production code path.
//!
//! `fight.ops` and `execution_time` are runtime-measurement fields and are
//! excluded from the comparison (the harness diff tool ignores them too).

use std::path::PathBuf;

use leek_game_runtime::official_builtins::call_official_builtin;
use leek_game_runtime::outcome::build_outcome;
use leek_game_runtime::state::{
    BeginTurn, Fighter, MAX_TURNS, STAT_AGILITY, STAT_FREQUENCY, STAT_LIFE, STAT_MP,
    STAT_RESISTANCE, STAT_STRENGTH, STAT_TP, STAT_WISDOM, State, Stats, WeaponSpec,
};
use leek_runtime::Value;
use serde_json::json;

/// The corpus AIs (`tools/fight-harness/examples/*.leek`), as the builtin
/// call sequences their sources compile to.
#[derive(Clone, Copy)]
enum Ai {
    /// `idle.leek`: `return;`
    Idle,
    /// `walker.leek`: `var e = getNearestEnemy(); moveToward(e);`
    Walker,
    /// `chase.leek`: `var e = getNearestEnemy(); setWeapon(WEAPON_PISTOL);
    /// moveToward(e, 10); useWeapon(e); useWeapon(e);`
    Chase,
}

fn run_ai(state: &mut State, fid: usize, ai: Ai) {
    let call = |state: &mut State, name: &str, args: &[Value]| -> Value {
        call_official_builtin(state, fid, name, args)
    };
    match ai {
        Ai::Idle => {}
        Ai::Walker => {
            let e = call(state, "getNearestEnemy", &[]);
            call(state, "moveToward", &[e]);
        }
        Ai::Chase => {
            let e = call(state, "getNearestEnemy", &[]);
            call(state, "setWeapon", &[Value::Int(37)]);
            call(state, "moveToward", &[e.clone(), Value::Int(10)]);
            call(state, "useWeapon", std::slice::from_ref(&e));
            call(state, "useWeapon", &[e]);
        }
    }
}

/// `Harness.defaultLeek` — the stock fight-harness leek with the synthetic
/// pistol (id 37) in its inventory.
fn harness_leek(id: i64, name: &str) -> Fighter {
    let mut stats = Stats::default();
    stats.set(STAT_LIFE, 500);
    stats.set(STAT_TP, 6);
    stats.set(STAT_MP, 7);
    stats.set(STAT_STRENGTH, 100);
    stats.set(STAT_AGILITY, 100);
    stats.set(STAT_FREQUENCY, 10);
    stats.set(STAT_WISDOM, 50);
    stats.set(STAT_RESISTANCE, 10);
    let mut f = Fighter::new(0, id, name.to_string(), 0, stats);
    f.level = 10;
    f.weapons = vec![37];
    f
}

/// `Harness.registerPistol` — synthetic pistol 37. `max_uses: 0` is verbatim
/// from the harness (it makes every `useWeapon` return `USE_MAX_USES`, which
/// is why the goldens contain no weapon-fire actions).
fn harness_pistol() -> WeaponSpec {
    WeaponSpec {
        id: 37,
        cost: 3,
        min_range: 1,
        max_range: 7,
        launch_type: 1,
        needs_los: true,
        max_uses: 0,
    }
}

/// Mirror of `Harness.main` + `Fight.startFight(true)`: build the 1v1, run
/// the official loop with the scripted AIs, assemble the Outcome JSON.
fn run_fight(seed: i64, ais: [Ai; 2]) -> serde_json::Value {
    let mut state = State::new(seed);
    state.add_entity(0, harness_leek(1, "AI_1"));
    state.add_entity(1, harness_leek(2, "AI_2"));
    state.weapon_specs.insert(37, harness_pistol());

    state.init();
    state.record_initial_state();

    while state.order.turn() <= MAX_TURNS && state.running {
        match state.begin_turn() {
            BeginTurn::Act(fid) => {
                let ai = ais[state.fighters[fid].team];
                run_ai(&mut state, fid, ai);
                state.end_entity_turn(fid);
                state.end_turn();
            }
            BeginTurn::Skip => state.end_turn(),
            BeginTurn::NoCurrent => {}
        }
        if state.order.current().is_none() {
            state.running = false;
            break;
        }
    }

    let winner = state.compute_winner(true);
    build_outcome(
        &state.leek_snapshots,
        &state.map,
        &state.actions,
        &state.teams,
        &state.fighters,
        &[0],
        winner,
        state.duration(),
    )
}

/// Compare against a golden, ignoring the runtime-measurement fields.
fn assert_matches_golden(mut ours: serde_json::Value, golden_name: &str) {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../../tools/fight-harness/goldens")
        .join(format!("{golden_name}.json"));
    let data = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("golden {} unreadable: {e}", path.display()));
    let mut golden: serde_json::Value = serde_json::from_str(&data).expect("golden parses");

    for doc in [&mut ours, &mut golden] {
        doc["execution_time"] = json!(0);
        doc["fight"]["ops"] = json!({});
    }

    if ours != golden {
        // Find the first divergent action for a readable failure.
        let oa = ours["fight"]["actions"].as_array().unwrap();
        let ga = golden["fight"]["actions"].as_array().unwrap();
        for (i, (o, g)) in oa.iter().zip(ga.iter()).enumerate() {
            assert_eq!(o, g, "{golden_name}: first divergent action at index {i}");
        }
        assert_eq!(
            oa.len(),
            ga.len(),
            "{golden_name}: action count (common prefix matches)"
        );
        assert_eq!(ours, golden, "{golden_name}: non-action field diverges");
    }
}

#[test]
fn idle_vs_idle_s42() {
    assert_matches_golden(run_fight(42, [Ai::Idle, Ai::Idle]), "idle_vs_idle_s42");
}

#[test]
fn walker_vs_walker_s7() {
    assert_matches_golden(
        run_fight(7, [Ai::Walker, Ai::Walker]),
        "walker_vs_walker_s7",
    );
}

#[test]
fn walker_vs_walker_s42() {
    assert_matches_golden(
        run_fight(42, [Ai::Walker, Ai::Walker]),
        "walker_vs_walker_s42",
    );
}

#[test]
fn chase_vs_chase_s1() {
    assert_matches_golden(run_fight(1, [Ai::Chase, Ai::Chase]), "chase_vs_chase_s1");
}

#[test]
fn chase_vs_chase_s7() {
    assert_matches_golden(run_fight(7, [Ai::Chase, Ai::Chase]), "chase_vs_chase_s7");
}

#[test]
fn chase_vs_chase_s42() {
    assert_matches_golden(run_fight(42, [Ai::Chase, Ai::Chase]), "chase_vs_chase_s42");
}

#[test]
fn chase_vs_chase_s99() {
    assert_matches_golden(run_fight(99, [Ai::Chase, Ai::Chase]), "chase_vs_chase_s99");
}

#[test]
fn chase_vs_chase_s12345() {
    assert_matches_golden(
        run_fight(12345, [Ai::Chase, Ai::Chase]),
        "chase_vs_chase_s12345",
    );
}

#[test]
fn chase_vs_idle_s42() {
    assert_matches_golden(run_fight(42, [Ai::Chase, Ai::Idle]), "chase_vs_idle_s42");
}

#[test]
fn chase_vs_walker_s99() {
    assert_matches_golden(
        run_fight(99, [Ai::Chase, Ai::Walker]),
        "chase_vs_walker_s99",
    );
}
