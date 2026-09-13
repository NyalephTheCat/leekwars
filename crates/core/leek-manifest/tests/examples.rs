//! Every `Miku.toml` shipped under `examples/` must parse cleanly — no
//! error and no warning — so the examples can't drift from the schema
//! (e.g. keep declaring a backend that was removed).

use std::path::{Path, PathBuf};

fn examples_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../examples")
}

fn collect_manifests(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_manifests(&path, out);
        } else if path.file_name().is_some_and(|n| n == "Miku.toml") {
            out.push(path);
        }
    }
}

#[test]
fn every_example_manifest_parses_without_warnings() {
    let dir = examples_dir();
    let mut manifests = Vec::new();
    collect_manifests(&dir, &mut manifests);
    manifests.sort();
    assert!(
        !manifests.is_empty(),
        "no Miku.toml found under {}",
        dir.display()
    );

    let mut problems = Vec::new();
    for path in &manifests {
        match leek_manifest::load_from(path) {
            Ok(load) => {
                for w in &load.warnings {
                    problems.push(format!("{}: warning: {w}", path.display()));
                }
                let entry = load.root.join(&load.manifest.project.entry);
                if !entry.is_file() {
                    problems.push(format!(
                        "{}: project.entry `{}` does not exist",
                        path.display(),
                        entry.display()
                    ));
                }
                if let Some(scenario) = &load.manifest.fight.default_scenario
                    && !load.root.join(scenario).is_file()
                {
                    problems.push(format!(
                        "{}: fight.default_scenario `{}` does not exist",
                        path.display(),
                        scenario.display()
                    ));
                }
            }
            Err(e) => problems.push(format!("{}: error: {e}", path.display())),
        }
    }
    assert!(problems.is_empty(), "{}", problems.join("\n"));
}
