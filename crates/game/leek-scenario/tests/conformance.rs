//! Conformance suite: replay every `tools/fight-harness/corpus.txt` mirror
//! fight through the `official-fight` bin and diff its Outcome JSON against
//! the Java generator's golden (`tools/fight-harness/goldens/`), ignoring
//! the runtime-measurement fields (`ops`, `execution_time`) exactly like
//! `check-conformance.sh` / `diff-outcome.py --ignore-ops` do.
//!
//! The `.leek` AIs in `tools/fight-harness/examples/` are the single source
//! of truth — both the Java harness and this test compile and run the same
//! files. Regenerate goldens with `tools/fight-harness/gen-goldens.sh`.
//!
//! Every entry is a whole fight in its own child process, so they run across
//! a [`Pool`] (#146) and the failures are merged back in corpus order — the
//! list a run prints must not depend on which fight finished first. A
//! divergence names the first differing JSON pointer and both sides, so a
//! failing run says *what* drifted without a second trip through the decode
//! script.

use std::path::{Path, PathBuf};
use std::process::Command;

use leek_workpool::Pool;
use serde_json::Value;

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

/// One value on one line, short enough that a failure message stays readable:
/// an action log or an effect table runs to thousands of characters, and the
/// point of the message is the pointer, not the payload.
fn render(v: &Value) -> String {
    const MAX: usize = 120;
    let s = v.to_string();
    match s.char_indices().nth(MAX) {
        Some((cut, _)) => format!("{}…", &s[..cut]),
        None => s,
    }
}

/// Append one JSON-pointer step, escaped per RFC 6901, so the pointer can be
/// pasted into something that takes one.
fn push_step(path: &mut String, step: &str) {
    path.push('/');
    for c in step.chars() {
        match c {
            '~' => path.push_str("~0"),
            '/' => path.push_str("~1"),
            _ => path.push(c),
        }
    }
}

/// The first place `a` and `b` differ: the JSON pointer to it plus both sides
/// [`render`]ed — or `None` when the two are equal.
///
/// `path` is the pointer to the pair being compared (empty at the root) and
/// is left as it was found, so one buffer serves the whole walk. Objects are
/// walked in key order and arrays in index order, so the same pair of
/// documents always reports the same pointer.
fn first_difference(a: &Value, b: &Value, path: &mut String) -> Option<String> {
    match (a, b) {
        (Value::Object(a_fields), Value::Object(b_fields)) => {
            for (key, a_child) in a_fields {
                let here = path.len();
                push_step(path, key);
                let found = match b_fields.get(key) {
                    Some(b_child) => first_difference(a_child, b_child, path),
                    None => Some(format!("{path}: {} vs absent", render(a_child))),
                };
                if found.is_some() {
                    return found;
                }
                path.truncate(here);
            }
            let (key, b_child) = b_fields
                .iter()
                .find(|(key, _)| !a_fields.contains_key(key.as_str()))?;
            push_step(path, key);
            Some(format!("{path}: absent vs {}", render(b_child)))
        }
        (Value::Array(a_items), Value::Array(b_items)) => {
            // The common prefix first: when a fight diverges mid-log both the
            // element and the length differ, and it is the element that names
            // the subsystem at fault.
            for (i, (a_child, b_child)) in a_items.iter().zip(b_items).enumerate() {
                let here = path.len();
                push_step(path, &i.to_string());
                if let Some(found) = first_difference(a_child, b_child, path) {
                    return Some(found);
                }
                path.truncate(here);
            }
            (a_items.len() != b_items.len())
                .then(|| format!("{path}: {} elements vs {}", a_items.len(), b_items.len()))
        }
        _ => (a != b).then(|| format!("{path}: {} vs {}", render(a), render(b))),
    }
}

/// Replay one corpus entry and compare it against its golden. The error is
/// the line the failure list prints, so it names the entry itself — the
/// entries are played out of order, and a bare message would be
/// unattributable.
fn check_entry(name: &str, ai: &str, seed: &str) -> Result<(), String> {
    let dir = harness_dir();
    let golden_path = dir.join("goldens").join(format!("{name}.json"));
    let golden = std::fs::read_to_string(&golden_path)
        .map_err(|e| format!("{}: {e} — run gen-goldens.sh", golden_path.display()))?;
    let ai_path = dir.join("examples").join(ai);
    let out = Command::new(env!("CARGO_BIN_EXE_official-fight"))
        .args([&ai_path, &ai_path])
        .arg(seed)
        .output()
        .map_err(|e| format!("{name}: spawning official-fight: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "{name}: official-fight errored: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    let mut ours: Value = serde_json::from_slice(&out.stdout)
        .map_err(|e| format!("{name}: official-fight output is not JSON: {e}"))?;
    let mut gold: Value = serde_json::from_str(&golden)
        .map_err(|e| format!("{}: not JSON: {e}", golden_path.display()))?;
    strip_volatile(&mut ours);
    strip_volatile(&mut gold);
    normalize_log_traces(&mut ours);
    normalize_log_traces(&mut gold);
    if ours == gold {
        return Ok(());
    }
    let mut path = String::new();
    let at = first_difference(&ours, &gold, &mut path)
        .unwrap_or_else(|| "a pointer `first_difference` doesn't reach".to_string());
    Err(format!(
        "{name}: diverges from the golden (ours vs golden) at {at} — run \
         `tools/fight-harness/check-conformance.sh {name}` for the decoded diff"
    ))
}

#[test]
fn corpus_matches_official_goldens() {
    let dir = harness_dir();
    let corpus = std::fs::read_to_string(dir.join("corpus.txt")).expect("reading corpus.txt");
    let entries: Vec<(&str, &str, &str)> = corpus
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(|line| {
            let mut cols = line.split_whitespace();
            let (Some(name), Some(ai), Some(seed)) = (cols.next(), cols.next(), cols.next()) else {
                panic!("malformed corpus.txt line: {line:?}");
            };
            (name, ai, seed)
        })
        .collect();
    assert!(!entries.is_empty(), "corpus.txt has no entries");

    // Each entry is a fight in its own child process, so nothing is shared;
    // `map` hands the failures back in index — corpus — order.
    let indices: Vec<usize> = (0..entries.len()).collect();
    let failures = Pool::new("conformance", leek_scenario::default_jobs()).map(
        &indices,
        || (),
        |i, ()| {
            let (name, ai, seed) = entries[i];
            check_entry(name, ai, seed).err()
        },
    );

    let report: Vec<&str> = failures.iter().map(|(_, msg)| msg.as_str()).collect();
    assert!(report.is_empty(), "{}", report.join("\n"));
}

/// The localiser is only exercised by a *failing* conformance run, which is
/// exactly when nobody wants to debug it — so it is pinned here instead.
mod difference {
    use super::first_difference;
    use serde_json::json;

    fn diff(a: &serde_json::Value, b: &serde_json::Value) -> Option<String> {
        first_difference(a, b, &mut String::new())
    }

    #[test]
    fn equal_documents_have_no_difference() {
        let v = json!({"logs": {"1": {"say": [[1, 203, "trace", "k"]]}}, "actions": [[6, 1]]});
        assert_eq!(diff(&v, &v.clone()), None);
    }

    #[test]
    fn a_scalar_deep_in_the_tree_is_reported_by_pointer() {
        let a = json!({"actions": [[6, 1], [101, 1, 2, 47]]});
        let b = json!({"actions": [[6, 1], [101, 1, 2, 52]]});
        assert_eq!(diff(&a, &b).as_deref(), Some("/actions/1/3: 47 vs 52"));
    }

    #[test]
    fn a_differing_element_beats_the_length_it_comes_with() {
        // A fight that diverges mid-log is both wrong *and* short: the
        // element names the subsystem, the length only says "shorter".
        let a = json!({"actions": [[6, 1], [10, 1, 200]]});
        let b = json!({"actions": [[6, 1], [10, 1, 201], [8, 1]]});
        assert_eq!(diff(&a, &b).as_deref(), Some("/actions/1/2: 200 vs 201"));
    }

    #[test]
    fn a_shared_prefix_leaves_only_the_length_to_report() {
        let a = json!({"actions": [[6, 1]]});
        let b = json!({"actions": [[6, 1], [8, 1]]});
        assert_eq!(diff(&a, &b).as_deref(), Some("/actions: 1 elements vs 2"));
    }

    #[test]
    fn a_key_missing_on_either_side_is_reported_at_its_own_pointer() {
        let a = json!({"winner": 1, "ours_only": 7});
        let b = json!({"winner": 1});
        assert_eq!(diff(&a, &b).as_deref(), Some("/ours_only: 7 vs absent"));
        assert_eq!(diff(&b, &a).as_deref(), Some("/ours_only: absent vs 7"));
    }

    #[test]
    fn a_type_change_is_a_difference_like_any_other() {
        let a = json!({"leeks": {"1": {"life": 100}}});
        let b = json!({"leeks": {"1": {"life": [100]}}});
        assert_eq!(diff(&a, &b).as_deref(), Some("/leeks/1/life: 100 vs [100]"));
    }

    #[test]
    fn a_slash_in_a_key_is_escaped_into_the_pointer() {
        let a = json!({"a/b": 1, "c~d": 1});
        let b = json!({"a/b": 2, "c~d": 2});
        assert_eq!(diff(&a, &b).as_deref(), Some("/a~1b: 1 vs 2"));
        assert_eq!(
            diff(&json!({"c~d": 1}), &json!({"c~d": 2})).as_deref(),
            Some("/c~0d: 1 vs 2")
        );
    }

    #[test]
    fn a_long_value_is_truncated_on_both_sides() {
        let a = json!({"log": "x".repeat(400)});
        let b = json!({"log": "y".repeat(400)});
        let reported = diff(&a, &b).expect("differs");
        assert!(reported.starts_with("/log: \"xxx"), "{reported}");
        assert!(reported.contains('…'), "{reported}");
        // Pointer, both sides, two ellipses — nowhere near the raw 800 chars.
        assert!(reported.chars().count() < 300, "{reported}");
    }
}
