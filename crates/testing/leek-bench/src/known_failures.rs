//! Serialising and ratcheting the fast rust-java sweep's failures.
//!
//! [`run_fast_java_corpus`](crate::run_fast_java_corpus) returns a
//! [`FastReport`] whose `failures` list lives only in memory: the sweep prints
//! aggregate counts and exits, so "150 wrong values, 49 compile/emit errors"
//! is the whole signal. A fix in one case and a regression in another net to
//! zero and nobody notices (#107).
//!
//! This module turns that list into a **tracked, diffable file**: one sorted
//! row per failing case, with a detail string chosen to be stable across runs
//! and machines. `leekbench --corpus --fast-java --write-known-failures`
//! writes it; `--check-known-failures` diffs the current sweep against the
//! committed file and fails on ids that are newly broken.
//!
//! Two deliberate weaknesses in the gate, both because the alternative is a
//! flaky red build rather than a stronger check:
//!
//! - Only **new ids** gate. A reworded `javac` message (a JDK bump can reword
//!   hundreds at once) lands in [`KnownFailuresDiff::changed_detail`], which
//!   is informational.
//! - `timeout` and `no-result` rows never gate. `BatchRunner`'s per-case
//!   timeout only *interrupts*, and a CPU-bound Leekscript loop never checks
//!   interrupts (#297), so a hung case keeps a core and can push later correct
//!   cases past the budget — which bucket a case lands in then depends on case
//!   order and machine speed. They are reported as
//!   [`KnownFailuresDiff::flaky_new_ids`] instead.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use anyhow::{Result, bail};

use crate::corpus_fast::{FastOutcome, FastReport};

/// Kind tag written in column 2. Kept as strings rather than an enum because
/// the file has to stay readable by a version that added a kind.
pub const KIND_DISAGREE: &str = "disagree";
/// See [`KIND_DISAGREE`].
pub const KIND_COMPILE_ERROR: &str = "compile-error";
/// See [`KIND_DISAGREE`].
pub const KIND_EMIT_ERROR: &str = "emit-error";
/// See [`KIND_DISAGREE`].
pub const KIND_RUNTIME_ERROR: &str = "runtime-error";
/// See [`KIND_DISAGREE`].
pub const KIND_TIMEOUT: &str = "timeout";
/// See [`KIND_DISAGREE`].
pub const KIND_NO_RESULT: &str = "no-result";

/// Whether a kind is excluded from the gate. See the module docs: these two
/// move run to run for reasons that are not the compiler's fault (#297).
#[must_use]
pub fn kind_is_flaky(kind: &str) -> bool {
    matches!(kind, KIND_TIMEOUT | KIND_NO_RESULT)
}

/// One row of the tracked file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KnownFailure {
    pub kind: String,
    pub detail: String,
}

/// The parsed contents of a known-failures file.
#[derive(Debug, Clone, Default)]
pub struct KnownFailures {
    /// `# total=<n>` header — how many cases the sweep that wrote this file
    /// covered. Checking a truncated sweep against a full-corpus file would
    /// report every unrun case as "fixed", so the header is what lets
    /// [`KnownFailures::check_comparable`] refuse the comparison.
    pub total: Option<usize>,
    /// Failing case id → row, sorted by id.
    pub entries: BTreeMap<String, KnownFailure>,
}

impl KnownFailures {
    /// Refuse to diff two sweeps of visibly different size. 1% of slack
    /// absorbs a corpus that gained or lost a handful of cases upstream; a
    /// `--limit`-truncated run is orders of magnitude off and is rejected.
    pub fn check_comparable(&self, current_total: usize) -> Result<()> {
        let Some(committed) = self.total else {
            return Ok(());
        };
        let slack = (committed / 100).max(1);
        if current_total.abs_diff(committed) > slack {
            bail!(
                "this sweep ran {current_total} cases but the known-failures file was written \
                 from {committed}; a partial sweep would report every unrun case as fixed. \
                 Run the full sweep, or regenerate with --write-known-failures."
            );
        }
        Ok(())
    }
}

/// What changed between the current sweep and the committed file.
#[derive(Debug, Clone, Default)]
pub struct KnownFailuresDiff {
    /// Cases that fail now and are not in the file. **This is the gate.**
    pub new_ids: Vec<(String, KnownFailure)>,
    /// Newly failing, but in a bucket too unstable to gate on (#297).
    pub flaky_new_ids: Vec<(String, KnownFailure)>,
    /// In the file, passing now — tighten the file.
    pub fixed_ids: Vec<String>,
    /// Same id, different row: `(id, committed, current)`. Informational — a
    /// `javac` reword must not be a red gate.
    pub changed_detail: Vec<(String, String, String)>,
}

impl KnownFailuresDiff {
    /// The gate. Only newly broken, non-flaky ids make the sweep fail.
    #[must_use]
    pub fn is_regression(&self) -> bool {
        !self.new_ids.is_empty()
    }
}

/// Diff a fresh sweep against the committed file.
#[must_use]
pub fn diff_known_failures(
    current: &KnownFailures,
    committed: &KnownFailures,
) -> KnownFailuresDiff {
    let mut diff = KnownFailuresDiff::default();
    for (id, now) in &current.entries {
        match committed.entries.get(id) {
            None => {
                if kind_is_flaky(&now.kind) {
                    diff.flaky_new_ids.push((id.clone(), now.clone()));
                } else {
                    diff.new_ids.push((id.clone(), now.clone()));
                }
            }
            Some(was) if was != now => {
                diff.changed_detail
                    .push((id.clone(), describe(was), describe(now)));
            }
            Some(_) => {}
        }
    }
    for id in committed.entries.keys() {
        if !current.entries.contains_key(id) {
            diff.fixed_ids.push(id.clone());
        }
    }
    diff
}

/// Human-readable one-liner for a row.
#[must_use]
pub fn describe(row: &KnownFailure) -> String {
    if row.detail.is_empty() {
        row.kind.clone()
    } else {
        format!("{} {}", row.kind, row.detail)
    }
}

/// Parse a known-failures file. Blank lines are skipped; `#` lines are
/// comments, except the `# total=<n>` header.
pub fn parse_known_failures(text: &str) -> Result<KnownFailures> {
    let mut out = KnownFailures::default();
    for (n, line) in text.lines().enumerate() {
        let line = line.trim_end_matches('\r');
        if line.trim().is_empty() {
            continue;
        }
        if let Some(rest) = line.strip_prefix('#') {
            if let Some(total) = rest.trim().strip_prefix("total=") {
                out.total = total.trim().parse::<usize>().ok();
            }
            continue;
        }
        let mut it = line.splitn(3, '\t');
        let (Some(id), Some(kind)) = (it.next(), it.next()) else {
            bail!(
                "known-failures line {}: expected `id<TAB>kind<TAB>detail`",
                n + 1
            );
        };
        let detail = it.next().unwrap_or("");
        out.entries.insert(
            unescape(id),
            KnownFailure {
                kind: kind.to_string(),
                detail: unescape(detail),
            },
        );
    }
    Ok(out)
}

impl FastReport {
    /// The tracked-file view of this sweep: one row per failing case.
    ///
    /// Keyed by id in a `BTreeMap` so the file is a stable diff, and so two
    /// runs of the same corpus produce byte-identical output despite
    /// `failures` being built in emit/compile/run phase order.
    #[must_use]
    pub fn to_known_failures(&self) -> KnownFailures {
        let mut entries = BTreeMap::new();
        for (id, outcome) in &self.failures {
            entries.insert(id.clone(), row_for(outcome));
        }
        KnownFailures {
            total: Some(self.total),
            entries,
        }
    }

    /// Serialise [`Self::to_known_failures`] as TSV.
    #[must_use]
    pub fn to_tsv(&self) -> String {
        let known = self.to_known_failures();
        let mut out = String::new();
        out.push_str("# leekbench --corpus --fast-java known failures\n");
        out.push_str(
            "# regenerate with: leekbench --corpus --fast-java --limit 100000 \
             --write-known-failures\n",
        );
        let _ = writeln!(out, "# total={}", self.total);
        for (id, row) in &known.entries {
            out.push_str(&escape(id));
            out.push('\t');
            out.push_str(&row.kind);
            out.push('\t');
            out.push_str(&escape(&row.detail));
            out.push('\n');
        }
        out
    }

    /// Failure signatures by frequency, commonest first (ties broken by
    /// signature so the output is deterministic).
    ///
    /// This is the "prioritise the work" half of #107: "49 compile errors" is
    /// a number, `38 × variable __scrut is already defined in method runIA()`
    /// is a bug report.
    #[must_use]
    pub fn signature_histogram(&self) -> Vec<(String, usize)> {
        let mut counts: BTreeMap<String, usize> = BTreeMap::new();
        for (_, outcome) in &self.failures {
            *counts.entry(describe(&row_for(outcome))).or_default() += 1;
        }
        let mut out: Vec<(String, usize)> = counts.into_iter().collect();
        out.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        out
    }
}

/// The `(kind, detail)` pair a failure serialises to.
///
/// `Disagree` keeps both values because that *is* the defect. Compile errors
/// keep [`javac_signature`] rather than the raw line: the raw line carries a
/// temp path and a line number, neither of which is stable, and the signature
/// is also the grouping key the histogram needs.
fn row_for(outcome: &FastOutcome) -> KnownFailure {
    let (kind, detail) = match outcome {
        FastOutcome::Disagree { got, expected } => (KIND_DISAGREE, format!("{got} => {expected}")),
        FastOutcome::CompileError { javac } => (KIND_COMPILE_ERROR, javac_signature(javac)),
        FastOutcome::EmitError(e) => (KIND_EMIT_ERROR, normalize_ids(e)),
        FastOutcome::RuntimeError(e) => (KIND_RUNTIME_ERROR, normalize_ids(e)),
        FastOutcome::Timeout => (KIND_TIMEOUT, String::new()),
        FastOutcome::NoResult => (KIND_NO_RESULT, String::new()),
    };
    KnownFailure {
        kind: kind.to_string(),
        detail,
    }
}

/// Reduce one `javac` error line to a machine-stable grouping key.
///
/// `javac` reports `<tmp>/AI_42.java:17: error: variable __scrut is already
/// defined in method runIA()`. The path is a per-pid temp dir, the line number
/// moves whenever the emitter's output shifts, and `AI_42` / `u_7` are
/// per-case identifiers — so the raw line is unique per case and groups
/// nothing. Dropping the location and `#`-ing the generated identifiers
/// collapses every case with the same defect onto one key.
#[must_use]
pub fn javac_signature(line: &str) -> String {
    let msg = match line.find("error:") {
        Some(at) => &line[at + "error:".len()..],
        // No `error:` marker (a crash dump, or an already-stripped message):
        // keep the line, still normalised, rather than losing the signal.
        None => line,
    };
    normalize_ids(msg.trim())
}

/// Rewrite generated identifiers (`AI_<n>`, `u_<n>`) to `AI_#` / `u_#`.
fn normalize_ids(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        // Only at an identifier boundary, so `menu_12` keeps its digits.
        let boundary = i == 0 || !(bytes[i - 1].is_ascii_alphanumeric() || bytes[i - 1] == b'_');
        if boundary && let Some(prefix) = ["AI_", "u_"].iter().find(|p| s[i..].starts_with(**p)) {
            let digits = s[i + prefix.len()..]
                .chars()
                .take_while(char::is_ascii_digit)
                .count();
            if digits > 0 {
                out.push_str(prefix);
                out.push('#');
                i += prefix.len() + digits;
                continue;
            }
        }
        let c = s[i..].chars().next().unwrap_or(char::REPLACEMENT_CHARACTER);
        out.push(c);
        i += c.len_utf8();
    }
    out
}

/// TSV cell escaping, matching `data/reference.tsv`'s convention (see
/// `unescape` in `leek-backend-java/tests/parity.rs`): a value may contain a
/// tab or a newline, and the file is line- and tab-delimited, so both have to
/// survive as escapes.
fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\t' => out.push_str("\\t"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            other => out.push(other),
        }
    }
    out
}

fn unescape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.next() {
                Some('n') => out.push('\n'),
                Some('r') => out.push('\r'),
                Some('t') => out.push('\t'),
                Some('\\') => out.push('\\'),
                Some(other) => {
                    out.push('\\');
                    out.push(other);
                }
                None => out.push('\\'),
            }
        } else {
            out.push(c);
        }
    }
    out
}
