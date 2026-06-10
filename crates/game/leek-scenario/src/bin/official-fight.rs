//! `official-fight` — the Rust twin of `tools/fight-harness/fight.sh`.
//!
//! Runs a 1v1 between two compiled `.leek` AIs through the official-parity
//! engine ([`leek_generator::official`]) under the exact fight-harness
//! scenario (`tools/fight-harness/Harness.java`): two stock level-10 leeks
//! and the synthetic pistol 37. Prints the official Outcome JSON on stdout —
//! byte-comparable (modulo `ops`/`execution_time`) with the Java harness's
//! output for the same AIs and seed, which is what
//! `tools/fight-harness/check-conformance.sh` does.

use std::collections::HashMap;
use std::path::Path;
use std::process::ExitCode;

use anyhow::{Context, Result, anyhow};
use leek_generator::official::{
    Fighter, STAT_AGILITY, STAT_FREQUENCY, STAT_LIFE, STAT_MP, STAT_RESISTANCE, STAT_STRENGTH,
    STAT_TP, STAT_WISDOM, State, Stats, WeaponSpec, run_official_fight,
};

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
/// from the harness (it makes every `useWeapon` return `USE_MAX_USES`).
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

fn run() -> Result<serde_json::Value> {
    let args: Vec<String> = std::env::args().collect();
    let (ai1, ai2) = match args.as_slice() {
        [_, a, b] | [_, a, b, _] => (a.clone(), b.clone()),
        _ => {
            return Err(anyhow!(
                "usage: official-fight <ai1.leek> <ai2.leek> [seed]"
            ));
        }
    };
    let seed: i64 = match args.get(3) {
        Some(s) => s.parse().with_context(|| format!("invalid seed {s:?}"))?,
        None => 1,
    };

    // The fight functions must resolve at compile time, same as `miku fight`,
    // and the fight constants must fold to literals like the harness's
    // `leekc --fold-constants` invocation that generated the goldens.
    leek_recipes::load_and_register_libraries(["leekwars"])
        .map_err(|e| anyhow!("registering the leekwars library: {e}"))?;
    leek_recipes::activate_leekwars_constant_folding();

    let mut state = State::new(seed);
    state.add_entity(0, harness_leek(1, "AI_1"));
    state.add_entity(1, harness_leek(2, "AI_2"));
    state.weapon_specs.insert(37, harness_pistol());

    let mut ais = HashMap::new();
    for (fid, path) in [(0_usize, &ai1), (1, &ai2)] {
        ais.insert(fid, leek_scenario::compile_ai(Path::new(path), 4, false)?);
    }

    let opts = leek_generator::NativeOptions::release()
        .with_lang(4, false)
        .with_link_game(true);
    run_official_fight(state, &ais, &[0], &opts).map_err(|e| anyhow!("running fight: {e}"))
}

fn main() -> ExitCode {
    match run() {
        Ok(outcome) => {
            println!("{outcome}");
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("official-fight: {e:#}");
            ExitCode::FAILURE
        }
    }
}
