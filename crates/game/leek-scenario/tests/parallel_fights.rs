//! The multi-fight drivers run on a worker pool (#134), and their reports are
//! written to disk and diffed against earlier runs — so the one thing that
//! must not change with the worker count is the report.
//!
//! Every test here compares a **whole** report (each cell's label, seed,
//! winner, turn count, outcome, AI errors and failure text, the hero counters,
//! and the standings in order) between `jobs = 1` and `jobs = 8`. A merge by
//! completion order, a build leaking between cells, or an RNG shared across
//! threads each show up as a difference here.
//!
//! These arenas fight for real — a leek that shoots against one that idles.
//! An all-idle arena would draw every game and pass even if the fight RNG
//! *were* shared between threads.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use leek_scenario::{
    Bracket, CellOutcome, EntrantScope, MatrixAxes, RandomSpec, RandomTarget, Scenario, StatKind,
    TestReport, TournamentSpec, run_matrix_with, run_random_with, run_tournament_with,
};

/// Serial and "as parallel as the default cap allows". `1` is not a separate
/// code path in the driver, so this compares the pool against itself rather
/// than against a serial fallback that could drift.
const SERIAL: usize = 1;
const PARALLEL: usize = 8;

/// A scratch directory for one test, named after it. Tests run on parallel
/// threads of one process, so the name — not a clock reading, which can repeat
/// on a coarse-grained one — is what keeps two tests out of each other's
/// files.
fn scratch(test: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("leek-parallel-{test}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("temp dir");
    dir
}

fn write_ai(dir: &Path, name: &str, source: &str) {
    std::fs::write(dir.join(name), source).expect("write AI");
}

/// Equips and empties its TP into the nearest enemy.
const SHOOTER: &str = "// @version: 4\n\
     var target = getNearestEnemy();\n\
     setWeapon(getWeapons()[0]);\n\
     while (getTP() >= 3) { if (useWeapon(target) <= 0) { break; } }\n";

/// Shoots a random number of times, so the fight's length depends on the AI's
/// own RNG and not only on the combat seed.
const GAMBLER: &str = "// @version: 4\n\
     var target = getNearestEnemy();\n\
     setWeapon(getWeapons()[0]);\n\
     var shots = randInt(1, 3);\n\
     for (var i = 0; i < shots; i++) { if (useWeapon(target) <= 0) { break; } }\n";

const IDLE: &str = "return 0;\n";

/// Two armed leeks facing each other across a small map, with enough life to
/// take several turns to kill: turn counts and winners then actually vary with
/// the seed, so a report has something to differ about.
fn duel() -> Scenario {
    Scenario::from_toml_str(
        r#"
        max_turns = 24
        [map]
        width = 5
        height = 5
        [[entities]]
        id = 1
        team = 0
        cell = 0
        life = 150
        tp = 12
        weapons = [37]
        ai = "shooter.leek"
        [[entities]]
        id = 2
        team = 1
        cell = 4
        life = 150
        tp = 12
        weapons = [37]
        "#,
    )
    .expect("parse duel")
}

/// The whole report as text: every field of every cell, the hero counters and
/// the standings in order. Rendering rather than comparing field by field
/// keeps a failure readable — the assert prints what actually moved.
fn render(report: &TestReport) -> String {
    let mut out = String::new();
    let _ = writeln!(
        out,
        "mode={} scoring={:?} wins={} losses={} draws={} errors={}",
        report.mode, report.scoring, report.wins, report.losses, report.draws, report.errors
    );
    for (i, c) in report.cells.iter().enumerate() {
        let _ = writeln!(
            out,
            "cell[{i}] label={:?} seed={} winner={:?} turns={} result={:?} ai_errors={:?} \
             failure={:?}",
            c.label, c.seed, c.winner, c.turns, c.result, c.ai_errors, c.failure
        );
    }
    for (i, s) in report.standings.iter().enumerate() {
        let _ = writeln!(
            out,
            "standing[{i}] {} wins={} losses={} draws={} points={}",
            s.label, s.wins, s.losses, s.draws, s.points
        );
    }
    out
}

fn matrix_axes(seeds: u64) -> MatrixAxes {
    MatrixAxes {
        seeds: (1..=seeds).collect(),
        opponents: vec![PathBuf::from("idle.leek"), PathBuf::from("shooter.leek")],
        profiles: Vec::new(),
    }
}

#[test]
fn the_worker_count_does_not_change_a_matrix_report() {
    let dir = scratch("matrix");
    write_ai(&dir, "shooter.leek", SHOOTER);
    write_ai(&dir, "idle.leek", IDLE);

    let axes = matrix_axes(12);
    let serial = run_matrix_with(&duel(), &dir, &axes, 0, SERIAL).expect("sweep");
    let parallel = run_matrix_with(&duel(), &dir, &axes, 0, PARALLEL).expect("sweep");
    let again = run_matrix_with(&duel(), &dir, &axes, 0, PARALLEL).expect("sweep");
    let _ = std::fs::remove_dir_all(&dir);

    assert_eq!(serial.cells.len(), 24, "12 seeds × 2 opponents");
    assert_eq!(render(&serial), render(&parallel));
    assert_eq!(
        render(&parallel),
        render(&again),
        "run to run, at 8 workers"
    );

    // Not every cell may be a draw, or the comparison above would hold even if
    // the fights were sharing state.
    assert!(
        serial.wins + serial.losses > 0,
        "the arena stopped fighting: {}",
        render(&serial)
    );
}

/// The cell order is a persisted, user-visible observable (`miku fight
/// --report` writes `cells` in vector order), so it is asserted directly and
/// not only against another run of itself: a merge by completion order would
/// satisfy "1 == 8" only by accident.
#[test]
fn matrix_cells_stay_in_seed_then_opponent_order() {
    let dir = scratch("matrix-order");
    write_ai(&dir, "shooter.leek", SHOOTER);
    write_ai(&dir, "idle.leek", IDLE);

    let axes = matrix_axes(6);
    let report = run_matrix_with(&duel(), &dir, &axes, 0, PARALLEL).expect("sweep");
    let _ = std::fs::remove_dir_all(&dir);

    let expected: Vec<String> = (1..=6)
        .flat_map(|seed| {
            ["idle.leek", "shooter.leek"]
                .map(|opp| format!("seed={seed} opp={opp} profile=-"))
                .into_iter()
        })
        .collect();
    let got: Vec<String> = report.cells.iter().map(|c| c.label.clone()).collect();
    assert_eq!(got, expected);
    assert_eq!(
        report.cells.iter().map(|c| c.seed).collect::<Vec<_>>(),
        (1..=6).flat_map(|s| [s, s]).collect::<Vec<u64>>()
    );
}

#[test]
fn the_worker_count_does_not_change_a_round_robin_report() {
    let dir = scratch("round-robin");
    write_ai(&dir, "shooter.leek", SHOOTER);
    write_ai(&dir, "gambler.leek", GAMBLER);
    write_ai(&dir, "idle.leek", IDLE);
    write_ai(&dir, "waiter.leek", "// @version: 4\nreturn 0;\n");

    let spec = TournamentSpec {
        entrants: ["shooter.leek", "gambler.leek", "idle.leek", "waiter.leek"]
            .into_iter()
            .map(PathBuf::from)
            .collect(),
        bracket: Bracket::RoundRobin,
        seeds: Vec::new(),
        games: Some(3),
        scope: EntrantScope::Lead,
    };
    let serial = run_tournament_with(&duel(), &dir, &spec, SERIAL).expect("tournament");
    let parallel = run_tournament_with(&duel(), &dir, &spec, PARALLEL).expect("tournament");
    let _ = std::fs::remove_dir_all(&dir);

    // 6 pairings × 3 seeds × 2 legs.
    assert_eq!(serial.cells.len(), 36);
    assert_eq!(serial.standings.len(), 4);
    assert_eq!(render(&serial), render(&parallel));
}

/// Single elimination is the bracket a wrongly flattened pool would break:
/// round 2's pairings are round 1's winners, so the rounds cannot share a
/// pass. If they did, round 2 would be played against stale entrants and the
/// standings would move.
#[test]
fn the_worker_count_does_not_change_a_single_elim_report() {
    let dir = scratch("single-elim");
    write_ai(&dir, "shooter.leek", SHOOTER);
    write_ai(&dir, "gambler.leek", GAMBLER);
    write_ai(&dir, "idle.leek", IDLE);
    write_ai(&dir, "waiter.leek", "// @version: 4\nreturn 0;\n");
    write_ai(
        &dir,
        "sleeper.leek",
        "// @version: 4\nvar x = 1;\nreturn x;\n",
    );

    // Five entrants, so round 1 hands out a bye — the one pairing slot that
    // plays no games and must not consume a result group.
    let spec = TournamentSpec {
        entrants: [
            "shooter.leek",
            "gambler.leek",
            "idle.leek",
            "waiter.leek",
            "sleeper.leek",
        ]
        .into_iter()
        .map(PathBuf::from)
        .collect(),
        bracket: Bracket::SingleElim,
        seeds: vec![7, 11],
        games: None,
        scope: EntrantScope::Lead,
    };
    let serial = run_tournament_with(&duel(), &dir, &spec, SERIAL).expect("tournament");
    let parallel = run_tournament_with(&duel(), &dir, &spec, PARALLEL).expect("tournament");
    let _ = std::fs::remove_dir_all(&dir);

    // Round 1: two pairings + a bye. Round 2: two survivors + the bye is three
    // entrants, so one pairing + a bye. Round 3: one pairing. Four pairings,
    // each 2 seeds × 2 legs.
    assert_eq!(serial.cells.len(), 16);
    assert_eq!(render(&serial), render(&parallel));
}

#[test]
fn the_worker_count_does_not_change_a_random_report() {
    let dir = scratch("random");
    write_ai(&dir, "shooter.leek", SHOOTER);

    let spec = RandomSpec {
        runs: 24,
        capital: 300,
        stats: vec![StatKind::Strength, StatKind::Agility, StatKind::Wisdom],
        min_per_stat: 0,
        target: RandomTarget::Opponent,
        seed: 42,
    };
    let serial = run_random_with(&duel(), &dir, &spec, 0, SERIAL).expect("random");
    let parallel = run_random_with(&duel(), &dir, &spec, 0, PARALLEL).expect("random");
    let _ = std::fs::remove_dir_all(&dir);

    assert_eq!(serial.cells.len(), 24);
    assert_eq!(render(&serial), render(&parallel));

    // Each cell's build belongs to it alone: a build leaking from one worker's
    // scenario clone into another's would show up as two identical labels.
    let mut labels: Vec<&str> = serial.cells.iter().map(|c| c.label.as_str()).collect();
    labels.sort_unstable();
    let distinct = {
        let mut l = labels.clone();
        l.dedup();
        l.len()
    };
    assert!(
        distinct > 1,
        "every random build came out the same: {labels:?}"
    );
}

/// The direct guard on the runtime RNG being per-thread: `randInt` decides how
/// many times the gambler fires, so a generator shared between workers would
/// change turn counts and winners with the worker count.
#[test]
fn an_ai_drawing_random_numbers_is_reproducible_across_workers() {
    let dir = scratch("randint");
    write_ai(&dir, "shooter.leek", SHOOTER);
    write_ai(&dir, "gambler.leek", GAMBLER);

    let axes = MatrixAxes {
        seeds: (1..=16).collect(),
        opponents: vec![PathBuf::from("gambler.leek")],
        profiles: Vec::new(),
    };
    let serial = run_matrix_with(&duel(), &dir, &axes, 0, SERIAL).expect("sweep");
    let parallel = run_matrix_with(&duel(), &dir, &axes, 0, PARALLEL).expect("sweep");
    let _ = std::fs::remove_dir_all(&dir);

    assert_eq!(serial.cells.len(), 16);
    assert!(
        serial.cells.iter().all(|c| c.ai_errors.is_empty()),
        "the gambler AI errored, so it drew no random numbers: {}",
        render(&serial)
    );
    assert_eq!(render(&serial), render(&parallel));
}

/// Regression (#67 on the parallel path): a cell whose fight can't even be set
/// up is an `Error` cell **at its own index**, not wherever the failing worker
/// happened to finish. It fails fast, so on a completion-order merge it would
/// land first — which is exactly where it belongs here, so the second failing
/// cell (the slow one, last) is what pins the order.
#[test]
fn a_failing_cell_keeps_its_own_index() {
    let dir = scratch("failing-cell");
    write_ai(&dir, "shooter.leek", SHOOTER);
    write_ai(&dir, "idle.leek", IDLE);

    let axes = MatrixAxes {
        seeds: vec![1, 2, 3, 4],
        opponents: vec![
            PathBuf::from("missing.leek"),
            PathBuf::from("idle.leek"),
            PathBuf::from("also-missing.leek"),
        ],
        profiles: Vec::new(),
    };
    let report = run_matrix_with(&duel(), &dir, &axes, 0, PARALLEL).expect("the sweep succeeds");
    let _ = std::fs::remove_dir_all(&dir);

    assert_eq!(report.cells.len(), 12);
    assert_eq!(report.errors, 8, "two of the three opponents never compile");

    // A cell that fails finishes in microseconds while a real fight takes
    // milliseconds, so on a completion-order merge the eight failures would
    // bunch up at the front. Pinning the labels catches that directly rather
    // than hoping the interleaving happens to expose it.
    let expected: Vec<String> = [1u64, 2, 3, 4]
        .into_iter()
        .flat_map(|seed| {
            ["missing.leek", "idle.leek", "also-missing.leek"]
                .map(|opp| format!("seed={seed} opp={opp} profile=-"))
        })
        .collect();
    assert_eq!(
        report
            .cells
            .iter()
            .map(|c| c.label.clone())
            .collect::<Vec<_>>(),
        expected
    );

    for seed in 0..4 {
        let base = seed * 3;
        assert_eq!(report.cells[base].result, CellOutcome::Error);
        assert!(
            report.cells[base]
                .failure
                .as_deref()
                .unwrap_or_default()
                .contains("missing.leek")
        );
        assert_ne!(
            report.cells[base + 1].result,
            CellOutcome::Error,
            "the compilable opponent sits between the two broken ones"
        );
        assert_eq!(report.cells[base + 2].result, CellOutcome::Error);
    }
}
