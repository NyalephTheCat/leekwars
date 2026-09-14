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
//! formatter silently changing what a program means. Those are open bugs,
//! tracked one row at a time in `data/fmt-known-failures-*.tsv`, and this
//! test gates on the **diff** against those files: a case that is broken and
//! not listed fails the build; a listed case that now passes is reported so
//! the list can shrink. See `leek_test_corpus::fmt_ratchet` for why the gate
//! is shaped that way and why an empty list is an error rather than a pass.
//!
//! Regenerate both files after fixing something (needs the upstream
//! submodule):
//!
//! ```text
//! LEEK_FMT_WRITE_KNOWN_FAILURES=1 cargo test -p leek-test-corpus --test fmt_roundtrip
//! ```
//!
//! Runs under `.github/workflows/corpus.yml`, which checks out the
//! upstream submodule; `cargo test --workspace` in ci.yml excludes this
//! crate.

use std::path::{Path, PathBuf};

use leek_fmt::{FormatOptions, format_source_checked};
use leek_span::SourceId;
use leek_syntax::Version;
use leek_test_corpus::{
    FmtFailure, FmtRatchet, KIND_NOT_IDEMPOTENT, KIND_UNSAFE, KIND_UNSAFE_REFORMAT,
    diff_fmt_ratchet, embedded_manifest, first_difference, fmt_known_failures_path,
    upstream_fixtures_dir,
};

/// How many ids to name per bucket before summarizing.
const REPORTED: usize = 20;

/// Set to regenerate the tracked known-failures files instead of gating.
const WRITE_ENV: &str = "LEEK_FMT_WRITE_KNOWN_FAILURES";

/// The command the file header tells you to run. Kept next to [`WRITE_ENV`]
/// so the two cannot drift.
const REGENERATE: &str =
    "LEEK_FMT_WRITE_KNOWN_FAILURES=1 cargo test -p leek-test-corpus --test fmt_roundtrip";

fn fmt(src: &str, version: Version) -> Result<String, String> {
    format_source_checked(
        src,
        SourceId::new(1).expect("1 is a valid source id"),
        version,
        &FormatOptions::default(),
    )
    .map_err(|e| e.to_string())
}

/// One failing id: the row that goes in the tracked file, plus the full
/// diagnostic (whole outputs for a non-idempotent pair) that only a *new*
/// failure is worth printing.
struct Failure {
    row: FmtFailure,
    verbose: String,
}

/// Format `src` twice; describe the failure if either run is unsafe or the
/// second run changes the first's output.
fn check(src: &str, version: Version) -> Option<Failure> {
    let once = match fmt(src, version) {
        Ok(text) => text,
        Err(e) => {
            return Some(Failure {
                row: FmtFailure {
                    kind: KIND_UNSAFE.to_string(),
                    detail: e.clone(),
                },
                verbose: format!("unsafe to format: {e}"),
            });
        }
    };
    match fmt(&once, version) {
        Err(e) => Some(Failure {
            row: FmtFailure {
                kind: KIND_UNSAFE_REFORMAT.to_string(),
                detail: e.clone(),
            },
            verbose: format!("unsafe to re-format: {e}"),
        }),
        Ok(twice) if twice != once => Some(Failure {
            row: FmtFailure {
                kind: KIND_NOT_IDEMPOTENT.to_string(),
                detail: first_difference(&once, &twice),
            },
            verbose: format!("not idempotent\n--- once ---\n{once}--- twice ---\n{twice}"),
        }),
        Ok(_) => None,
    }
}

/// Run `check` over every `(id, source, version)` and gate the result
/// against the suite's tracked known-failures file.
///
/// `suite` names both the file (`data/fmt-known-failures-<suite>.tsv`) and
/// the run in the failure message.
fn gate(suite: &str, what: &str, inputs: &[(String, String, Version)]) {
    assert!(
        !inputs.is_empty(),
        "the {suite} suite ran zero inputs; an empty run gates on nothing",
    );

    let mut failures: Vec<(String, Failure)> = Vec::new();
    for (id, src, version) in inputs {
        if let Some(f) = check(src, *version) {
            failures.push((id.clone(), f));
        }
    }
    let current = FmtRatchet::from_failures(
        inputs.len(),
        failures.iter().map(|(id, f)| (id.clone(), f.row.clone())),
    );

    let path = fmt_known_failures_path(suite);
    if std::env::var_os(WRITE_ENV).is_some() {
        std::fs::write(&path, current.to_tsv(what, REGENERATE))
            .unwrap_or_else(|e| panic!("writing {}: {e}", path.display()));
        eprintln!(
            "wrote {} ({} of {} {what} failing)",
            path.display(),
            current.entries.len(),
            inputs.len(),
        );
        return;
    }

    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| {
        panic!(
            "{} is missing ({e}). It is the tracked list of formatter bugs this \
             suite still tolerates; without it the gate measures nothing. Restore it \
             from git, or regenerate with `{REGENERATE}`.",
            path.display(),
        )
    });
    let committed =
        FmtRatchet::parse(&text).unwrap_or_else(|e| panic!("parsing {}: {e:#}", path.display()));
    committed
        .check_gateable(&path)
        .unwrap_or_else(|e| panic!("{e:#}"));
    committed
        .check_comparable(inputs.len())
        .unwrap_or_else(|e| panic!("{e:#}"));

    let diff = diff_fmt_ratchet(&current, &committed);

    if !diff.fixed_ids.is_empty() {
        eprintln!(
            "\n{} {what} listed in {} now format cleanly — drop them from the file \
             (`{REGENERATE}`):\n{}",
            diff.fixed_ids.len(),
            path.display(),
            summarize(diff.fixed_ids.iter().map(String::as_str)),
        );
    }
    if !diff.changed_detail.is_empty() {
        eprintln!(
            "\n{} {what} still fail with a different message (informational):\n{}",
            diff.changed_detail.len(),
            summarize(
                diff.changed_detail
                    .iter()
                    .map(|(id, was, now)| format!("{id}: {was}  ->  {now}"))
            ),
        );
    }

    let verbose: std::collections::BTreeMap<&str, &str> = failures
        .iter()
        .map(|(id, f)| (id.as_str(), f.verbose.as_str()))
        .collect();
    assert!(
        !diff.is_regression(),
        "{} of {} {what} newly fail to format safely or idempotently, and are not in \
         {}.\n\nThis is a formatter regression: fix it, do not add the ids to the \
         file.\n\n{}",
        diff.new_ids.len(),
        inputs.len(),
        path.display(),
        summarize(
            diff.new_ids
                .iter()
                .map(|(id, _)| { format!("{id}: {}", verbose.get(id.as_str()).unwrap_or(&"")) })
        ),
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
    gate("corpus", "corpus case(s)", &inputs);
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
    gate("ai", "upstream AI file(s)", &inputs);
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
