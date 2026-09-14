//! The parser's known-bad list over the fixtures upstream *enables*.
//!
//! `tests/parser_fixtures.rs` asserts that every upstream `.leek` file
//! the standalone-LeekScript JUnit suite actually runs parses with no
//! lexer, pragma or parser diagnostic at all. Two of them do not, and
//! neither is fixable in the change that scoped the gate, so they are
//! tracked here the way this repo tracks every other known-bad set: a
//! **tracked, diffable file**, one sorted row per failing fixture, with
//! the suite gating on the *diff* rather than on the list.
//!
//! The file format is deliberately the one
//! [`fmt_ratchet`](crate::fmt_ratchet) and `leek-bench`'s
//! `known_failures` already use — `# total=<n>` header, then
//! `id<TAB>kind<TAB>detail` rows, sorted by id, tabs and newlines
//! escaped — so a reader who knows one knows all three, and so the two
//! honesty checks a ratchet needs come out the same:
//!
//! - a **missing or empty** file is an error, not an empty allow-list
//!   ([`ParseRatchet::check_gateable`]). The only honest way to reach
//!   zero rows is to fix the last gap and delete the file and the
//!   ratchet together;
//! - a run covering visibly fewer ids than the file was written from is
//!   an error, because every unrun id would otherwise read as "fixed"
//!   ([`ParseRatchet::check_comparable`]).
//!
//! **Where this ratchet is stricter than the formatter's, and why.**
//! `fmt_ratchet` gates only on newly-broken ids: a fixed row is reported
//! and a reworded detail is information. That was right for 289 rows
//! whose details came out of a third party (a JDK bump rewords hundreds
//! of `javac` messages at once, and a red build teaches nobody
//! anything). Here there are two rows, and their details are this
//! toolchain's *own* diagnostics. So [`ParseRatchetDiff::is_regression`]
//! is true for all three buckets:
//!
//! - a fixture that fails and is **not** listed — a parser regression,
//!   the reason the gate exists;
//! - a **listed fixture that now parses cleanly** — the gap was closed
//!   and nobody deleted the row, so the file has started lying about
//!   what is broken. The list must only ever shrink;
//! - a listed fixture whose **diagnostics moved** — still broken, but
//!   the row no longer describes it. Since we own the message, updating
//!   it is one command.
//!
//! Every row is an open parser gap against epic R7 (#351). Nothing here
//! is accepted behaviour, and nothing here is a reason to relax the
//! check that found it.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::Path;

use anyhow::{Result, bail};

/// Longest a `detail` cell may get before it is clipped. The row is
/// meant to be read in a diff; the full diagnostics are printed by the
/// suite when it fails.
const DETAIL_MAX: usize = 120;

/// One row of the tracked file: a failing fixture's diagnostic-code
/// histogram and a stable one-line detail.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseFailure {
    /// Diagnostic codes with counts, e.g. `E0100 x16` or
    /// `E0002 x3, E0100 x4`. Column 2 is what groups rows by defect.
    pub kind: String,
    /// The first diagnostic's message, clipped. Not the whole list: the
    /// whole list is hundreds of characters for a minified fixture and
    /// would turn any recovery change into an unreadable diff, while the
    /// count in `kind` already moves when the list does.
    pub detail: String,
}

impl ParseFailure {
    /// Build a row from the diagnostics one fixture produced, as
    /// `(code, message)` pairs in emission order.
    ///
    /// # Panics
    ///
    /// Panics if `diags` is empty — a fixture with no diagnostics parses
    /// cleanly and has no row.
    #[must_use]
    pub fn from_diagnostics(diags: &[(String, String)]) -> Self {
        assert!(
            !diags.is_empty(),
            "a fixture with no diagnostics parses cleanly and cannot have a ratchet row",
        );
        // Counts by code, in first-seen order: the histogram reads like
        // the file, and a new code appearing moves the cell.
        let mut order: Vec<&str> = Vec::new();
        let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
        for (code, _) in diags {
            let code = code.as_str();
            if !counts.contains_key(code) {
                order.push(code);
            }
            *counts.entry(code).or_default() += 1;
        }
        let kind = order
            .iter()
            .map(|code| format!("{code} x{}", counts[code]))
            .collect::<Vec<_>>()
            .join(", ");
        Self {
            kind,
            detail: clip(&diags[0].1),
        }
    }

    /// Human-readable one-liner for a row.
    #[must_use]
    pub fn describe(&self) -> String {
        if self.detail.is_empty() {
            self.kind.clone()
        } else {
            format!("{} — {}", self.kind, self.detail)
        }
    }
}

/// The parsed contents of the known-failures file.
#[derive(Debug, Clone, Default)]
pub struct ParseRatchet {
    /// `# total=<n>` header — how many fixtures the run that wrote this
    /// file covered. See [`ParseRatchet::check_comparable`].
    pub total: Option<usize>,
    /// Failing fixture id → row, sorted by id so the file is a stable
    /// diff.
    pub entries: BTreeMap<String, ParseFailure>,
}

impl ParseRatchet {
    /// Build the file view of a finished run.
    #[must_use]
    pub fn from_failures(
        total: usize,
        failures: impl IntoIterator<Item = (String, ParseFailure)>,
    ) -> Self {
        Self {
            total: Some(total),
            entries: failures.into_iter().collect(),
        }
    }

    /// Serialise as TSV, with a header saying what the rows are and how
    /// to rewrite the file.
    #[must_use]
    pub fn to_tsv(&self, write_command: &str) -> String {
        let mut out = String::new();
        let _ = writeln!(
            out,
            "# Upstream fixtures this parser does NOT parse cleanly, of the ones the"
        );
        let _ = writeln!(
            out,
            "# upstream JUnit suite itself enables. Every row is a parser GAP to close"
        );
        let _ = writeln!(
            out,
            "# (epic R7, #351), not accepted behaviour: the row exists so a NEW break"
        );
        let _ = writeln!(
            out,
            "# fails the build while the known ones are worked off."
        );
        let _ = writeln!(
            out,
            "# Closing one means DELETING its row — a row that starts parsing cleanly"
        );
        let _ = writeln!(out, "# fails the build too, so the list can only shrink.");
        let _ = writeln!(out, "# Rewrite with: {write_command}");
        let _ = writeln!(
            out,
            "# Columns: id<TAB>kind<TAB>detail (\\t \\n \\r \\\\ escaped)."
        );
        if let Some(total) = self.total {
            let _ = writeln!(out, "# total={total}");
        }
        for (id, row) in &self.entries {
            let _ = writeln!(out, "{}\t{}\t{}", escape(id), row.kind, escape(&row.detail));
        }
        out
    }

    /// Parse a known-failures file. Blank lines are skipped; `#` lines
    /// are comments, except the `# total=<n>` header.
    pub fn parse(text: &str) -> Result<Self> {
        let mut out = Self::default();
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
            out.entries.insert(
                unescape(id),
                ParseFailure {
                    kind: kind.to_string(),
                    detail: unescape(it.next().unwrap_or("")),
                },
            );
        }
        Ok(out)
    }

    /// Refuse to gate on a file that says nothing.
    ///
    /// An empty list and a deleted list look identical to a diff that
    /// only reports *new* ids: everything is new, so everything gates —
    /// which is correct. What is not correct is reaching that state by
    /// accident and believing the ratchet still holds a known-bad set.
    pub fn check_gateable(&self, path: &Path) -> Result<()> {
        if self.entries.is_empty() {
            bail!(
                "{} lists no known failures. A ratchet with nothing in it is not a \
                 passing gate, it is a missing one. If every enabled fixture really \
                 parses cleanly, delete this file *and* the ratchet in \
                 tests/parser_fixtures.rs so the suite goes back to a plain assertion; \
                 otherwise restore the file.",
                path.display(),
            );
        }
        Ok(())
    }

    /// Refuse to diff two runs of visibly different size.
    ///
    /// The enabled set is small enough that one fixture is real movement,
    /// so the slack the corpus-sized ratchets allow would swallow it: a
    /// submodule bump that enables or disables a fixture has to be looked
    /// at, not absorbed. Any change in the covered count is reported.
    pub fn check_comparable(&self, current_total: usize) -> Result<()> {
        let Some(committed) = self.total else {
            return Ok(());
        };
        if current_total != committed {
            bail!(
                "this run covered {current_total} upstream-enabled fixture(s) but the \
                 known-failures file was written from {committed}. Either the submodule \
                 moved (check what upstream enabled or disabled, then rewrite the file) \
                 or the sweep was filtered — and a partial sweep would report every \
                 unrun fixture as fixed."
            );
        }
        Ok(())
    }
}

/// What changed between the current run and the committed file. Every
/// bucket gates — see the module docs.
#[derive(Debug, Clone, Default)]
pub struct ParseRatchetDiff {
    /// Failing now, not in the file: a parser regression.
    pub new_ids: Vec<(String, ParseFailure)>,
    /// In the file, parsing cleanly now: the gap was closed and the row
    /// was not deleted.
    pub fixed_ids: Vec<String>,
    /// Same id, different row: `(id, committed, current)`. Still broken,
    /// but the row no longer describes how.
    pub changed_detail: Vec<(String, String, String)>,
}

impl ParseRatchetDiff {
    /// Whether the suite must fail.
    #[must_use]
    pub fn is_regression(&self) -> bool {
        !self.new_ids.is_empty() || !self.fixed_ids.is_empty() || !self.changed_detail.is_empty()
    }
}

/// Diff a fresh run against the committed file.
#[must_use]
pub fn diff_parse_ratchet(current: &ParseRatchet, committed: &ParseRatchet) -> ParseRatchetDiff {
    let mut diff = ParseRatchetDiff::default();
    for (id, now) in &current.entries {
        match committed.entries.get(id) {
            None => diff.new_ids.push((id.clone(), now.clone())),
            Some(was) if was != now => {
                diff.changed_detail
                    .push((id.clone(), was.describe(), now.describe()));
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

/// Keep a detail cell short enough to read in a diff.
fn clip(s: &str) -> String {
    if s.chars().count() <= DETAIL_MAX {
        return s.to_string();
    }
    let head: String = s.chars().take(DETAIL_MAX).collect();
    format!("{head}…")
}

/// TSV cell escaping: an id or a detail may contain a tab or a newline,
/// and the file is line- and tab-delimited, so both have to survive as
/// escapes.
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
