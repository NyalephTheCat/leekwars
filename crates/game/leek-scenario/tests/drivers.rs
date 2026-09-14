//! The three drivers — [`run_matrix_with`], [`run_tournament_with`] and
//! [`run_random_with`] — turn one scenario plus a spec into a *set* of fights
//! and a report over them (#147). The rest of the scenario suite covers
//! parsing and world-building; this file covers the drivers' own arithmetic:
//! how many cells a spec produces, what each cell is labelled and seeded with,
//! how a cell is classified, and how the standings fall out of the cells.
//!
//! Every arena here is a small duel between a leek that shoots and one that
//! idles, and every spec is sized so a test plays a handful of turns a fight
//! and no more than about nine fights — the counts asserted below are real
//! fights, so the file has to stay cheap enough to run on every change.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use leek_scenario::{
    Bracket, CellOutcome, EntrantScope, FightResult, MatrixAxes, RandomSpec, RandomTarget,
    Scenario, Scoring, StatKind, TestReport, TournamentSpec, run_matrix_with, run_random_with,
    run_tournament_with,
};

/// One worker throughout. The drivers document that the report does not depend
/// on the worker count — `parallel_fights.rs` is the test for that — so these
/// tests fix it and are about the report alone.
const JOBS: usize = 1;

/// A scratch directory for one test, named after it. Tests run on parallel
/// threads of one process, so the name — not a clock reading — is what keeps
/// two tests out of each other's files.
fn scratch(test: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("leek-drivers-{test}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("temp dir");
    dir
}

fn write_ai(dir: &Path, name: &str, source: &str) {
    std::fs::write(dir.join(name), source).expect("write AI");
}

/// Equips its pistol and empties its TP into the nearest enemy: ~50 damage a
/// turn, so it kills a 100-life idler well inside the arena's turn limit.
const KILLER: &str = "// @version: 4\n\
     var target = getNearestEnemy();\n\
     setWeapon(getWeapons()[0]);\n\
     while (getTP() >= 3) { if (useWeapon(target) <= 0) { break; } }\n";

const IDLE: &str = "return 0;\n";

/// The two AIs every arena below refers to.
fn write_arena_ais(dir: &Path) {
    write_ai(dir, "killer.leek", KILLER);
    write_ai(dir, "idle.leek", IDLE);
}

/// A killer (team 0) facing an idler (team 1) across a small map: every fight
/// has a winner, unless a profile changes that. `plated` gives the idler more
/// life than the killer can chew through in `max_turns` — the same fight, now
/// a draw — and `paper` gives it barely any.
fn duel() -> Scenario {
    Scenario::from_toml_str(
        r#"
        seed = 11
        max_turns = 12
        [map]
        width = 5
        height = 5
        [[entities]]
        id = 1
        team = 0
        cell = 0
        life = 100
        tp = 10
        weapons = [37]
        ai = "killer.leek"
        [[entities]]
        id = 2
        team = 1
        cell = 4
        life = 100
        tp = 10
        weapons = [37]
        ai = "idle.leek"
        [[profiles.plated.entities]]
        id = 2
        life = 2000
        [[profiles.paper.entities]]
        id = 2
        life = 40
        "#,
    )
    .expect("parse duel")
}

/// The whole report as text — every field of every cell, the hero counters and
/// the standings in order. Rendering rather than comparing field by field
/// keeps a failure readable: the assert prints what actually moved.
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

fn labels(report: &TestReport) -> Vec<String> {
    report.cells.iter().map(|c| c.label.clone()).collect()
}

fn cell_count(report: &TestReport) -> u32 {
    u32::try_from(report.cells.len()).expect("a test report has few cells")
}

// ---------------------------------------------------------------------------
// Matrix
// ---------------------------------------------------------------------------

/// The sweep is a cartesian product, and each cell carries the coordinates it
/// was built from: seed outer, opponent next, profile inner.
#[test]
fn a_matrix_runs_one_cell_per_seed_opponent_and_profile() {
    let dir = scratch("matrix-axes");
    write_arena_ais(&dir);

    let axes = MatrixAxes {
        seeds: vec![3, 5],
        opponents: vec![PathBuf::from("idle.leek"), PathBuf::from("killer.leek")],
        profiles: vec!["paper".to_string()],
    };
    let report = run_matrix_with(&duel(), &dir, &axes, 0, JOBS).expect("sweep");
    let _ = std::fs::remove_dir_all(&dir);

    assert_eq!(
        report.cells.len(),
        axes.seeds.len() * axes.opponents.len() * axes.profiles.len(),
        "cells: {:?}",
        labels(&report)
    );

    // Every cell says which combination it is, and carries that seed.
    let expected: Vec<(String, u64)> = [3u64, 5]
        .into_iter()
        .flat_map(|seed| {
            ["idle.leek", "killer.leek"]
                .map(|opp| (format!("seed={seed} opp={opp} profile=paper"), seed))
        })
        .collect();
    let got: Vec<(String, u64)> = report
        .cells
        .iter()
        .map(|c| (c.label.clone(), c.seed))
        .collect();
    assert_eq!(got, expected);

    // A hero report's totals account for every fight that ran, and all four
    // ran here (both opponents compile).
    assert_eq!(report.mode, "matrix");
    assert_eq!(report.scoring, Scoring::Hero);
    assert_eq!(report.errors, 0);
    assert_eq!(
        report.wins + report.losses + report.draws,
        cell_count(&report)
    );
    assert!(report.win_rate().is_some());
}

/// The profile axis is not just a label: each cell is played on the profile it
/// names. `plated` makes the idler unkillable inside the turn limit, `paper`
/// kills it at once — the same seed, the same opponent, opposite outcomes.
#[test]
fn a_matrix_plays_each_cell_on_the_profile_it_names() {
    let dir = scratch("matrix-profiles");
    write_arena_ais(&dir);

    let axes = MatrixAxes {
        seeds: vec![3],
        opponents: vec![PathBuf::from("idle.leek")],
        profiles: vec!["plated".to_string(), "paper".to_string()],
    };
    let report = run_matrix_with(&duel(), &dir, &axes, 0, JOBS).expect("sweep");

    assert_eq!(
        labels(&report),
        [
            "seed=3 opp=idle.leek profile=plated",
            "seed=3 opp=idle.leek profile=paper",
        ]
    );
    assert_eq!(report.cells[0].result, CellOutcome::Hero(FightResult::Draw));
    assert_eq!(report.cells[1].result, CellOutcome::Hero(FightResult::Win));
    assert_eq!((report.wins, report.losses, report.draws), (1, 0, 1));

    // An unknown profile is rejected before any fight runs, so a typo costs
    // nothing instead of surfacing after half a sweep.
    let axes = MatrixAxes {
        profiles: vec!["no-such-profile".to_string()],
        ..axes
    };
    let err = run_matrix_with(&duel(), &dir, &axes, 0, JOBS).expect_err("unknown profile");
    let _ = std::fs::remove_dir_all(&dir);
    assert!(err.to_string().contains("no-such-profile"), "{err}");
}

/// `hero_team` is the whole of a matrix cell's meaning: the same fights, read
/// from the killer's side, are wins; from the idler's side, losses.
#[test]
fn a_matrix_cell_is_classified_against_the_hero_team() {
    let dir = scratch("matrix-hero");
    write_arena_ais(&dir);

    // No opponent axis, so neither run swaps an AI in: both play the duel as
    // written and can only differ in how they read it.
    let axes = MatrixAxes {
        seeds: vec![3, 5],
        opponents: Vec::new(),
        profiles: Vec::new(),
    };
    let killers_side = run_matrix_with(&duel(), &dir, &axes, 0, JOBS).expect("sweep");
    let idlers_side = run_matrix_with(&duel(), &dir, &axes, 1, JOBS).expect("sweep");
    let _ = std::fs::remove_dir_all(&dir);

    // The same two fights, decided the same way.
    let fights = |r: &TestReport| -> Vec<(u64, Option<i64>, u32)> {
        r.cells
            .iter()
            .map(|c| (c.seed, c.winner, c.turns))
            .collect()
    };
    assert_eq!(fights(&killers_side), fights(&idlers_side));
    assert!(
        killers_side.cells.iter().all(|c| c.winner == Some(0)),
        "the killer stopped winning: {}",
        render(&killers_side)
    );

    assert!(
        killers_side
            .cells
            .iter()
            .all(|c| c.result == CellOutcome::Hero(FightResult::Win))
    );
    assert_eq!(
        (
            killers_side.wins,
            killers_side.losses,
            killers_side.draws,
            killers_side.errors
        ),
        (2, 0, 0, 0)
    );
    assert!(
        killers_side
            .win_rate()
            .is_some_and(|r| (r - 100.0).abs() < f64::EPSILON)
    );

    assert!(
        idlers_side
            .cells
            .iter()
            .all(|c| c.result == CellOutcome::Hero(FightResult::Loss))
    );
    assert_eq!(
        (
            idlers_side.wins,
            idlers_side.losses,
            idlers_side.draws,
            idlers_side.errors
        ),
        (0, 2, 0, 0)
    );
    assert!(
        idlers_side
            .win_rate()
            .is_some_and(|r| r.abs() < f64::EPSILON)
    );
}

// ---------------------------------------------------------------------------
// Tournament
// ---------------------------------------------------------------------------

fn tournament(entrants: [&str; 3], bracket: Bracket) -> TournamentSpec {
    TournamentSpec {
        entrants: entrants.into_iter().map(PathBuf::from).collect(),
        bracket,
        seeds: vec![9],
        games: None,
        scope: EntrantScope::Lead,
    }
}

/// Round robin: every pair of entrants meets, each of the pairing's seeds is
/// played from both sides, and the leaderboard is the result — 3 points for a
/// pairing won, 1 for a pairing drawn, sorted by points, then wins, then
/// label.
#[test]
fn a_round_robin_plays_every_pairing_and_ranks_by_points() {
    let dir = scratch("round-robin");
    write_arena_ais(&dir);
    write_ai(&dir, "idle-a.leek", IDLE);
    write_ai(&dir, "idle-b.leek", IDLE);

    let spec = tournament(
        ["killer.leek", "idle-a.leek", "idle-b.leek"],
        Bracket::RoundRobin,
    );
    let report = run_tournament_with(&duel(), &dir, &spec, JOBS).expect("tournament");
    let _ = std::fs::remove_dir_all(&dir);

    // C(3,2) pairings, each playing every seed from both sides.
    let pairings = spec.entrants.len() * (spec.entrants.len() - 1) / 2;
    assert_eq!(
        report.cells.len(),
        pairings * spec.seeds.len() * 2,
        "cells: {:?}",
        labels(&report)
    );
    assert_eq!(
        labels(&report),
        [
            "killer vs idle-a @seed=9 sides=as-listed",
            "killer vs idle-a @seed=9 sides=swapped",
            "killer vs idle-b @seed=9 sides=as-listed",
            "killer vs idle-b @seed=9 sides=swapped",
            "idle-a vs idle-b @seed=9 sides=as-listed",
            "idle-a vs idle-b @seed=9 sides=swapped",
        ]
    );

    // The killer wins its four games from either slot; two idlers can only
    // draw. A cell names the winner rather than a side (#66).
    let won = CellOutcome::Won("killer".to_string());
    let results: Vec<&CellOutcome> = report.cells.iter().map(|c| &c.result).collect();
    assert_eq!(
        results,
        [
            &won,
            &won,
            &won,
            &won,
            &CellOutcome::Level,
            &CellOutcome::Level
        ]
    );

    // Two pairings won is 6 points; a drawn pairing is 1 for each side, so the
    // two idlers tie on points and wins and the label breaks it.
    let rows: Vec<(&str, u32, u32, u32, u32)> = report
        .standings
        .iter()
        .map(|s| (s.label.as_str(), s.wins, s.losses, s.draws, s.points))
        .collect();
    assert_eq!(
        rows,
        [
            ("killer", 2, 0, 0, 6),
            ("idle-a", 0, 1, 1, 1),
            ("idle-b", 0, 1, 1, 1),
        ]
    );
    // …and the documented order holds pairwise, not only for this podium.
    for pair in report.standings.windows(2) {
        let (hi, lo) = (&pair[0], &pair[1]);
        assert!(
            (hi.points, hi.wins) > (lo.points, lo.wins)
                || ((hi.points, hi.wins) == (lo.points, lo.wins) && hi.label < lo.label),
            "{} should not outrank {}",
            hi.label,
            lo.label
        );
    }

    // A tournament has no hero, so the hero totals stay unset.
    assert_eq!(report.mode, "tournament");
    assert_eq!(report.scoring, Scoring::Leaderboard);
    assert_eq!(
        (report.wins, report.losses, report.draws, report.errors),
        (0, 0, 0, 0)
    );
    assert!(report.win_rate().is_none());
}

/// Single elimination with an odd entrant: round 1 pairs the first two and
/// hands the third a bye, round 2 pairs the winner with it, and the bracket
/// ends with one entrant that never lost.
#[test]
fn a_single_elimination_bracket_byes_the_odd_entrant_and_leaves_one_survivor() {
    let dir = scratch("single-elim");
    write_arena_ais(&dir);
    write_ai(&dir, "idle-a.leek", IDLE);
    write_ai(&dir, "idle-b.leek", IDLE);

    let spec = tournament(
        ["idle-a.leek", "killer.leek", "idle-b.leek"],
        Bracket::SingleElim,
    );
    let report = run_tournament_with(&duel(), &dir, &spec, JOBS).expect("tournament");
    let _ = std::fs::remove_dir_all(&dir);

    // Two pairings for three entrants — one a round — is the bye: a third
    // pairing would mean the odd entrant had played in round 1.
    let games = spec.seeds.len() * 2;
    assert_eq!(
        report.cells.len(),
        2 * games,
        "cells: {:?}",
        labels(&report)
    );
    assert_eq!(
        labels(&report),
        [
            "idle-a vs killer @seed=9 sides=as-listed",
            "idle-a vs killer @seed=9 sides=swapped",
            "killer vs idle-b @seed=9 sides=as-listed",
            "killer vs idle-b @seed=9 sides=swapped",
        ]
    );
    assert!(
        report.cells[..games]
            .iter()
            .all(|c| !c.label.contains("idle-b")),
        "the odd entrant played in round 1: {:?}",
        labels(&report)
    );

    // The killer takes both matches; everyone else is out after one.
    let survivors: Vec<&str> = report
        .standings
        .iter()
        .filter(|s| s.losses == 0)
        .map(|s| s.label.as_str())
        .collect();
    assert_eq!(survivors, ["killer"]);
    let rows: Vec<(&str, u32, u32, u32, u32)> = report
        .standings
        .iter()
        .map(|s| (s.label.as_str(), s.wins, s.losses, s.draws, s.points))
        .collect();
    assert_eq!(
        rows,
        [
            ("killer", 2, 0, 0, 6),
            ("idle-a", 0, 1, 0, 0),
            ("idle-b", 0, 1, 0, 0),
        ]
    );
}

/// An entrant takes over its team's lead entity, or the whole team. The team
/// here keeps a second leek that shoots: under `Lead` it stays and its team
/// wins whichever entrant is sitting in it, under `Team` the entrant's idle AI
/// replaces it too and nobody shoots at all.
#[test]
fn entrant_scope_decides_how_much_of_a_team_an_entrant_takes_over() {
    let dir = scratch("entrant-scope");
    write_arena_ais(&dir);
    write_ai(&dir, "idle-a.leek", IDLE);
    write_ai(&dir, "idle-b.leek", IDLE);

    let squad = Scenario::from_toml_str(
        r#"
        seed = 11
        max_turns = 12
        [map]
        width = 5
        height = 5
        [[entities]]
        id = 1
        team = 0
        cell = 8
        life = 100
        tp = 10
        ai = "idle.leek"
        [[entities]]
        id = 3
        team = 0
        cell = 0
        life = 100
        tp = 10
        weapons = [37]
        ai = "killer.leek"
        [[entities]]
        id = 2
        team = 1
        cell = 4
        life = 100
        tp = 10
        ai = "idle.leek"
        "#,
    )
    .expect("parse squad");

    let mut spec = TournamentSpec {
        entrants: vec![PathBuf::from("idle-a.leek"), PathBuf::from("idle-b.leek")],
        bracket: Bracket::RoundRobin,
        seeds: vec![9],
        games: None,
        scope: EntrantScope::Lead,
    };
    let lead = run_tournament_with(&squad, &dir, &spec, JOBS).expect("tournament");
    spec.scope = EntrantScope::Team;
    let team = run_tournament_with(&squad, &dir, &spec, JOBS).expect("tournament");
    let _ = std::fs::remove_dir_all(&dir);

    // Lead: the support leek keeps shooting for whichever entrant holds team
    // 0, so the two legs are won by different entrants.
    assert_eq!(
        lead.cells
            .iter()
            .map(|c| c.result.clone())
            .collect::<Vec<_>>(),
        [
            CellOutcome::Won("idle-a".to_string()),
            CellOutcome::Won("idle-b".to_string()),
        ],
        "{}",
        render(&lead)
    );

    // Team: the entrant replaced the support too, so no one fires a shot.
    assert!(
        team.cells.iter().all(|c| c.result == CellOutcome::Level),
        "{}",
        render(&team)
    );
    assert_eq!(team.errors, 0);
}

// ---------------------------------------------------------------------------
// Random point-buy
// ---------------------------------------------------------------------------

/// One fight per run, each on its own build and its own fight seed — and the
/// whole report is a function of `RandomSpec::seed`, which is what makes a
/// build that beats the hero reproducible from the report alone.
#[test]
fn a_random_run_plays_one_fight_per_run_and_repeats_for_a_seed() {
    let dir = scratch("random");
    write_arena_ais(&dir);

    let spec = RandomSpec {
        runs: 3,
        capital: 120,
        stats: vec![StatKind::Strength, StatKind::Agility, StatKind::Wisdom],
        min_per_stat: 0,
        target: RandomTarget::Opponent,
        seed: 7,
    };
    let first = run_random_with(&duel(), &dir, &spec, 0, JOBS).expect("random");
    let again = run_random_with(&duel(), &dir, &spec, 0, JOBS).expect("random");
    let _ = std::fs::remove_dir_all(&dir);

    assert_eq!(
        first.cells.len(),
        usize::try_from(spec.runs).expect("a small run count")
    );
    assert_eq!(
        render(&first),
        render(&again),
        "same seed, different report"
    );

    // Each cell names its run and walks the fight seed on from the scenario's
    // own, so two runs never replay one fight.
    for (run, cell) in first.cells.iter().enumerate() {
        let run = u64::try_from(run).expect("a small run count");
        assert!(
            cell.label.starts_with(&format!("build#{run} ")),
            "cell {run}: {}",
            cell.label
        );
        assert_eq!(cell.seed, 11 + run, "the duel's seed is 11");
    }

    // The builds really are drawn per run: one build for all three would make
    // the report above reproducible for the wrong reason.
    let mut distinct: Vec<&str> = first.cells.iter().map(|c| c.label.as_str()).collect();
    distinct.sort_unstable();
    distinct.dedup();
    assert!(
        distinct.len() > 1,
        "every build came out the same: {distinct:?}"
    );

    assert_eq!(first.mode, "random");
    assert_eq!(first.scoring, Scoring::Hero);
    assert_eq!(first.errors, 0);
    assert_eq!(first.wins + first.losses + first.draws, cell_count(&first));
}
