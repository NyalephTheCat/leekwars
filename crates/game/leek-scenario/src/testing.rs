//! Testing modes — drive an AI against many settings on top of one single-fight
//! primitive ([`play_one`]) and one compiled-AI cache.
//!
//! - [`run_matrix`] — a cartesian sweep over seeds × opponents × profiles.
//! - [`run_tournament`] — round-robin or single-elimination among N entrants,
//!   producing a leaderboard.
//! - [`run_random`] — randomized point-buy build fuzzing (see [`build_gen`]).
//!
//! All return a [`TestReport`]. [`run_matrix`] and [`run_random`] classify each
//! fight relative to a *hero team* (the AI under test); a tournament has no
//! hero, so each of its cells names the entrant that won that game and the
//! leaderboard — not the hero totals, which stay unset — is the result (see
//! [`Scoring`]). One bad cell never sinks the run: an AI error inside a fight
//! is contained by the generator and listed on the cell
//! ([`CellResult::ai_errors`]), and a fight that can't even be set up (an AI
//! that doesn't compile, say) becomes a [`CellOutcome::Error`] cell.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Result, bail};
use leek_generator::Outcome;
use leek_hir::HirFile;

use crate::build_gen;
use crate::load::{build_fight_with_cache, compile_ai};
use crate::schema::{EntrantScope, RandomSpec, RandomTarget, Scenario};

/// Outcome of a single fight relative to the hero team.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FightResult {
    Win,
    Loss,
    Draw,
}

/// What one cell of a report says happened.
///
/// A hero mode ([`run_matrix`], [`run_random`]) classifies every fight against
/// the AI under test. A tournament has no hero: a game is named by its winner
/// instead, because a win or a loss there could only be read against the side
/// one of the entrants happened to be assigned (#66).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CellOutcome {
    /// A hero-mode fight, classified against the hero team.
    Hero(FightResult),
    /// A tournament game won by the named entrant.
    Won(String),
    /// A tournament game that ended level.
    Level,
    /// The fight couldn't be run; [`CellResult::failure`] says why.
    Error,
}

/// How a report's cells are scored, and so what its totals mean.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scoring {
    /// Every fight is classified against the hero team: the report's
    /// `wins`/`losses`/`draws` and [`TestReport::win_rate`] are the result.
    Hero,
    /// There is no hero: every cell names the entrant that won its game and
    /// [`TestReport::standings`] is the result. The hero totals stay zero and
    /// [`TestReport::win_rate`] is `None`.
    Leaderboard,
}

/// One fight in a report.
#[derive(Debug, Clone)]
pub struct CellResult {
    pub label: String,
    pub seed: u64,
    pub winner: Option<i64>,
    pub turns: u32,
    pub result: CellOutcome,
    /// AI turns that ended in an error during the fight (each rendered as
    /// `turn N: entity E: ERROR`); the fight still played out.
    pub ai_errors: Vec<String>,
    /// Why the fight couldn't be run, for a [`CellOutcome::Error`] cell.
    pub failure: Option<String>,
}

/// A competitor's aggregate record (tournament leaderboard row).
#[derive(Debug, Clone)]
pub struct Standing {
    pub label: String,
    pub wins: u32,
    pub losses: u32,
    pub draws: u32,
    pub points: u32,
}

/// The result of a testing run.
#[derive(Debug, Clone)]
pub struct TestReport {
    pub mode: &'static str,
    /// What the cells and the totals below mean.
    pub scoring: Scoring,
    pub cells: Vec<CellResult>,
    pub standings: Vec<Standing>,
    /// Hero totals: the fights the hero team won, lost and drew. All three
    /// stay zero under [`Scoring::Leaderboard`], which has no hero to count
    /// them for — read `standings` instead.
    pub wins: u32,
    pub losses: u32,
    pub draws: u32,
    /// Cells whose fight couldn't be run ([`CellOutcome::Error`]), in every
    /// mode.
    pub errors: u32,
}

impl TestReport {
    fn new(mode: &'static str, scoring: Scoring) -> Self {
        Self {
            mode,
            scoring,
            cells: Vec::new(),
            standings: Vec::new(),
            wins: 0,
            losses: 0,
            draws: 0,
            errors: 0,
        }
    }

    /// Record a hero-mode fight, classified against `hero_team`.
    fn record_hero(&mut self, label: String, seed: u64, outcome: &Outcome, hero_team: i64) {
        let result = classify(outcome.winner_team, hero_team);
        match result {
            FightResult::Win => self.wins += 1,
            FightResult::Loss => self.losses += 1,
            FightResult::Draw => self.draws += 1,
        }
        self.push_cell(label, seed, outcome, CellOutcome::Hero(result));
    }

    /// Record a tournament game won by `winner` (`None` when it ended level).
    /// The hero totals are left alone: the standings are the result.
    fn record_game(&mut self, label: String, seed: u64, outcome: &Outcome, winner: Option<String>) {
        let result = winner.map_or(CellOutcome::Level, CellOutcome::Won);
        self.push_cell(label, seed, outcome, result);
    }

    fn push_cell(&mut self, label: String, seed: u64, outcome: &Outcome, result: CellOutcome) {
        self.cells.push(CellResult {
            label,
            seed,
            winner: outcome.winner_team,
            turns: outcome.turns,
            result,
            ai_errors: outcome.errors.iter().map(ToString::to_string).collect(),
            failure: None,
        });
    }

    /// Record a cell whose fight couldn't be run.
    fn record_failure(&mut self, label: String, seed: u64, err: &anyhow::Error) {
        self.errors += 1;
        self.cells.push(CellResult {
            label,
            seed,
            winner: None,
            turns: 0,
            result: CellOutcome::Error,
            ai_errors: Vec::new(),
            failure: Some(format!("{err:#}")),
        });
    }

    /// Win rate over the fights that ran (wins, losses and draws; error cells
    /// don't count), as a percentage. `None` when there is nothing to compute
    /// it from: a [`Scoring::Leaderboard`] report has no hero, and a report in
    /// which no fight ran has no fights.
    #[must_use]
    pub fn win_rate(&self) -> Option<f64> {
        if self.scoring != Scoring::Hero {
            return None;
        }
        let total = self.wins + self.losses + self.draws;
        (total > 0).then(|| f64::from(self.wins) * 100.0 / f64::from(total))
    }
}

fn classify(winner: Option<i64>, hero_team: i64) -> FightResult {
    match winner {
        Some(t) if t == hero_team => FightResult::Win,
        Some(_) => FightResult::Loss,
        None => FightResult::Draw,
    }
}

/// The shared unit of work: build the fight from a (cache-backed) scenario and
/// run it to an [`Outcome`]. Compiles nothing when every AI is in `cache`.
///
/// # Errors
/// Only when the fight can't be built (e.g. an AI missing from `cache` fails
/// to compile); AI errors during the fight are part of the [`Outcome`].
fn play_one(
    scn: &Scenario,
    base_dir: &Path,
    cache: &HashMap<PathBuf, Arc<HirFile>>,
) -> Result<Outcome> {
    let lf = build_fight_with_cache(scn, base_dir, Some(cache))?;
    let fight = leek_generator::shared(lf.fight);
    Ok(leek_generator::run_fight_release(
        &fight,
        &lf.ais,
        lf.max_turns,
        lf.version,
        lf.strict,
        lf.max_ops_per_turn,
    ))
}

/// Compile every distinct AI path used by `base` plus the `extra` opponents
/// once, keyed by the joined path (matching [`build_fight_with_cache`]).
///
/// An AI that fails to compile is left out rather than failing the run: the
/// cells that use it retry the compile and report the error as their own
/// [`CellOutcome::Error`], while the other cells still play.
fn precompile(
    base: &Scenario,
    base_dir: &Path,
    extra: &[PathBuf],
) -> HashMap<PathBuf, Arc<HirFile>> {
    let version = base.version.unwrap_or(4);
    let strict = base.strict.unwrap_or(false);

    let mut cache: HashMap<PathBuf, Arc<HirFile>> = HashMap::new();
    let paths = base
        .entities
        .iter()
        .filter_map(|e| e.ai.clone())
        .chain(extra.iter().cloned());
    for path in paths {
        let joined = base_dir.join(&path);
        if let std::collections::hash_map::Entry::Vacant(slot) = cache.entry(joined.clone())
            && let Ok(hir) = compile_ai(&joined, version, strict)
        {
            slot.insert(hir);
        }
    }
    cache
}

/// Return the team ids present in `scn`, in first-seen order.
fn team_ids(scn: &Scenario) -> Vec<i64> {
    let mut teams: Vec<i64> = Vec::new();
    for e in &scn.entities {
        let team = e.team.unwrap_or(0);
        if !teams.contains(&team) {
            teams.push(team);
        }
    }
    teams
}

/// Point the entities of `team` selected by `scope` at a different AI file.
///
/// [`EntrantScope::Lead`] touches only the first-listed entity of the team —
/// the rest keep the AI the scenario gave them, so a team scenario still runs
/// its own supporting AIs; [`EntrantScope::Team`] hands the whole team over.
fn set_team_ai(scn: &mut Scenario, team: i64, ai: &Path, scope: EntrantScope) {
    let mut members = scn
        .entities
        .iter_mut()
        .filter(|e| e.team.unwrap_or(0) == team);
    match scope {
        EntrantScope::Lead => {
            if let Some(e) = members.next() {
                e.ai = Some(ai.to_path_buf());
            }
        }
        EntrantScope::Team => {
            for e in members {
                e.ai = Some(ai.to_path_buf());
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Matrix
// ---------------------------------------------------------------------------

/// The sweep axes. Empty axes collapse to a single implicit value (the base
/// seed, no opponent swap, no profile).
#[derive(Debug, Clone, Default)]
pub struct MatrixAxes {
    pub seeds: Vec<u64>,
    pub opponents: Vec<PathBuf>,
    pub profiles: Vec<String>,
}

/// Run a cartesian sweep of seeds × opponents × profiles, classifying each
/// fight relative to `hero_team`. A cell whose fight can't be run is reported
/// as [`CellOutcome::Error`] and the sweep continues.
///
/// # Errors
/// A profile name not found in the scenario (checked before any fight runs).
pub fn run_matrix(
    base: &Scenario,
    base_dir: &Path,
    axes: &MatrixAxes,
    hero_team: i64,
) -> Result<TestReport> {
    for name in &axes.profiles {
        base.clone().apply_profile(name)?;
    }
    let cache = precompile(base, base_dir, &axes.opponents);
    let opp_team = team_ids(base).into_iter().find(|&t| t != hero_team);

    let seeds = if axes.seeds.is_empty() {
        vec![base.seed.unwrap_or(1)]
    } else {
        axes.seeds.clone()
    };
    let opponents: Vec<Option<&PathBuf>> = if axes.opponents.is_empty() {
        vec![None]
    } else {
        axes.opponents.iter().map(Some).collect()
    };
    let profiles: Vec<Option<&String>> = if axes.profiles.is_empty() {
        vec![None]
    } else {
        axes.profiles.iter().map(Some).collect()
    };

    let mut report = TestReport::new("matrix", Scoring::Hero);
    for &seed in &seeds {
        for opp in &opponents {
            for prof in &profiles {
                let mut scn = base.clone();
                if let Some(name) = prof {
                    scn.apply_profile(name)?;
                }
                scn.seed = Some(seed);
                if let (Some(opp_path), Some(team)) = (opp, opp_team) {
                    set_team_ai(&mut scn, team, opp_path, EntrantScope::Lead);
                }
                let label = format!(
                    "seed={seed} opp={} profile={}",
                    opp.map_or("-", |p| p.to_str().unwrap_or("?")),
                    prof.map_or("-", String::as_str),
                );
                match play_one(&scn, base_dir, &cache) {
                    Ok(outcome) => report.record_hero(label, seed, &outcome, hero_team),
                    Err(e) => report.record_failure(label, seed, &e),
                }
            }
        }
    }
    Ok(report)
}

// ---------------------------------------------------------------------------
// Tournament
// ---------------------------------------------------------------------------

/// Tournament configuration. Each entrant is an AI file; it takes over a team
/// of the base scenario — its lead entity only, or all of it, per
/// [`TournamentSpec::scope`] — and faces the others.
#[derive(Debug, Clone)]
pub struct TournamentSpec {
    pub entrants: Vec<PathBuf>,
    pub bracket: crate::schema::Bracket,
    /// Seeds played per pairing. Each seed is played twice, once with the
    /// entrants on either side, so a pairing is `2 × seeds` games; the side
    /// winning the most of them takes the match.
    pub seeds: Vec<u64>,
    /// How much of a team an entrant takes over: only the lead entity
    /// (default) or every member.
    pub scope: EntrantScope,
}

/// Run a tournament among `entrants`, returning a leaderboard in `standings`.
/// A game that can't be run is reported as a [`CellOutcome::Error`] cell and
/// counts for neither side.
///
/// There is no hero here, so the report is scored as a [`Scoring::Leaderboard`]
/// one: each cell names the entrant that won that game ([`CellOutcome::Won`],
/// or [`CellOutcome::Level`] for a draw), the hero win/loss/draw totals stay
/// unset — they could only count "whoever this pairing listed first" (#66) —
/// and the standings are the result.
///
/// Every seed of a pairing is played twice, with the entrants swapping team
/// slots between the two legs, so no entrant collects whatever edge a slot
/// carries (#39). A single-elimination match that ends level is still scored
/// as a draw for both entrants; one of them has to advance, and
/// [`tie_break_favors_a`] picks which.
///
/// # Errors
/// Needs at least two entrants and two teams in the base scenario.
pub fn run_tournament(
    base: &Scenario,
    base_dir: &Path,
    spec: &TournamentSpec,
) -> Result<TestReport> {
    if spec.entrants.len() < 2 {
        bail!("a tournament needs at least two entrants");
    }
    let teams = team_ids(base);
    let (Some(&team_a), Some(&team_b)) = (teams.first(), teams.get(1)) else {
        bail!("the base scenario needs two teams for a tournament");
    };

    let cache = precompile(base, base_dir, &spec.entrants);
    let seeds = if spec.seeds.is_empty() {
        vec![base.seed.unwrap_or(1)]
    } else {
        spec.seeds.clone()
    };

    let mut report = TestReport::new("tournament", Scoring::Leaderboard);
    let mut standings: HashMap<String, Standing> = HashMap::new();
    for e in &spec.entrants {
        standings.insert(
            label_of(e),
            Standing {
                label: label_of(e),
                wins: 0,
                losses: 0,
                draws: 0,
                points: 0,
            },
        );
    }

    // Play A vs B over the seeds; returns (a_wins, b_wins, draws). Each seed
    // is played twice, with the entrants swapped between the two team slots:
    // the slots aren't interchangeable (starting cells, and the start order is
    // drawn per team), so playing one leg would hand whoever sits in `team_a`
    // the same edge in every pairing (#39).
    let mut play_match = |a: &Path, b: &Path| -> (u32, u32, u32) {
        let (mut aw, mut bw, mut dw) = (0, 0, 0);
        for &seed in &seeds {
            for swapped in [false, true] {
                let (a_team, b_team) = if swapped {
                    (team_b, team_a)
                } else {
                    (team_a, team_b)
                };
                let mut scn = base.clone();
                scn.seed = Some(seed);
                set_team_ai(&mut scn, a_team, a, spec.scope);
                set_team_ai(&mut scn, b_team, b, spec.scope);
                let sides = if swapped { "swapped" } else { "as-listed" };
                let label = format!(
                    "{} vs {} @seed={seed} sides={sides}",
                    label_of(a),
                    label_of(b)
                );
                let outcome = match play_one(&scn, base_dir, &cache) {
                    Ok(outcome) => outcome,
                    Err(e) => {
                        report.record_failure(label, seed, &e);
                        continue;
                    }
                };
                let winner = match outcome.winner_team {
                    Some(t) if t == a_team => {
                        aw += 1;
                        Some(label_of(a))
                    }
                    Some(_) => {
                        bw += 1;
                        Some(label_of(b))
                    }
                    None => {
                        dw += 1;
                        None
                    }
                };
                report.record_game(label, seed, &outcome, winner);
            }
        }
        (aw, bw, dw)
    };

    match spec.bracket {
        crate::schema::Bracket::RoundRobin => {
            for i in 0..spec.entrants.len() {
                for j in (i + 1)..spec.entrants.len() {
                    let a = &spec.entrants[i];
                    let b = &spec.entrants[j];
                    let (aw, bw, _dw) = play_match(a, b);
                    let (la, lb) = (label_of(a), label_of(b));
                    match aw.cmp(&bw) {
                        std::cmp::Ordering::Greater => award(&mut standings, &la, &lb),
                        std::cmp::Ordering::Less => award(&mut standings, &lb, &la),
                        std::cmp::Ordering::Equal => draw(&mut standings, &la, &lb),
                    }
                }
            }
        }
        crate::schema::Bracket::SingleElim => {
            let mut round: Vec<PathBuf> = spec.entrants.clone();
            while round.len() > 1 {
                let mut next = Vec::new();
                for pair in round.chunks(2) {
                    if pair.len() == 1 {
                        next.push(pair[0].clone()); // bye
                        continue;
                    }
                    let (a, b) = (&pair[0], &pair[1]);
                    let (aw, bw, _dw) = play_match(a, b);
                    let (la, lb) = (label_of(a), label_of(b));
                    // Someone has to advance; a level match is decided by a
                    // coin that depends on the pair, not on who is listed
                    // first (both legs of every seed were played, so ties are
                    // common). The bracket needs that coin, the leaderboard
                    // doesn't: a level match is a draw for both entrants, not
                    // a win the tie-break invented.
                    let a_advances = match aw.cmp(&bw) {
                        std::cmp::Ordering::Greater => {
                            award(&mut standings, &la, &lb);
                            true
                        }
                        std::cmp::Ordering::Less => {
                            award(&mut standings, &lb, &la);
                            false
                        }
                        std::cmp::Ordering::Equal => {
                            draw(&mut standings, &la, &lb);
                            tie_break_favors_a(a, b, &seeds)
                        }
                    };
                    next.push(if a_advances { a.clone() } else { b.clone() });
                }
                round = next;
            }
        }
    }

    let mut rows: Vec<Standing> = standings.into_values().collect();
    rows.sort_by(|a, b| {
        b.points
            .cmp(&a.points)
            .then(b.wins.cmp(&a.wins))
            .then(a.label.cmp(&b.label))
    });
    report.standings = rows;
    Ok(report)
}

/// Does a dead-level single-elimination match go to `a`? The coin is a hash
/// of the pairing — the two labels *sorted*, plus the seeds — so it is
/// reproducible, and, unlike advancing `pair[0]`, it doesn't reward being
/// listed first in the bracket.
fn tie_break_favors_a(a: &Path, b: &Path, seeds: &[u64]) -> bool {
    const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;
    let (la, lb) = (label_of(a), label_of(b));
    let a_first = la <= lb;
    let (lo, hi) = if a_first { (&la, &lb) } else { (&lb, &la) };

    let mut hash = 0xcbf2_9ce4_8422_2325_u64; // FNV-1a offset basis
    for byte in lo.bytes().chain([0]).chain(hi.bytes()) {
        hash = (hash ^ u64::from(byte)).wrapping_mul(FNV_PRIME);
    }
    for &seed in seeds {
        hash = (hash ^ seed).wrapping_mul(FNV_PRIME);
    }

    // The coin picks one of the two *labels*; map it back to the entrants.
    (hash & 1 == 0) == a_first
}

fn award(standings: &mut HashMap<String, Standing>, winner: &str, loser: &str) {
    if let Some(s) = standings.get_mut(winner) {
        s.wins += 1;
        s.points += 3;
    }
    if let Some(s) = standings.get_mut(loser) {
        s.losses += 1;
    }
}

fn draw(standings: &mut HashMap<String, Standing>, a: &str, b: &str) {
    for label in [a, b] {
        if let Some(s) = standings.get_mut(label) {
            s.draws += 1;
            s.points += 1;
        }
    }
}

fn label_of(path: &Path) -> String {
    path.file_stem()
        .and_then(|s| s.to_str())
        .map_or_else(|| path.display().to_string(), ToString::to_string)
}

// ---------------------------------------------------------------------------
// Random point-buy
// ---------------------------------------------------------------------------

/// Run randomized point-buy fuzzing: generate `spec.runs` seeded builds, apply
/// each to the targeted entities, and fight. Builds that beat the hero are kept
/// as `Loss` cells so the caller can surface them; a build whose fight can't be
/// run is an `Error` cell.
///
/// # Errors
/// None today; the `Result` keeps the driver signatures uniform.
pub fn run_random(
    base: &Scenario,
    base_dir: &Path,
    spec: &RandomSpec,
    hero_team: i64,
) -> Result<TestReport> {
    // AIs are fixed across runs — only stats change — so the cache is built once.
    let cache = precompile(base, base_dir, &[]);
    let mut report = TestReport::new("random", Scoring::Hero);

    for run in 0..spec.runs {
        let run_seed = build_gen::mix(spec.seed, u64::from(run));
        let build = build_gen::gen_build(spec.capital, &spec.stats, spec.min_per_stat, run_seed);

        let mut scn = base.clone();
        for e in &mut scn.entities {
            let team = e.team.unwrap_or(0);
            let hit = match spec.target {
                RandomTarget::Hero => team == hero_team,
                RandomTarget::Opponent => team != hero_team,
                RandomTarget::Both => true,
            };
            if hit {
                build_gen::apply_build(e, &build);
            }
        }
        let fight_seed = base.seed.unwrap_or(1).wrapping_add(u64::from(run));
        scn.seed = Some(fight_seed);

        let label = format!("build#{run} {}", fmt_build(&build));
        match play_one(&scn, base_dir, &cache) {
            Ok(outcome) => report.record_hero(label, fight_seed, &outcome, hero_team),
            Err(e) => report.record_failure(label, fight_seed, &e),
        }
    }
    Ok(report)
}

fn fmt_build(build: &HashMap<crate::schema::StatKind, i64>) -> String {
    let mut parts: Vec<String> = build.iter().map(|(k, v)| format!("{k:?}={v}")).collect();
    parts.sort();
    parts.join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A two-leek, AI-less arena: every fight is an idle 1-turn draw, so the
    /// only thing that can go wrong in a cell is the opponent AI itself.
    fn arena() -> Scenario {
        Scenario::from_toml_str(
            r"
            max_turns = 1
            [map]
            width = 5
            height = 5
            [[entities]]
            id = 1
            team = 0
            cell = 0
            [[entities]]
            id = 2
            team = 1
            cell = 24
            ",
        )
        .expect("parse arena")
    }

    /// Regression (#67): a matrix cell whose opponent can't be compiled used to
    /// abort the whole sweep with `?`; it's now an `Error` cell and the other
    /// cells still play.
    #[test]
    fn matrix_reports_a_failing_cell_and_keeps_going() {
        let dir = std::env::temp_dir().join(format!("leek-matrix-cell-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        std::fs::write(dir.join("idle.leek"), "return 0;\n").expect("write AI");

        let axes = MatrixAxes {
            seeds: vec![1],
            opponents: vec![PathBuf::from("missing.leek"), PathBuf::from("idle.leek")],
            profiles: Vec::new(),
        };
        let report = run_matrix(&arena(), &dir, &axes, 0).expect("the sweep itself succeeds");
        let _ = std::fs::remove_dir_all(&dir);

        assert_eq!(report.cells.len(), 2);
        assert_eq!(report.cells[0].result, CellOutcome::Error);
        let failure = report.cells[0].failure.as_deref().unwrap_or_default();
        assert!(failure.contains("missing.leek"), "failure: {failure}");
        assert_eq!(report.cells[1].result, CellOutcome::Hero(FightResult::Draw));
        assert_eq!((report.errors, report.draws), (1, 1));
    }

    /// Regression (#39): a tournament pairing used to put the first entrant in
    /// `team_a` for every game, so whatever edge that slot carries went to the
    /// entrant listed first. Each seed is now played from both sides.
    #[test]
    fn tournament_plays_each_seed_from_both_sides() {
        let dir =
            std::env::temp_dir().join(format!("leek-tournament-sides-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        std::fs::write(dir.join("a.leek"), "return 0;\n").expect("write AI");
        std::fs::write(dir.join("b.leek"), "return 1;\n").expect("write AI");

        let spec = TournamentSpec {
            entrants: vec![PathBuf::from("a.leek"), PathBuf::from("b.leek")],
            bracket: crate::schema::Bracket::RoundRobin,
            seeds: vec![1, 2],
            scope: EntrantScope::Lead,
        };
        let report = run_tournament(&arena(), &dir, &spec).expect("the tournament runs");
        let _ = std::fs::remove_dir_all(&dir);

        // Two seeds × two legs, and each seed shows up on both sides.
        assert_eq!(report.cells.len(), 4);
        for seed in [1, 2] {
            for sides in ["as-listed", "swapped"] {
                assert!(
                    report
                        .cells
                        .iter()
                        .any(|c| c.seed == seed && c.label.contains(sides)),
                    "missing seed {seed} {sides} in {:?}",
                    report.cells.iter().map(|c| &c.label).collect::<Vec<_>>()
                );
            }
        }
    }

    /// Regression (#66): every tournament cell used to be classified against
    /// whichever entrant the pairing happened to list first, so the report's
    /// win/loss totals counted "the side called `a`" and each cell's WIN/LOSS
    /// said nothing. A cell now names the entrant that won the game, and the
    /// hero totals — which need a hero — stay unset.
    #[test]
    fn tournament_cells_name_the_winner_and_leave_the_hero_totals_unset() {
        let dir =
            std::env::temp_dir().join(format!("leek-tournament-cells-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        // A leek starts unequipped, so the shooter equips before firing; the
        // other entrant idles and is shot down from either side of the map.
        std::fs::write(
            dir.join("shooter.leek"),
            "// @version: 4\n\
             var target = getNearestEnemy();\n\
             setWeapon(getWeapons()[0]);\n\
             while (getTP() >= 3) { if (useWeapon(target) <= 0) { break; } }\n",
        )
        .expect("write AI");
        std::fs::write(dir.join("idle.leek"), "return 0;\n").expect("write AI");

        let base = Scenario::from_toml_str(
            r"
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
            [[entities]]
            id = 2
            team = 1
            cell = 4
            life = 100
            tp = 10
            weapons = [37]
            ",
        )
        .expect("parse arena");

        let spec = TournamentSpec {
            entrants: vec![PathBuf::from("shooter.leek"), PathBuf::from("idle.leek")],
            bracket: crate::schema::Bracket::RoundRobin,
            seeds: vec![1],
            scope: EntrantScope::Lead,
        };
        let report = run_tournament(&base, &dir, &spec).expect("the tournament runs");
        let _ = std::fs::remove_dir_all(&dir);

        // Both legs of the seed: the shooter wins each, whichever slot it sat
        // in, and the cell says so by name.
        assert_eq!(report.cells.len(), 2);
        for c in &report.cells {
            assert_eq!(
                c.result,
                CellOutcome::Won("shooter".to_string()),
                "{}: {:?}",
                c.label,
                c.result
            );
        }

        // No hero: the totals stay unset and there is no win rate to read.
        assert_eq!(report.scoring, Scoring::Leaderboard);
        assert_eq!((report.wins, report.losses, report.draws), (0, 0, 0));
        assert_eq!(report.win_rate(), None);

        // The standings carry the real record.
        let record = |label: &str| {
            let s = report
                .standings
                .iter()
                .find(|s| s.label == label)
                .expect("entrant in the standings");
            (s.wins, s.losses, s.draws)
        };
        assert_eq!(record("shooter"), (1, 0, 0));
        assert_eq!(record("idle"), (0, 1, 0));
    }

    /// Regression (#65): a level single-elimination match used to be recorded
    /// as a win for whoever advanced. It advances someone — the bracket needs
    /// it — but the leaderboard says draw.
    #[test]
    fn single_elim_records_a_level_match_as_a_draw() {
        let dir = std::env::temp_dir().join(format!("leek-tournament-tie-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        std::fs::write(dir.join("a.leek"), "return 0;\n").expect("write AI");
        std::fs::write(dir.join("b.leek"), "return 1;\n").expect("write AI");

        let spec = TournamentSpec {
            entrants: vec![PathBuf::from("a.leek"), PathBuf::from("b.leek")],
            bracket: crate::schema::Bracket::SingleElim,
            seeds: vec![1],
            scope: EntrantScope::Lead,
        };
        let report = run_tournament(&arena(), &dir, &spec).expect("the tournament runs");
        let _ = std::fs::remove_dir_all(&dir);

        // Every game of the arena is an idle draw, so the match is level.
        assert!(report.cells.iter().all(|c| c.result == CellOutcome::Level));
        assert_eq!(report.standings.len(), 2);
        for s in &report.standings {
            assert_eq!(
                (s.wins, s.losses, s.draws, s.points),
                (0, 0, 1, 1),
                "{} should be level, not a tie-break win",
                s.label
            );
        }
    }

    /// Regression (#65): an entrant took over its team's lead entity only,
    /// with no way to ask for the whole team. `scope = Team` hands it all over.
    #[test]
    fn entrant_scope_team_replaces_every_member_of_the_team() {
        let dir =
            std::env::temp_dir().join(format!("leek-tournament-scope-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        std::fs::write(dir.join("a.leek"), "return 0;\n").expect("write AI");
        std::fs::write(dir.join("b.leek"), "return 1;\n").expect("write AI");
        // The support leek of team 0 can't compile, so a game only runs when
        // the entrant's AI has replaced it too.
        std::fs::write(dir.join("broken.leek"), "return (;\n").expect("write AI");

        let base = Scenario::from_toml_str(
            r#"
            max_turns = 1
            [map]
            width = 5
            height = 5
            [[entities]]
            id = 1
            team = 0
            cell = 0
            [[entities]]
            id = 3
            team = 0
            cell = 1
            ai = "broken.leek"
            [[entities]]
            id = 2
            team = 1
            cell = 24
            "#,
        )
        .expect("parse arena");

        let mut spec = TournamentSpec {
            entrants: vec![PathBuf::from("a.leek"), PathBuf::from("b.leek")],
            bracket: crate::schema::Bracket::RoundRobin,
            seeds: vec![1],
            scope: EntrantScope::Lead,
        };
        let lead = run_tournament(&base, &dir, &spec).expect("the tournament runs");
        spec.scope = EntrantScope::Team;
        let team = run_tournament(&base, &dir, &spec).expect("the tournament runs");
        let _ = std::fs::remove_dir_all(&dir);

        // A tournament has no hero totals, so count the games that ran.
        let level = |r: &TestReport| {
            r.cells
                .iter()
                .filter(|c| c.result == CellOutcome::Level)
                .count()
        };
        assert_eq!(
            (lead.errors, level(&lead)),
            (2, 0),
            "lead leaves the support AI in place"
        );
        assert_eq!(
            (team.errors, level(&team)),
            (0, 2),
            "team scope replaces it"
        );
    }

    /// A level single-elimination match is decided by the pairing, not by the
    /// bracket position: swapping the two entrants advances the same one.
    #[test]
    fn tie_break_does_not_favor_the_entrant_listed_first() {
        let seeds = [1, 2, 3];
        for (x, y) in [("alpha", "beta"), ("beta", "gamma"), ("zz", "aa")] {
            let (a, b) = (PathBuf::from(x), PathBuf::from(y));
            assert_ne!(
                tie_break_favors_a(&a, &b, &seeds),
                tie_break_favors_a(&b, &a, &seeds),
                "{x} vs {y} must resolve to the same entrant either way round"
            );
        }
    }
}
