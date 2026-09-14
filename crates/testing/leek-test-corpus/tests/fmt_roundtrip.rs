//! The formatter, run over the whole upstream corpus.
//!
//! `leek-fmt`'s own fixtures are nine curated files; this is the only
//! place the formatter meets thousands of real LeekScript programs. For
//! each one we assert the two properties a formatter must have:
//!
//! 1. `format_source_checked` succeeds — the output keeps every comment
//!    and token of the input, parses as cleanly, and has the same tree
//!    shape (see `leek_fmt::equivalence`);
//! 2. formatting is idempotent — a second run is a no-op.
//!
//! Runs under `.github/workflows/corpus.yml`, which checks out the
//! upstream submodule; `cargo test --workspace` in ci.yml excludes this
//! crate.

use std::path::{Path, PathBuf};

use leek_fmt::{FormatOptions, format_source_checked};
use leek_span::SourceId;
use leek_syntax::Version;
use leek_test_corpus::{embedded_manifest, upstream_fixtures_dir};

/// How many failing cases to name before summarizing.
const REPORTED: usize = 20;

fn fmt(src: &str, version: Version) -> Result<String, String> {
    format_source_checked(
        src,
        SourceId::new(1).expect("1 is a valid source id"),
        version,
        &FormatOptions::default(),
    )
    .map_err(|e| e.to_string())
}

/// Format `src` twice; return a failure description if either run is
/// unsafe or the second run changes the first's output.
fn check(label: &str, src: &str, version: Version) -> Option<String> {
    let once = match fmt(src, version) {
        Ok(text) => text,
        Err(e) => return Some(format!("{label}: unsafe to format: {e}")),
    };
    match fmt(&once, version) {
        Err(e) => Some(format!("{label}: unsafe to re-format: {e}")),
        Ok(twice) if twice != once => Some(format!(
            "{label}: not idempotent\n--- once ---\n{once}--- twice ---\n{twice}"
        )),
        Ok(_) => None,
    }
}

fn report(what: &str, checked: usize, failures: &[String]) {
    assert!(
        failures.is_empty(),
        "{}/{checked} {what} failed:\n\n{}{}",
        failures.len(),
        failures
            .iter()
            .take(REPORTED)
            .cloned()
            .collect::<Vec<_>>()
            .join("\n\n"),
        if failures.len() > REPORTED {
            format!("\n\n… and {} more", failures.len() - REPORTED)
        } else {
            String::new()
        },
    );
}

#[test]
fn formatting_every_corpus_case_is_safe_and_idempotent() {
    let manifest = embedded_manifest();
    if manifest.cases.is_empty() {
        // No upstream submodule at build time — `upstream_suite.rs`
        // owns failing loudly about that; nothing to format here.
        eprintln!("skipping: the embedded manifest has no cases");
        return;
    }
    let mut failures = Vec::new();
    for case in &manifest.cases {
        if let Some(f) = check(&case.id, &case.code, Version::from_byte(case.version)) {
            failures.push(f);
        }
    }
    report("corpus case(s)", manifest.cases.len(), &failures);
}

#[test]
fn formatting_every_upstream_ai_file_is_safe_and_idempotent() {
    if embedded_manifest().cases.is_empty() {
        eprintln!("skipping: the upstream submodule is not checked out");
        return;
    }
    let files = leek_files(&upstream_fixtures_dir());
    assert!(!files.is_empty(), "no .leek files under the fixtures dir");
    let mut failures = Vec::new();
    for path in &files {
        let src = std::fs::read_to_string(path)
            .unwrap_or_else(|e| panic!("reading {}: {e}", path.display()));
        // The upstream AI files carry no `@version` pragma; they are
        // written for the latest language version.
        if let Some(f) = check(&path.display().to_string(), &src, Version::LATEST) {
            failures.push(f);
        }
    }
    report("upstream AI file(s)", files.len(), &failures);
}

/// Every `.leek` file under `dir`, recursively, in a stable order.
fn leek_files(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(current) = stack.pop() {
        let entries = std::fs::read_dir(&current)
            .unwrap_or_else(|e| panic!("reading {}: {e}", current.display()));
        for entry in entries {
            let path = entry.expect("directory entry").path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|e| e == "leek") {
                out.push(path);
            }
        }
    }
    out.sort();
    out
}
