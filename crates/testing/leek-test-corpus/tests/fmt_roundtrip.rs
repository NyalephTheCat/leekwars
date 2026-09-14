//! The formatter, run over the whole upstream corpus.
//!
//! `leek-fmt`'s own fixtures are nine curated files; this is the only
//! place the formatter meets thousands of real LeekScript programs. For
//! each one we check the two properties a formatter must have:
//!
//! 1. `format_source_checked` succeeds — the output keeps every comment
//!    and token of the input, parses as cleanly, and has the same tree
//!    shape (see `leek_fmt::equivalence`);
//! 2. formatting is idempotent — a second run is a no-op.
//!
//! The first run of this test found 289 of 11005 corpus cases and 16 of 101
//! AI files failing, including several where a keyword is emitted glued to
//! the next token (`if` + `x` relexes as the identifier `ifx`) — the
//! formatter silently changing what a program means. Those were tracked one
//! row at a time in `data/fmt-known-failures-<suite>.tsv`, and each suite
//! gated on the **diff** against its own file.
//!
//! Neither file exists any more. #412/#413/#414, #416/#418 and
//! #415/#417/#419/#420 between them took the corpus list from 289 rows to
//! none and the AI list from 16 to none, and a ratchet holding nothing gates
//! on nothing (see `leek_test_corpus::fmt_ratchet`). So both suites are back
//! to the plain assertion a ratchet is only ever a detour from: every corpus
//! case and every upstream `.leek` file formats safely and idempotently, and
//! anything that stops doing so is a regression to fix — not a row to add.
//!
//! The ratchet machinery itself stays in `leek_test_corpus::fmt_ratchet`,
//! with its own tests, for the next defect class too large to fix in the
//! change that finds it.
//!
//! Runs under `.github/workflows/corpus.yml`, which checks out the
//! upstream submodule; `cargo test --workspace` in ci.yml excludes this
//! crate.

use std::path::{Path, PathBuf};

use leek_fmt::{FormatOptions, format_source_checked};
use leek_span::SourceId;
use leek_syntax::Version;
use leek_test_corpus::{embedded_manifest, first_difference, upstream_fixtures_dir};

/// How many ids to name per bucket before summarizing.
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

/// Format `src` twice; describe the failure if either run is unsafe or the
/// second run changes the first's output.
fn check(src: &str, version: Version) -> Option<String> {
    let once = match fmt(src, version) {
        Ok(text) => text,
        Err(e) => return Some(format!("unsafe to format: {e}")),
    };
    match fmt(&once, version) {
        Err(e) => Some(format!("unsafe to re-format: {e}")),
        Ok(twice) if twice != once => Some(format!(
            "not idempotent ({})\n--- once ---\n{once}--- twice ---\n{twice}",
            first_difference(&once, &twice),
        )),
        Ok(_) => None,
    }
}

/// Assert every input formats cleanly, with no tolerated failures.
///
/// The plain assertion a suite goes back to once its known-failures file
/// has been worked off entirely. A ratchet holding nothing gates on
/// nothing, so keeping an empty file around would look like a guard while
/// being none — see [`leek_test_corpus::fmt_ratchet`].
fn assert_all_clean(suite: &str, what: &str, inputs: &[(String, String, Version)]) {
    assert!(
        !inputs.is_empty(),
        "the {suite} suite ran zero inputs; an empty run gates on nothing",
    );

    let failures: Vec<(String, String)> = inputs
        .iter()
        .filter_map(|(id, src, version)| check(src, *version).map(|f| (id.clone(), f)))
        .collect();

    assert!(
        failures.is_empty(),
        "{} of {} {what} fail to format safely or idempotently.\n\nThis suite has no \
         known-failures file: every one of these was passing, so each is a formatter \
         regression to fix.\n\n{}",
        failures.len(),
        inputs.len(),
        summarize(failures.iter().map(|(id, f)| format!("{id}: {f}")))
    );
}

/// Join at most [`REPORTED`] items, then say how many were elided.
fn summarize<T: std::fmt::Display>(items: impl IntoIterator<Item = T>) -> String {
    let all: Vec<String> = items.into_iter().map(|i| i.to_string()).collect();
    let shown = all
        .iter()
        .take(REPORTED)
        .cloned()
        .collect::<Vec<_>>()
        .join("\n\n");
    if all.len() > REPORTED {
        format!("{shown}\n\n… and {} more", all.len() - REPORTED)
    } else {
        shown
    }
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
    let inputs: Vec<(String, String, Version)> = manifest
        .cases
        .iter()
        .map(|c| (c.id.clone(), c.code.clone(), Version::from_byte(c.version)))
        .collect();
    // The corpus known-failures file was worked off to nothing by
    // #415/#417/#419/#420 on top of #412/#413/#414 and #416/#418, so it is
    // gone and this is a plain assertion.
    assert_all_clean("corpus", "corpus case(s)", &inputs);
}

#[test]
fn formatting_every_upstream_ai_file_is_safe_and_idempotent() {
    if embedded_manifest().cases.is_empty() {
        eprintln!("skipping: the upstream submodule is not checked out");
        return;
    }
    let dir = upstream_fixtures_dir();
    let inputs: Vec<(String, String, Version)> = leek_files(&dir)
        .iter()
        .map(|path| {
            let src = std::fs::read_to_string(path)
                .unwrap_or_else(|e| panic!("reading {}: {e}", path.display()));
            // Ids are relative to the fixtures dir: the absolute path
            // carries the checkout root, which differs between a developer's
            // machine and the CI runner, so a tracked file keyed on it would
            // match nothing in CI.
            let id = path
                .strip_prefix(&dir)
                .unwrap_or(path)
                .to_string_lossy()
                .replace('\\', "/");
            // The upstream AI files carry no `@version` pragma; they are
            // written for the latest language version.
            (id, src, Version::LATEST)
        })
        .collect();
    // Likewise for the AI suite: its last three rows went with those fixes.
    assert_all_clean("ai", "upstream AI file(s)", &inputs);
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
