//! Conformance suite: replay every `tools/fight-harness/corpus.txt` mirror
//! fight through the `official-fight` bin and diff its Outcome JSON against
//! the Java generator's golden (`tools/fight-harness/goldens/`), ignoring
//! the runtime-measurement fields (`ops`, `execution_time`) exactly like
//! `check-conformance.sh` / `diff-outcome.py --ignore-ops` do.
//!
//! The `.leek` AIs in `tools/fight-harness/examples/` are the single source
//! of truth — both the Java harness and this test compile and run the same
//! files. Regenerate goldens with `tools/fight-harness/gen-goldens.sh`.

use std::path::{Path, PathBuf};
use std::process::Command;

fn harness_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../tools/fight-harness")
}

/// Strip the runtime-measurement fields the diff ignores (`--ignore-ops`).
fn strip_volatile(v: &mut serde_json::Value) {
    match v {
        serde_json::Value::Object(obj) => {
            obj.remove("ops");
            obj.remove("execution_time");
            for child in obj.values_mut() {
                strip_volatile(child);
            }
        }
        serde_json::Value::Array(arr) => {
            for child in arr {
                strip_volatile(child);
            }
        }
        _ => {}
    }
}

/// Blank the trace element of `logs.<farmer>.<action>` system-log entries
/// (`[fid, type, trace, key, params?]`) — the trace renders Java's own AI
/// call stack (codegen line numbers), which the Rust simulator can't
/// reproduce. Mirrors `diff-outcome.py`'s `normalize_log_traces`.
fn normalize_log_traces(v: &mut serde_json::Value) {
    let Some(farmers) = v.get_mut("logs").and_then(|l| l.as_object_mut()) else {
        return;
    };
    for groups in farmers.values_mut() {
        let Some(groups) = groups.as_object_mut() else {
            continue;
        };
        for entries in groups.values_mut() {
            let Some(entries) = entries.as_array_mut() else {
                continue;
            };
            for entry in entries {
                if let Some(entry) = entry.as_array_mut()
                    && entry.len() >= 4
                    && entry[2].is_string()
                {
                    entry[2] = serde_json::Value::String(String::new());
                }
            }
        }
    }
}

#[test]
fn corpus_matches_official_goldens() {
    let dir = harness_dir();
    let corpus = std::fs::read_to_string(dir.join("corpus.txt")).expect("reading corpus.txt");
    let mut checked = 0;
    let mut failures = Vec::new();
    for line in corpus.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut cols = line.split_whitespace();
        let (Some(name), Some(ai), Some(seed)) = (cols.next(), cols.next(), cols.next()) else {
            panic!("malformed corpus.txt line: {line:?}");
        };
        let golden_path = dir.join("goldens").join(format!("{name}.json"));
        let golden = std::fs::read_to_string(&golden_path)
            .unwrap_or_else(|e| panic!("{}: {e} — run gen-goldens.sh", golden_path.display()));
        let ai_path = dir.join("examples").join(ai);
        let out = Command::new(env!("CARGO_BIN_EXE_official-fight"))
            .args([&ai_path, &ai_path])
            .arg(seed)
            .output()
            .expect("spawning official-fight");
        if !out.status.success() {
            failures.push(format!(
                "{name}: official-fight errored: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            ));
            continue;
        }
        let mut ours: serde_json::Value =
            serde_json::from_slice(&out.stdout).expect("official-fight output is JSON");
        let mut gold: serde_json::Value = serde_json::from_str(&golden).expect("golden is JSON");
        strip_volatile(&mut ours);
        strip_volatile(&mut gold);
        normalize_log_traces(&mut ours);
        normalize_log_traces(&mut gold);
        if ours != gold {
            failures.push(format!(
                "{name}: diverges from the golden — run \
                 `tools/fight-harness/check-conformance.sh {name}` for the decoded diff"
            ));
        }
        checked += 1;
    }
    assert!(checked > 0, "corpus.txt has no entries");
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
