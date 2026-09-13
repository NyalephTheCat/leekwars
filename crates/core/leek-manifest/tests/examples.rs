//! The manifests shipped under `examples/` have to keep parsing cleanly —
//! they double as the worked documentation for the schema.

use std::path::PathBuf;

fn example_manifest(rel: &str) -> leek_manifest::ManifestLoad {
    let path = PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../../../examples")).join(rel);
    leek_manifest::load_from(&path).expect("example manifest should load")
}

#[test]
fn fight_example_manifest_parses() {
    let load = example_manifest("fight/Miku.toml");
    let fight = &load.manifest.fight;

    assert_eq!(fight.default_scenario, Some(PathBuf::from("duel.toml")));
    assert_eq!(fight.reports_dir, PathBuf::from("build/fight-reports"));
    assert_eq!(fight.jobs, Some(4));

    // No key of the `[fight]` table may be unknown to this toolchain.
    let fight_warnings: Vec<&str> = load
        .warnings
        .iter()
        .map(|w| w.message.as_str())
        .filter(|m| m.contains("fight."))
        .collect();
    assert!(
        fight_warnings.is_empty(),
        "unexpected warnings: {fight_warnings:?}"
    );
}
