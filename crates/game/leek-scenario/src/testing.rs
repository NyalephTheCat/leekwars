//! Testing modes — drive an AI against many settings on top of one single-fight
//! primitive ([`play_one`]) and one compiled-AI cache.
//!
//! - [`run_matrix`] — a cartesian sweep over seeds × opponents × profiles.
//! - [`run_tournament`] — round-robin or single-elimination among N entrants,
//!   producing a leaderboard.
//! - [`run_random`] — randomized point-buy build fuzzing (see [`build_gen`]).
//!
//! All classify each fight relative to a *hero team* (the AI under test) and
//! return a [`TestReport`]. One bad cell never sinks the run: an AI error
//! inside a fight is contained by the generator and listed on the cell
//! ([`CellResult::ai_errors`]), and a fight that can't even be set up (an AI
//! that doesn't compile, say) becomes a [`FightResult::Error`] cell.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Result, bail};
use leek_generator::Outcome;
use leek_hir::HirFile;

use crate::build_gen;
use crate::load::{build_fight_with_cache, compile_ai};
use crate::schema::{RandomSpec, RandomTarget, Scenario};

/// Outcome of a single fight relative to the hero team.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FightResult {
    Win,
    Loss,
    Draw,
    /// The fight couldn't be run; [`CellResult::failure`] says why.
    Error,
}

/// One fight in a report.
#[derive(Debug, Clone)]
pub struct CellResult {
    pub label: String,
    pub seed: u64,
    pub winner: Option<i64>,
    pub turns: u32,
    pub result: FightResult,
    /// AI turns that ended in an error during the fight (each rendered as
    /// `turn N: entity E: ERROR`); the fight still played out.
    pub ai_errors: Vec<String>,
    /// Why the fight couldn't be run, for a [`FightResult::Error`] cell.
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
    pub cells: Vec<CellResult>,
    pub standings: Vec<Standing>,
    pub wins: u32,
    pub losses: u32,
    pub draws: u32,
    /// Cells whose fight couldn't be run ([`FightResult::Error`]).
    pub errors: u32,
}

impl TestReport {
    fn new(mode: &'static str) -> Self {
        Self {
            mode,
            cells: Vec::new(),
            standings: Vec::new(),
            wins: 0,
            losses: 0,
            draws: 0,
            errors: 0,
        }
    }

    fn record(
        &mut self,
        label: String,
        seed: u64,
        outcome: &Outcome,
        hero_team: i64,
    ) -> FightResult {
        let result = classify(outcome.winner_team, hero_team);
        match result {
            FightResult::Win => self.wins += 1,
            FightResult::Loss => self.losses += 1,
            FightResult::Draw => self.draws += 1,
            FightResult::Error => self.errors += 1,
        }
        self.cells.push(CellResult {
            label,
            seed,
            winner: outcome.winner_team,
            turns: outcome.turns,
            result,
            ai_errors: outcome.errors.iter().map(ToString::to_string).collect(),
            failure: None,
        });
        result
    }

    /// Record a cell whose fight couldn't be run.
    fn record_failure(&mut self, label: String, seed: u64, err: &anyhow::Error) {
        self.errors += 1;
        self.cells.push(CellResult {
            label,
            seed,
            winner: None,
            turns: 0,
            result: FightResult::Error,
            ai_errors: Vec::new(),
            failure: Some(format!("{err:#}")),
        });
    }

    /// Win rate over the fights that ran (wins, losses and draws; error cells
    /// don't count), as a percentage.
    #[must_use]
    pub fn win_rate(&self) -> f64 {
        let total = self.wins + self.losses + self.draws;
        if total == 0 {
            0.0
        } else {
            f64::from(self.wins) * 100.0 / f64::from(total)
        }
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
/// [`FightResult::Error`], while the other cells still play.
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

/// Point the lead (first) entity of `team` at a different AI file.
fn set_team_lead_ai(scn: &mut Scenario, team: i64, ai: &Path) {
    if let Some(e) = scn
        .entities
        .iter_mut()
        .find(|e| e.team.unwrap_or(0) == team)
    {
        e.ai = Some(ai.to_path_buf());
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
/// as [`FightResult::Error`] and the sweep continues.
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

    let mut report = TestReport::new("matrix");
    for &seed in &seeds {
        for opp in &opponents {
            for prof in &profiles {
                let mut scn = base.clone();
                if let Some(name) = prof {
                    scn.apply_profile(name)?;
                }
                scn.seed = Some(seed);
                if let (Some(opp_path), Some(team)) = (opp, opp_team) {
                    set_team_lead_ai(&mut scn, team, opp_path);
                }
                let label = format!(
                    "seed={seed} opp={} profile={}",
                    opp.map_or("-", |p| p.to_str().unwrap_or("?")),
                    prof.map_or("-", String::as_str),
                );
                match play_one(&scn, base_dir, &cache) {
                    Ok(outcome) => {
                        report.record(label, seed, &outcome, hero_team);
                    }
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

/// Tournament configuration. Each entrant is an AI file; it is dropped into the
/// hero team's lead slot and faces the others.
#[derive(Debug, Clone)]
pub struct TournamentSpec {
    pub entrants: Vec<PathBuf>,
    pub bracket: crate::schema::Bracket,
    /// Seeds played per pairing (each is one game); the side winning the most
    /// games takes the match.
    pub seeds: Vec<u64>,
}

/// Run a tournament among `entrants`, returning a leaderboard in `standings`.
/// A game that can't be run is reported as a [`FightResult::Error`] cell and
/// counts for neither side.
///
/// # Errors
/// Needs at least two entrants and two teams in the base scenario.
pub fn run_tournament(
    base: &Scenario,
    base_dir: &Path,
    spec: &TournamentSpec,
    _hero_team: i64,
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

    let mut report = TestReport::new("tournament");
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

    // Play A (team_a) vs B (team_b) over the seeds; returns (a_wins, b_wins, draws).
    let mut play_match = |a: &Path, b: &Path| -> (u32, u32, u32) {
        let (mut aw, mut bw, mut dw) = (0, 0, 0);
        for &seed in &seeds {
            let mut scn = base.clone();
            scn.seed = Some(seed);
            set_team_lead_ai(&mut scn, team_a, a);
            set_team_lead_ai(&mut scn, team_b, b);
            let label = format!("{} vs {} @seed={seed}", label_of(a), label_of(b));
            let outcome = match play_one(&scn, base_dir, &cache) {
                Ok(outcome) => outcome,
                Err(e) => {
                    report.record_failure(label, seed, &e);
                    continue;
                }
            };
            match outcome.winner_team {
                Some(t) if t == team_a => aw += 1,
                Some(_) => bw += 1,
                None => dw += 1,
            }
            report.record(label, seed, &outcome, team_a);
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
                    if bw > aw {
                        award(&mut standings, &lb, &la);
                        next.push(b.clone());
                    } else {
                        award(&mut standings, &la, &lb);
                        next.push(a.clone());
                    }
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
    let mut report = TestReport::new("random");

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
            Ok(outcome) => {
                report.record(label, fight_seed, &outcome, hero_team);
            }
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
        assert_eq!(report.cells[0].result, FightResult::Error);
        let failure = report.cells[0].failure.as_deref().unwrap_or_default();
        assert!(failure.contains("missing.leek"), "failure: {failure}");
        assert_eq!(report.cells[1].result, FightResult::Draw);
        assert_eq!((report.errors, report.draws), (1, 1));
    }
}
