//! `miku fight` — run a leek-wars fight from a scenario file, or test the hero
//! AI against many settings (matrix sweep, tournament, randomized builds).

use std::path::Path;
use std::process::ExitCode;

use anyhow::{anyhow, Result};
use leek_scenario::{
    Bracket, MatrixAxes, RandomSpec, RandomTarget, Scenario, StatKind, TestReport, TournamentSpec,
};

use crate::cli::{BracketArg, Fight, FightFormat, FightMode, RandomTargetArg};

pub fn run(args: Fight, quiet: bool) -> Result<ExitCode> {
    // A fight always needs the leek-wars game builtins resolvable at compile
    // time, so register them up front (idempotent — harmless if `--library
    // leekwars` already did). This makes plain `miku fight scenario.toml` work.
    leek_recipes::load_and_register_libraries(["leekwars"])
        .map_err(|e| anyhow!("registering the leekwars library: {e}"))?;

    let Some(scenario_path) = args.scenario.clone() else {
        return Err(anyhow!(
            "no scenario file given (usage: miku fight <scenario.toml>)"
        ));
    };
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

    // `--mode` selects the driver (default `single`); the scenario's
    // `[testing]` table supplies that driver's parameters.
    match args.mode {
        FightMode::Single => run_single(&scn, &base_dir, hero_team, args.format, quiet),
        FightMode::Matrix => {
            let report = run_matrix_mode(&args, &scn, &base_dir, hero_team)?;
            render_report(&report, args.format);
            Ok(verdict(&report))
        }
        FightMode::Tournament => {
            let report = run_tournament_mode(&args, &scn, &base_dir, hero_team)?;
            render_report(&report, args.format);
            Ok(ExitCode::SUCCESS)
        }
        FightMode::Random => {
            let report = run_random_mode(&args, &scn, &base_dir, hero_team)?;
            render_report(&report, args.format);
            Ok(verdict(&report))
        }
    }
}

fn run_single(
    scn: &Scenario,
    base_dir: &Path,
    hero_team: i64,
    format: FightFormat,
    quiet: bool,
) -> Result<ExitCode> {
    let lf = leek_scenario::build_fight(scn, base_dir)?;
    let fight = leek_generator::shared(lf.fight);
    let outcome =
        leek_generator::run_fight_release(&fight, &lf.ais, lf.max_turns, lf.version, lf.strict)
            .map_err(|e| anyhow!("fight execution error: {e}"))?;

    let f = fight.borrow();
    match format {
        FightFormat::Json => {
            let log: Vec<_> = f
                .log()
                .iter()
                .map(|(id, msg)| serde_json::json!({ "entity": id, "message": msg }))
                .collect();
            let obj = serde_json::json!({
                "winner_team": outcome.winner_team,
                "turns": outcome.turns,
                "log": log,
            });
            println!("{}", serde_json::to_string_pretty(&obj)?);
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
        }
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

fn run_tournament_mode(
    args: &Fight,
    scn: &Scenario,
    base_dir: &Path,
    hero_team: i64,
) -> Result<TestReport> {
    let testing = scn.testing.clone().unwrap_or_default();
    let entrants = pick_vec(&args.entrant, &testing.entrants);
    let spec = TournamentSpec {
        entrants,
        bracket: match args.bracket {
            BracketArg::RoundRobin => Bracket::RoundRobin,
            BracketArg::SingleElim => Bracket::SingleElim,
        },
        seeds: pick_vec(&args.games, &testing.seeds),
    };
    leek_scenario::run_tournament(scn, base_dir, &spec, hero_team)
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

fn verdict(report: &TestReport) -> ExitCode {
    // Non-zero if the hero lost any fight — useful as a regression gate.
    if report.losses > 0 {
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
    use leek_scenario::FightResult;

    println!("mode: {}", report.mode);
    println!("{:<48} {:>10} {:>8} {:>6} {:>6}", "label", "seed", "winner", "turns", "result");
    for c in &report.cells {
        let winner = c.winner.map_or_else(|| "-".to_string(), |t| t.to_string());
        let result = match c.result {
            FightResult::Win => "WIN",
            FightResult::Loss => "LOSS",
            FightResult::Draw => "DRAW",
        };
        println!(
            "{:<48} {:>10} {:>8} {:>6} {:>6}",
            truncate(&c.label, 48),
            c.seed,
            winner,
            c.turns,
            result
        );
    }
    println!(
        "\nwins {}  losses {}  draws {}   (win rate {:.1}%)",
        report.wins,
        report.losses,
        report.draws,
        report.win_rate()
    );

    if !report.standings.is_empty() {
        println!("\nleaderboard:");
        println!("{:<4} {:<24} {:>4} {:>4} {:>4} {:>4}", "#", "entrant", "pts", "W", "L", "D");
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

    // Surface the settings that beat the hero.
    let beaten: Vec<&str> = report
        .cells
        .iter()
        .filter(|c| c.result == FightResult::Loss)
        .map(|c| c.label.as_str())
        .collect();
    if !beaten.is_empty() {
        println!("\nlost to:");
        for label in beaten {
            println!("  {label}");
        }
    }
}

fn render_json(report: &TestReport) {
    use leek_scenario::FightResult;

    let cells: Vec<_> = report
        .cells
        .iter()
        .map(|c| {
            serde_json::json!({
                "label": c.label,
                "seed": c.seed,
                "winner": c.winner,
                "turns": c.turns,
                "result": match c.result {
                    FightResult::Win => "win",
                    FightResult::Loss => "loss",
                    FightResult::Draw => "draw",
                },
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
    let obj = serde_json::json!({
        "mode": report.mode,
        "wins": report.wins,
        "losses": report.losses,
        "draws": report.draws,
        "win_rate": report.win_rate(),
        "cells": cells,
        "standings": standings,
    });
    match serde_json::to_string_pretty(&obj) {
        Ok(s) => println!("{s}"),
        Err(e) => eprintln!("error serializing report: {e}"),
    }
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
