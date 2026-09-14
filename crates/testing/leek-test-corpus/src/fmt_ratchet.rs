//! The formatter's known-bad list over the upstream corpus (#197).
//!
//! `tests/fmt_roundtrip.rs` asserts two properties of every upstream case
//! and every upstream `.leek` AI file: `format_source_checked` accepts the
//! output, and a second run is a no-op. On the day that test was written it
//! failed on 289 of 11005 corpus cases and 16 of 101 AI files — the safety
//! net's first contact with real code found real formatter bugs, including
//! ones that rewrite a keyword into an identifier and so change what the
//! program means.
//!
//! Those bugs cannot all be fixed in the change that made them visible, and
//! deleting the assertion would put the formatter back where it was. So this
//! module is the middle path leek-bench already took for the rust-java sweep
//! (#107, PR #398): the failures are serialised to a **tracked, diffable
//! file**, one sorted row per failing id. The suite then gates on the *diff*:
//!
//! - an id that fails and is **not** in the file fails the build — that is a
//!   new formatter bug, and it is the whole point of the ratchet;
//! - an id in the file that now passes is **reported** so the file shrinks;
//! - a row whose detail changed is informational: the same case is still
//!   broken, the message moved.
//!
//! Read every row as an open bug. Nothing here is accepted behaviour, and
//! nothing here is a reason to relax the check that found it: a row saying
//! `` `KwIf` becomes `ifx` `` is the formatter emitting a keyword glued to
//! the next token, which silently changes the program. The rows group into
//! nine defect classes, each with an issue: #412 (adjacent tokens glued),
//! #413 (`not in` loses its `in`), #414 (`static` dropped from class
//! members), #415 (`[a:b]` / `[a..b]` shredded), #416 (the paren that makes
//! a lambda the callee peeled — 114 rows, the largest), #417 (tokens
//! invented past end of file), #418 (`|x|` broken), #419 (unterminated
//! block comment grows), #420 (annotation spacing not idempotent).
//! `data/README.md` carries the same map for someone reading the files.
//!
//! Two things the gate deliberately refuses to do, because both would let it
//! pass while measuring nothing:
//!
//! - a **missing or empty** file is an error, not an empty allow-list. The
//!   only honest way to reach zero entries is to fix the last bug and delete
//!   the file and the gate together ([`FmtRatchet::check_gateable`]);
//! - a run covering visibly fewer ids than the file was written from is an
//!   error, because every unrun id would otherwise read as "fixed"
//!   ([`FmtRatchet::check_comparable`]).

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::Path;

use anyhow::{Result, bail};

/// `format_source_checked` rejected the *first* format of the input: the
/// output was not equivalent to what went in. The most serious kind — the
/// formatter would corrupt the program.
pub const KIND_UNSAFE: &str = "unsafe";
/// The first format was accepted, the second was not. Same defect class as
/// [`KIND_UNSAFE`], reached only through the formatter's own output.
pub const KIND_UNSAFE_REFORMAT: &str = "unsafe-reformat";
/// Both runs were safe, but the second changed the first's output. Not a
/// corruption, still a bug: `miku fmt --check` can never be satisfied.
pub const KIND_NOT_IDEMPOTENT: &str = "not-idempotent";

/// One row of the tracked file: a failing id's kind and a stable one-line
/// detail.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FmtFailure {
    pub kind: String,
    pub detail: String,
}

impl FmtFailure {
    /// Human-readable one-liner for a row.
    #[must_use]
    pub fn describe(&self) -> String {
        if self.detail.is_empty() {
            self.kind.clone()
        } else {
            format!("{} {}", self.kind, self.detail)
        }
    }
}

/// The parsed contents of one known-failures file.
#[derive(Debug, Clone, Default)]
pub struct FmtRatchet {
    /// `# total=<n>` header — how many ids the run that wrote this file
    /// covered. See [`FmtRatchet::check_comparable`].
    pub total: Option<usize>,
    /// Failing id → row, sorted by id so the file is a stable diff.
    pub entries: BTreeMap<String, FmtFailure>,
}

impl FmtRatchet {
    /// Build the file view of a finished run.
    #[must_use]
    pub fn from_failures(
        total: usize,
        failures: impl IntoIterator<Item = (String, FmtFailure)>,
    ) -> Self {
        Self {
            total: Some(total),
            entries: failures.into_iter().collect(),
        }
    }

    /// Serialise as TSV, with a header naming the suite and how to
    /// regenerate it.
    #[must_use]
    pub fn to_tsv(&self, suite: &str, regenerate: &str) -> String {
        let mut out = String::new();
        let _ = writeln!(out, "# leek-fmt known failures over {suite}.");
        let _ = writeln!(
            out,
            "# Every row is a formatter BUG to fix, not accepted behaviour: the entry"
        );
        let _ = writeln!(
            out,
            "# exists so a NEW break fails the build while the known ones are worked off."
        );
        let _ = writeln!(out, "# Fixing one means DELETING its row, not editing it.");
        let _ = writeln!(
            out,
            "# Which defect each row belongs to, and its issue: see data/README.md."
        );
        let _ = writeln!(out, "# Regenerate with: {regenerate}");
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

    /// Parse a known-failures file. Blank lines are skipped; `#` lines are
    /// comments, except the `# total=<n>` header.
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
                FmtFailure {
                    kind: kind.to_string(),
                    detail: unescape(it.next().unwrap_or("")),
                },
            );
        }
        Ok(out)
    }

    /// Refuse to gate on a file that says nothing.
    ///
    /// An empty allow-list and a deleted allow-list look identical to a diff
    /// that only reports *new* ids: everything is new, so everything gates —
    /// which is correct. What is not correct is reaching that state by
    /// accident and believing the ratchet is still holding a known-bad set.
    /// An empty file is therefore an error with an explicit way out: if the
    /// formatter really is clean, delete the file and the gate together.
    pub fn check_gateable(&self, path: &Path) -> Result<()> {
        if self.entries.is_empty() {
            bail!(
                "{} lists no known failures. A ratchet with nothing in it is not a \
                 passing gate, it is a missing one. If every formatter bug really is \
                 fixed, delete this file *and* the ratchet in tests/fmt_roundtrip.rs so \
                 the suite goes back to a plain assertion; otherwise restore the file.",
                path.display(),
            );
        }
        Ok(())
    }

    /// Refuse to diff two runs of visibly different size. 1% of slack absorbs
    /// a corpus that gained or lost a handful of cases upstream; a filtered
    /// or truncated run is orders of magnitude off and is rejected, because
    /// every unrun id would be reported as fixed.
    pub fn check_comparable(&self, current_total: usize) -> Result<()> {
        let Some(committed) = self.total else {
            return Ok(());
        };
        let slack = (committed / 100).max(1);
        if current_total.abs_diff(committed) > slack {
            bail!(
                "this run covered {current_total} id(s) but the known-failures file was \
                 written from {committed}; a partial run would report every unrun id as \
                 fixed. Run the whole suite, or regenerate the file."
            );
        }
        Ok(())
    }
}

/// What changed between the current run and the committed file.
#[derive(Debug, Clone, Default)]
pub struct FmtRatchetDiff {
    /// Failing now, not in the file. **This is the gate.**
    pub new_ids: Vec<(String, FmtFailure)>,
    /// In the file, passing now — shrink the file.
    pub fixed_ids: Vec<String>,
    /// Same id, different row: `(id, committed, current)`. Informational:
    /// the case is still broken, the message moved.
    pub changed_detail: Vec<(String, String, String)>,
}

impl FmtRatchetDiff {
    /// The gate. Only newly broken ids make the suite fail.
    #[must_use]
    pub fn is_regression(&self) -> bool {
        !self.new_ids.is_empty()
    }
}

/// Diff a fresh run against the committed file.
#[must_use]
pub fn diff_fmt_ratchet(current: &FmtRatchet, committed: &FmtRatchet) -> FmtRatchetDiff {
    let mut diff = FmtRatchetDiff::default();
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

/// A stable one-line signature for a non-idempotent pair.
///
/// The full "once vs twice" texts are what a developer needs and are printed
/// for *new* failures, but they are whole source files: storing them would
/// make the tracked file megabytes of source and turn any reflow into a
/// thousand-line diff. The first differing line is short, is the thing that
/// identifies the defect, and stays put when an unrelated part of the file
/// changes.
#[must_use]
pub fn first_difference(once: &str, twice: &str) -> String {
    let mut first = once.lines();
    let mut second = twice.lines();
    let mut line = 0usize;
    loop {
        line += 1;
        match (first.next(), second.next()) {
            // `lines()` hides a missing final newline, so identical line
            // sequences can still be different texts. Say which it is
            // rather than claiming a line number that did not move.
            (None, None) => return "trailing whitespace only".to_string(),
            (was, now) if was == now => {}
            (was, now) => {
                return format!(
                    "line {line}: {} => {}",
                    clip(was.unwrap_or("<eof>").trim()),
                    clip(now.unwrap_or("<eof>").trim()),
                );
            }
        }
    }
}

/// Keep a detail cell short enough to read in a diff.
fn clip(s: &str) -> String {
    const MAX: usize = 60;
    if s.chars().count() <= MAX {
        return s.to_string();
    }
    let head: String = s.chars().take(MAX).collect();
    format!("{head}…")
}

/// TSV cell escaping: an id or a detail may contain a tab or a newline, and
/// the file is line- and tab-delimited, so both have to survive as escapes.
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
