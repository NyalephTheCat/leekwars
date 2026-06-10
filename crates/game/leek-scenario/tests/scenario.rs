//! Schema, merge, and include-resolution tests. These exercise parsing and
//! composition only (no compilation), so they need no game library.

use std::path::PathBuf;

use leek_scenario::{Scenario, StatKind};

fn examples_dir() -> PathBuf {
    PathBuf::from(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../../examples/fight"
    ))
}

#[test]
fn toml_flat_entities_roundtrip() {
    let scn = Scenario::from_toml_str(
        r"
        seed = 42
        max_turns = 30

        [map]
        width = 10
        height = 10
        obstacles = [5, 6]

        [[entities]]
        id = 1
        team = 0
        cell = 0
        strength = 100

        [[entities]]
        id = 2
        team = 1
        cell = 99
        ",
    )
    .expect("parse toml");

    assert_eq!(scn.seed, Some(42));
    assert_eq!(scn.max_turns, Some(30));
    assert_eq!(scn.map.as_ref().unwrap().width, 10);
    assert_eq!(scn.entities.len(), 2);
    assert_eq!(scn.entities[0].strength, Some(100));
    assert_eq!(scn.entities[1].team, Some(1));
}

#[test]
fn json_nested_entities_flatten_with_team_from_index() {
    // The official generator's shape: `entities` is a list-of-teams and the
    // seed key is `random_seed`.
    let scn = Scenario::from_json_str(
        r#"{
            "random_seed": 7,
            "map": { "width": 17, "height": 17, "obstacles": [1, 2] },
            "farmers": [{ "id": 1, "name": "Pilow", "country": "fr" }],
            "teams": [{ "id": 1, "name": "A" }, { "id": 2, "name": "B" }],
            "entities": [
                [{ "id": 12, "cell": 0, "type": 1, "cores": 10, "strength": 500 }],
                [{ "id": 59, "cell": 288, "team": 2, "frequency": 100 }]
            ]
        }"#,
    )
    .expect("parse json");

    assert_eq!(scn.seed, Some(7));
    assert_eq!(scn.entities.len(), 2);
    // First group → team index + 1 = 1 (no explicit team).
    assert_eq!(scn.entities[0].team, Some(1));
    assert_eq!(scn.entities[0].strength, Some(500));
    // Second group keeps its explicit team. Unknown fields (`type`, `cores`,
    // `frequency`) are ignored, not errors.
    assert_eq!(scn.entities[1].team, Some(2));
    assert_eq!(scn.farmers.len(), 1);
}

#[test]
fn profile_overlay_is_sparse() {
    let mut scn = Scenario::from_toml_str(
        r"
        max_turns = 64
        [map]
        width = 10
        height = 10
        [[entities]]
        id = 1
        team = 0
        cell = 0
        strength = 100
        weapons = [37]

        [profiles.aggressive]
        max_turns = 32
        [[profiles.aggressive.entities]]
        id = 1
        strength = 900
        weapons = [47]
        ",
    )
    .unwrap();

    scn.apply_profile("aggressive").unwrap();
    assert_eq!(scn.max_turns, Some(32));
    let hero = &scn.entities[0];
    assert_eq!(hero.strength, Some(900));
    assert_eq!(hero.weapons, vec![47]);
    // Untouched fields survive the overlay.
    assert_eq!(hero.cell, Some(0));
    assert_eq!(hero.team, Some(0));
}

#[test]
fn missing_profile_errors() {
    let mut scn = Scenario::from_toml_str("[map]\nwidth=1\nheight=1\n").unwrap();
    assert!(scn.apply_profile("nope").is_err());
}

#[test]
fn extends_and_leek_refs_resolve() {
    // duel.toml `extends = base-arena.toml` and each entity `leek = leeks/*.toml`.
    let scn = Scenario::load(&examples_dir().join("duel.toml")).expect("load duel");

    // Map + seed came from the inherited base-arena.toml.
    assert_eq!(scn.map.as_ref().unwrap().width, 17);
    assert_eq!(scn.seed, Some(1_234_567));

    // The hero entity inherited stats from leeks/hero.toml, kept its own
    // id/cell/team/ai, and no longer carries a `leek` reference.
    let hero = scn.entities.iter().find(|e| e.id == Some(1)).unwrap();
    assert_eq!(hero.life, Some(3000));
    assert_eq!(hero.weapons, vec![37, 47]);
    assert_eq!(hero.cell, Some(0));
    assert_eq!(hero.team, Some(0));
    assert!(hero.leek.is_none());
    assert!(hero.ai.is_some());
}

#[test]
fn gen_build_is_deterministic_and_sums_to_capital() {
    let stats = [StatKind::Strength, StatKind::Agility, StatKind::Wisdom];
    let a = leek_scenario::build_gen::gen_build(1000, &stats, 0, 123);
    let b = leek_scenario::build_gen::gen_build(1000, &stats, 0, 123);
    let c = leek_scenario::build_gen::gen_build(1000, &stats, 0, 124);

    assert_eq!(a, b, "same seed → same build");
    assert_ne!(a, c, "different seed → different build");
    let total: i64 = a.values().sum();
    assert_eq!(total, 1000, "the whole capital is distributed");
}
