//! What `miku check` and `miku lint` compile, and how they report it.
//!
//! **The default is the entry file plus its include closure**, the same
//! model `cargo` uses and the one a leek-wars AI is written against: a
//! project ships as one entry that `include(...)`s its fragments, and a
//! fragment that leans on symbols the includer declares is not a program
//! on its own. Linting every file in the tree by default would report
//! errors such a fragment does not have (leekwars#274).
//!
//! `--all` is the opt-in for the other question — "is anything in this
//! tree broken?" — and widens the scope to every `.leek` file under
//! `[paths].src` and `[paths].tests`, which is the set `fmt`, `fix`,
//! `migrate` and `doc` already walk. Two rules keep the wider run honest:
//!
//! 1. **One report per file.** A file reached through another file's
//!    `include(...)` is not compiled again in its own right, and a
//!    diagnostic raised inside a file already reported is dropped rather
//!    than printed a second time — so a helper included by two entries is
//!    diagnosed once.
//! 2. **One exit status.** The status is the OR over every file compiled,
//!    so an error in a file the entry never reaches fails the run.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use anyhow::Result;
use leek_diagnostics::Diagnostic;
use leek_project::Project;
use leek_session::{Compilation, Session};
use leek_span::SourceId;

/// The files a diagnostics command compiles, in the order it compiles
/// them: the entry first (so the program's own view of a shared helper is
/// the one reported), then `[paths].src`, then `[paths].tests`.
///
/// With `all` unset this is just the entry — its includes come with it,
/// resolved by the session rather than listed here.
#[must_use]
pub fn targets(project: &Project, all: bool) -> Vec<PathBuf> {
    let entry = project.entry_path();
    if !all {
        return vec![entry];
    }
    let mut files = vec![entry];
    files.extend(project.walk_sources());
    files.extend(project.walk_tests());
    // The entry normally sits under `src/` too, and a project may point
    // `[paths].tests` at `[paths].src`; either way a path is compiled once.
    let mut seen = HashSet::new();
    files.retain(|path| seen.insert(key(path)));
    files
}

/// Compile each of `files`, report its diagnostics at most once, and
/// answer whether any file reported an error.
///
/// `each` runs per compiled file with the compilation and whether *that*
/// file reported an error — `check` hangs its native-compat pass off it.
/// A file skipped as already-covered never reaches it.
pub fn compile_and_report(
    session: &Session<'_>,
    files: &[PathBuf],
    mut each: impl FnMut(&Compilation<'_>, bool) -> Result<()>,
) -> Result<bool> {
    let mut reported: HashSet<PathBuf> = HashSet::new();
    let mut had_error = false;
    for path in files {
        if reported.contains(&key(path)) {
            continue;
        }
        // `compile_shared`, not `compile_file`: several entries in one
        // invocation must take their ids from the session's interner, or
        // the second file is handed an id the first file's includes
        // already hold (#191).
        let compiled = session.compile_shared(path)?;
        let covered = compiled_files(path, &compiled);
        let fresh = unreported(compiled.diagnostics(), &covered, &reported);
        // Through the session's reporter and the compilation's own source
        // map — what `Compilation::report` does, minus the diagnostics
        // another file already printed — so a complaint raised inside an
        // included file still points at that file.
        let file_error = session.reporter().emit(&fresh, compiled.sources());
        had_error |= file_error;
        each(&compiled, file_error)?;
        reported.extend(covered.into_values());
    }
    Ok(had_error)
}

/// Every file this compilation covers, by the `SourceId` its spans carry:
/// the compiled file itself and everything it includes, transitively.
fn compiled_files(path: &Path, compiled: &Compilation<'_>) -> HashMap<SourceId, PathBuf> {
    let mut files = HashMap::new();
    // The id the session's interner settled on, not the one the caller
    // asked for — see `Session::input_for`.
    files.insert(compiled.input().source, key(path));
    for included in compiled.includes() {
        files.insert(included.source, key(&included.path));
    }
    files
}

/// The diagnostics whose file has not been reported yet.
///
/// A diagnostic whose `SourceId` belongs to no file of this compilation —
/// nothing raises one today, but a synthesized span would — is kept: a
/// dropped diagnostic is worse than a repeated one.
fn unreported(
    diagnostics: &[Diagnostic],
    covered: &HashMap<SourceId, PathBuf>,
    reported: &HashSet<PathBuf>,
) -> Vec<Diagnostic> {
    diagnostics
        .iter()
        .filter(|d| {
            covered
                .get(&d.span.source)
                .is_none_or(|path| !reported.contains(path))
        })
        .cloned()
        .collect()
}

/// The identity two paths to the same file share.
fn key(path: &Path) -> PathBuf {
    leek_span::paths::canonical_or_normalized(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use leek_diagnostics::{Code, Severity};
    use leek_span::Span;

    fn source(n: u32) -> SourceId {
        SourceId::new(n).unwrap()
    }

    fn diag(n: u32) -> Diagnostic {
        Diagnostic::new(
            Code("L0001"),
            Severity::Warning,
            Span::new(source(n), 0, 1),
            "boom",
        )
    }

    #[test]
    fn a_diagnostic_from_an_already_reported_file_is_dropped() {
        let mut covered = HashMap::new();
        covered.insert(source(1), PathBuf::from("/p/main.leek"));
        covered.insert(source(2), PathBuf::from("/p/helper.leek"));
        let reported: HashSet<PathBuf> = [PathBuf::from("/p/helper.leek")].into_iter().collect();

        let kept = unreported(&[diag(1), diag(2)], &covered, &reported);
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].span.source, source(1));
    }

    #[test]
    fn a_diagnostic_from_a_file_this_compilation_does_not_own_is_kept() {
        let kept = unreported(&[diag(9)], &HashMap::new(), &HashSet::new());
        assert_eq!(kept.len(), 1);
    }
}
