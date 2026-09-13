//! `miku fight` — run a leek-wars fight from a scenario file, or test the hero
//! AI against many settings (matrix sweep, tournament, randomized builds).

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Context, Result, anyhow};
use leek_manifest::FightTable;
use leek_project::Project;
use leek_scenario::{
    Bracket, CellOutcome, EntrantScope, FightResult, MatrixAxes, RandomSpec, RandomTarget,
    Scenario, StatKind, TestReport, TournamentSpec,
};

use crate::cli::{BracketArg, EntrantScopeArg, Fight, FightFormat, FightMode, RandomTargetArg};

pub fn run(args: &Fight, manifest_path: Option<&Path>, quiet: bool) -> Result<ExitCode> {
    // A fight always needs the leek-wars game builtins resolvable at compile
    // time, so register them up front (idempotent — harmless if `--library
    // leekwars` already did). This makes plain `miku fight scenario.toml` work.
    leek_recipes::load_and_register_libraries(["leekwars"])
        .map_err(|e| anyhow!("registering the leekwars library: {e}"))?;

    // Fights run on bare scenario files too, so a missing `Miku.toml` is not
    // an error here — the `[fight]` defaults apply. An explicit
    // `--manifest-path` still has to load.
    let project = match manifest_path {
        Some(path) => Some(Project::discover(Some(path))?),
        None => Project::discover(None).ok(),
    };
    if let Some(project) = &project {
        for w in &project.warnings {
            eprintln!("warning: {w}");
        }
    }

    let scenario_path = resolve_scenario(args, project.as_ref())?;
    let base_dir = scenario_path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .to_path_buf();

    let mut scn = Scenario::load(&scenario_path)?;
    if let Some(name) = &args.profile {
        scn.apply_profile(name)?;
    }
    if args.seed.is_some() {
        scn.seed = args.seed;
    }
    if args.max_turns.is_some() {
        scn.max_turns = args.max_turns;
    }

    // `--emit` generates a standalone executable of this fight instead of
    // running it. Honors the seed/profile/max-turns overrides applied above.
    if let Some(out) = &args.emit {
        crate::cmd::fight_emit::emit(&scn, &base_dir, out, quiet)?;
        return Ok(ExitCode::SUCCESS);
    }

    let hero_team = args
        .hero_team
        .or_else(|| scn.testing.as_ref().and_then(|t| t.hero_team))
        .unwrap_or_else(|| scn.entities.first().and_then(|e| e.team).unwrap_or(0));

    // `--report` writes the same JSON the `--format json` renderer prints;
    // with no path it lands in the manifest's `[fight].reports_dir`.
    let report_dest = report_dest(args, project.as_ref());

    // `--mode` selects the driver (default `single`); the scenario's
    // `[testing]` table supplies that driver's parameters.
    match args.mode {
        FightMode::Single => run_single(
            &scn,
            &base_dir,
            hero_team,
            args.format,
            report_dest.as_deref(),
            quiet,
        ),
        FightMode::Matrix => {
            let report = run_matrix_mode(args, &scn, &base_dir, hero_team)?;
            render_report(&report, args.format);
            write_report(report_dest.as_deref(), &report_json(&report), quiet)?;
            Ok(verdict(&report))
        }
        FightMode::Tournament => {
            let report = run_tournament_mode(args, &scn, &base_dir)?;
            render_report(&report, args.format);
            write_report(report_dest.as_deref(), &report_json(&report), quiet)?;
            Ok(verdict(&report))
        }
        FightMode::Random => {
            let report = run_random_mode(args, &scn, &base_dir, hero_team)?;
            render_report(&report, args.format);
            write_report(report_dest.as_deref(), &report_json(&report), quiet)?;
            Ok(verdict(&report))
        }
    }
}

/// The scenario to play: the CLI path (resolved against the project), else
/// the manifest's `[fight].default_scenario`.
fn resolve_scenario(args: &Fight, project: Option<&Project>) -> Result<PathBuf> {
    match (args.scenario.as_deref(), project) {
        (Some(path), Some(project)) => Ok(project.scenario_path(path)),
        (Some(path), None) => Ok(path.to_path_buf()),
        (None, Some(project)) => match project.default_scenario() {
            Some(path) if path.is_file() => Ok(path),
            Some(path) => Err(anyhow!(
                "Miku.toml: `[fight].default_scenario` = `{}` does not exist ({})",
                project
                    .manifest
                    .fight
                    .default_scenario
                    .as_deref()
                    .unwrap_or(&path)
                    .display(),
                path.display()
            )),
            None => Err(anyhow!(no_default_scenario_message(project))),
        },
        (None, None) => Err(anyhow!(
            "no scenario file given (usage: miku fight <scenario.toml>)"
        )),
    }
}

/// The error for a bare `miku fight` in a project whose manifest sets no
/// `[fight].default_scenario`. When `[fight].scenarios_dir` is set, the
/// scenarios sitting there are listed, so the user can pick one instead of
/// having to go looking.
fn no_default_scenario_message(project: &Project) -> String {
    let mut message = "no scenario file given and no `[fight].default_scenario` in the manifest \
         (usage: miku fight <scenario.toml>)"
        .to_string();
    let Some(dir) = &project.manifest.fight.scenarios_dir else {
        return message;
    };
    let dir = project.root.join(dir);
    match list_scenarios(&dir) {
        Ok(scenarios) if scenarios.is_empty() => {
            let _ = write!(message, "; no scenarios in {}", dir.display());
        }
        Ok(scenarios) => {
            let _ = write!(message, "; scenarios in {}:", dir.display());
            for scenario in scenarios {
                let rel = scenario.strip_prefix(&project.root).unwrap_or(&scenario);
                let _ = write!(message, "\n  {}", rel.display());
            }
        }
        Err(e) => {
            let _ = write!(message, "; {e}");
        }
    }
    message
}

/// Scenario files (`.toml`/`.json`, the manifest itself excluded) directly
/// inside `dir`, sorted.
fn list_scenarios(dir: &Path) -> Result<Vec<PathBuf>> {
    let entries = std::fs::read_dir(dir)
        .map_err(|e| anyhow!("reading `[fight].scenarios_dir` {}: {e}", dir.display()))?;
    let mut out: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.is_file()
                && p.file_name().is_some_and(|n| n != "Miku.toml")
                && p.extension()
                    .is_some_and(|ext| ext == "toml" || ext == "json")
        })
        .collect();
    out.sort();
    Ok(out)
}

/// Where `--report` writes, or `None` when it wasn't asked for. A bare
/// `--report` writes `<[fight].reports_dir>/<mode>.json`; outside a project
/// that is the manifest default, `build/fight-reports`.
fn report_dest(args: &Fight, project: Option<&Project>) -> Option<PathBuf> {
    let requested = args.report.as_ref()?;
    if let Some(path) = requested {
        return Some(path.clone());
    }
    let dir = project.map_or_else(
        || FightTable::default().reports_dir,
        Project::fight_reports_dir,
    );
    Some(dir.join(format!("{}.json", mode_name(args.mode))))
}

fn mode_name(mode: FightMode) -> &'static str {
    match mode {
        FightMode::Single => "single",
        FightMode::Matrix => "matrix",
        FightMode::Tournament => "tournament",
        FightMode::Random => "random",
    }
}

/// Write the JSON report to `dest` (creating its directory), and say where
/// it went unless `--quiet`.
fn write_report(dest: Option<&Path>, value: &serde_json::Value, quiet: bool) -> Result<()> {
    let Some(dest) = dest else {
        return Ok(());
    };
    if let Some(parent) = dest.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }
    let text = serde_json::to_string_pretty(value).context("serializing the fight report")?;
    std::fs::write(dest, format!("{text}\n"))
        .with_context(|| format!("writing {}", dest.display()))?;
    if !quiet {
        eprintln!("report written to {}", dest.display());
    }
    Ok(())
}

fn run_single(
    scn: &Scenario,
    base_dir: &Path,
    hero_team: i64,
    format: FightFormat,
    report_dest: Option<&Path>,
    quiet: bool,
) -> Result<ExitCode> {
    let lf = leek_scenario::build_fight(scn, base_dir)?;
    let fight = leek_generator::shared(lf.fight);
    let outcome = leek_generator::run_fight_release(
        &fight,
        &lf.ais,
        lf.max_turns,
        lf.version,
        lf.strict,
        lf.max_ops_per_turn,
    );

    let f = fight.borrow();
    let json = (format == FightFormat::Json || report_dest.is_some())
        .then(|| single_json(f.log(), &outcome));
    match format {
        FightFormat::Json => {
            let obj = json.as_ref().expect("built for the json format above");
            println!("{}", serde_json::to_string_pretty(obj)?);
        }
        FightFormat::Human => {
            match outcome.winner_team {
                Some(t) => println!("winner: team {t} ({} turns)", outcome.turns),
                None => println!("draw ({} turns)", outcome.turns),
            }
            if !quiet {
                for (id, msg) in f.log() {
                    println!("  [{id}] {msg}");
                }
            }
            // AI errors are the one thing `--quiet` doesn't hide: they explain
            // a turn an entity lost.
            for e in &outcome.errors {
                eprintln!("  AI error: {e}");
            }
        }
    }

    if let Some(obj) = &json {
        write_report(report_dest, obj, quiet)?;
    }

    let hero_won = outcome.winner_team == Some(hero_team);
    Ok(if hero_won || outcome.winner_team.is_none() {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    })
}

fn run_matrix_mode(
    args: &Fight,
    scn: &Scenario,
    base_dir: &Path,
    hero_team: i64,
) -> Result<TestReport> {
    let testing = scn.testing.clone().unwrap_or_default();
    let axes = MatrixAxes {
        seeds: pick_vec(&args.seeds, &testing.seeds),
        opponents: pick_vec(&args.vs, &testing.opponents),
        profiles: pick_vec(&args.with_profile, &testing.profiles),
    };
    leek_scenario::run_matrix(scn, base_dir, &axes, hero_team)
}

/// A tournament has no hero: every entrant is under test, each cell names the
/// entrant that won that game, and the leaderboard — not the report's win/loss
/// totals, which stay unset — is the result.
fn run_tournament_mode(args: &Fight, scn: &Scenario, base_dir: &Path) -> Result<TestReport> {
    let testing = scn.testing.clone().unwrap_or_default();
    let entrants = pick_vec(&args.entrant, &testing.entrants);
    let spec = TournamentSpec {
        entrants,
        bracket: match args.bracket {
            BracketArg::RoundRobin => Bracket::RoundRobin,
            BracketArg::SingleElim => Bracket::SingleElim,
        },
        seeds: pick_vec(&args.games, &testing.seeds),
        scope: args
            .entrant_scope
            .map(|s| match s {
                EntrantScopeArg::Lead => EntrantScope::Lead,
                EntrantScopeArg::Team => EntrantScope::Team,
            })
            .or(testing.entrant_scope)
            .unwrap_or_default(),
    };
    leek_scenario::run_tournament(scn, base_dir, &spec)
}

fn run_random_mode(
    args: &Fight,
    scn: &Scenario,
    base_dir: &Path,
    hero_team: i64,
) -> Result<TestReport> {
    let testing = scn.testing.clone().unwrap_or_default();
    let file_spec = testing.random.clone();

    let capital = args
        .capital
        .or_else(|| file_spec.as_ref().map(|r| r.capital))
        .ok_or_else(|| anyhow!("random mode needs --capital (or [testing.random].capital)"))?;
    let runs = args
        .runs
        .or_else(|| file_spec.as_ref().map(|r| r.runs))
        .unwrap_or(20);
    let stats = if args.random_stats.is_empty() {
        file_spec.as_ref().map_or_else(
            || vec![StatKind::Strength, StatKind::Agility, StatKind::Wisdom],
            |r| r.stats.clone(),
        )
    } else {
        parse_stats(&args.random_stats)?
    };
    let spec = RandomSpec {
        runs,
        capital,
        stats,
        min_per_stat: file_spec.as_ref().map_or(0, |r| r.min_per_stat),
        target: match args.random_target {
            RandomTargetArg::Hero => RandomTarget::Hero,
            RandomTargetArg::Opponent => RandomTarget::Opponent,
            RandomTargetArg::Both => RandomTarget::Both,
        },
        seed: file_spec.as_ref().map_or(0, |r| r.seed),
    };
    leek_scenario::run_random(scn, base_dir, &spec, hero_team)
}

/// CLI value wins when non-empty, else the scenario's `[testing]` value.
fn pick_vec<T: Clone>(cli: &[T], file: &[T]) -> Vec<T> {
    if cli.is_empty() {
        file.to_vec()
    } else {
        cli.to_vec()
    }
}

fn parse_stats(names: &[String]) -> Result<Vec<StatKind>> {
    names
        .iter()
        .map(|n| match n.to_ascii_lowercase().as_str() {
            "strength" => Ok(StatKind::Strength),
            "agility" => Ok(StatKind::Agility),
            "wisdom" => Ok(StatKind::Wisdom),
            "resistance" => Ok(StatKind::Resistance),
            "science" => Ok(StatKind::Science),
            "magic" => Ok(StatKind::Magic),
            "power" => Ok(StatKind::Power),
            other => Err(anyhow!("unknown stat '{other}'")),
        })
        .collect()
}

/// The exit status of a testing run, so it can be a regression gate.
///
/// Non-zero if the hero lost a fight, or a fight couldn't be run. A tournament
/// has no hero and so no losses to count: only a game that couldn't be run
/// fails it.
fn verdict(report: &TestReport) -> ExitCode {
    if report.losses > 0 || report.errors > 0 {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    }
}

fn render_report(report: &TestReport, format: FightFormat) {
    match format {
        FightFormat::Json => render_json(report),
        FightFormat::Human => render_human(report),
    }
}

fn render_human(report: &TestReport) {
    use leek_scenario::Scoring;

    println!("mode: {}", report.mode);
    println!(
        "{:<48} {:>10} {:>8} {:>6} {:>16}",
        "label", "seed", "winner", "turns", "result"
    );
    for c in &report.cells {
        let winner = c.winner.map_or_else(|| "-".to_string(), |t| t.to_string());
        println!(
            "{:<48} {:>10} {:>8} {:>6} {:>16}",
            truncate(&c.label, 48),
            c.seed,
            winner,
            c.turns,
            cell_result(&c.result)
        );
    }
    match report.scoring {
        Scoring::Hero => {
            let rate = report
                .win_rate()
                .map_or_else(|| "-".to_string(), |rate| format!("{rate:.1}%"));
            println!(
                "\nwins {}  losses {}  draws {}  errors {}   (win rate {rate})",
                report.wins, report.losses, report.draws, report.errors,
            );
        }
        // No hero, so no win rate to print and nothing for a win/loss total to
        // count: the leaderboard below is the result.
        Scoring::Leaderboard => {
            println!("\ngames {}  errors {}", report.cells.len(), report.errors);
        }
    }

    if !report.standings.is_empty() {
        println!("\nleaderboard:");
        println!(
            "{:<4} {:<24} {:>4} {:>4} {:>4} {:>4}",
            "#", "entrant", "pts", "W", "L", "D"
        );
        for (i, s) in report.standings.iter().enumerate() {
            println!(
                "{:<4} {:<24} {:>4} {:>4} {:>4} {:>4}",
                i + 1,
                truncate(&s.label, 24),
                s.points,
                s.wins,
                s.losses,
                s.draws
            );
        }
    }

    // Surface the settings that beat the hero. A leaderboard report has no
    // hero and so no losses: this is empty for a tournament.
    let beaten: Vec<&str> = report
        .cells
        .iter()
        .filter(|c| c.result == CellOutcome::Hero(FightResult::Loss))
        .map(|c| c.label.as_str())
        .collect();
    if !beaten.is_empty() {
        println!("\nlost to:");
        for label in beaten {
            println!("  {label}");
        }
    }

    // Surface cells that failed outright and AI errors inside fights that ran.
    let troubled: Vec<&leek_scenario::CellResult> = report
        .cells
        .iter()
        .filter(|c| c.failure.is_some() || !c.ai_errors.is_empty())
        .collect();
    if !troubled.is_empty() {
        println!("\nerrors:");
        for c in troubled {
            println!("  {}", c.label);
            if let Some(failure) = &c.failure {
                println!("    fight not run: {failure}");
            }
            for e in &c.ai_errors {
                println!("    AI error: {e}");
            }
        }
    }
}

/// A cell's result column. A tournament game names the entrant that won it:
/// there is no hero to call it a win or a loss for.
fn cell_result(result: &CellOutcome) -> String {
    match result {
        CellOutcome::Hero(FightResult::Win) => "WIN".to_string(),
        CellOutcome::Hero(FightResult::Loss) => "LOSS".to_string(),
        CellOutcome::Hero(FightResult::Draw) | CellOutcome::Level => "DRAW".to_string(),
        CellOutcome::Won(label) => format!("{} won", truncate(label, 12)),
        CellOutcome::Error => "ERROR".to_string(),
    }
}

/// The JSON body of a single fight — what `--format json` prints and what
/// `--report` writes.
fn single_json(log: &[(i64, String)], outcome: &leek_generator::Outcome) -> serde_json::Value {
    let log: Vec<_> = log
        .iter()
        .map(|(id, msg)| serde_json::json!({ "entity": id, "message": msg }))
        .collect();
    let errors: Vec<_> = outcome
        .errors
        .iter()
        .map(|e| serde_json::json!({ "turn": e.turn, "entity": e.entity, "error": e.error }))
        .collect();
    serde_json::json!({
        "winner_team": outcome.winner_team,
        "turns": outcome.turns,
        "log": log,
        "errors": errors,
    })
}

fn render_json(report: &TestReport) {
    match serde_json::to_string_pretty(&report_json(report)) {
        Ok(s) => println!("{s}"),
        Err(e) => eprintln!("error serializing report: {e}"),
    }
}

/// The JSON body of a sweep/tournament/random report.
fn report_json(report: &TestReport) -> serde_json::Value {
    use leek_scenario::Scoring;

    let cells: Vec<_> = report
        .cells
        .iter()
        .map(|c| {
            // `winner_entrant` names who won a tournament game; a hero-mode
            // cell leaves it null and says win/loss instead.
            let (result, winner_entrant) = match &c.result {
                CellOutcome::Hero(FightResult::Win) => ("win", None),
                CellOutcome::Hero(FightResult::Loss) => ("loss", None),
                CellOutcome::Hero(FightResult::Draw) | CellOutcome::Level => ("draw", None),
                CellOutcome::Won(label) => ("win", Some(label.clone())),
                CellOutcome::Error => ("error", None),
            };
            serde_json::json!({
                "label": c.label,
                "seed": c.seed,
                "winner": c.winner,
                "winner_entrant": winner_entrant,
                "turns": c.turns,
                "result": result,
                "failure": c.failure,
                "ai_errors": c.ai_errors,
            })
        })
        .collect();
    let standings: Vec<_> = report
        .standings
        .iter()
        .map(|s| {
            serde_json::json!({
                "label": s.label,
                "points": s.points,
                "wins": s.wins,
                "losses": s.losses,
                "draws": s.draws,
            })
        })
        .collect();
    // The win/loss/draw totals only mean something against a hero team, so a
    // leaderboard report reports them as null rather than as zeroes.
    let hero = report.scoring == Scoring::Hero;
    serde_json::json!({
        "mode": report.mode,
        "scoring": match report.scoring {
            Scoring::Hero => "hero",
            Scoring::Leaderboard => "leaderboard",
        },
        "wins": hero.then_some(report.wins),
        "losses": hero.then_some(report.losses),
        "draws": hero.then_some(report.draws),
        "errors": report.errors,
        "win_rate": report.win_rate(),
        "cells": cells,
        "standings": standings,
    })
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
        out.push('…');
        out
    }
}
