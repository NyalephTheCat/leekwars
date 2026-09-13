//! `cargo xtask check-artifacts`: keep generated output out of the repository.
//!
//! Some of the tooling here writes its scratch files into a fixed directory
//! relative to the process's working directory rather than somewhere under
//! `target/`. The upstream LeekScript compiler is the worst offender: it
//! hard-codes `JavaCompiler.IA_PATH = "ai"` and drops `AI_<id>.java`,
//! `.class`, `.lines` and `.sig` there for every AI it compiles. Run the JVM
//! suite from the repository root once and `ai/` appears next to `Cargo.toml`;
//! commit with `git add -A` and 387 files of compiler output land in history
//! (that is exactly how commit 3698e18 happened).
//!
//! The scripts now run the JVM from a scratch directory under
//! `tools/java-emitter/build/`, so the root stays clean, and `.gitignore`
//! covers the directory in case something else writes it. This task is what keeps both true: for every
//! known generated path it checks that git tracks nothing under it *and* that
//! `.gitignore` still names it, so the next accidental `git add -A` is a CI
//! failure rather than a permanent addition to the clone.

use std::path::Path;
use std::process::{Command, ExitCode};

/// `.gitignore` location, relative to the workspace root.
const GITIGNORE_PATH: &str = ".gitignore";

/// How many offending paths to name before summarising the rest.
const MAX_LISTED: usize = 5;

/// A path some tool generates, which must stay out of git.
pub struct Generated {
    /// Repository-relative path, without a trailing slash.
    pub path: &'static str,
    /// What writes it, for the error message.
    pub produced_by: &'static str,
}

/// Every generated path the repository knows about.
const GENERATED: &[Generated] = &[Generated {
    path: "ai",
    produced_by: "the upstream LeekScript compiler (`JavaCompiler.IA_PATH`), which writes \
                  AI_<id>.java/.class/.lines/.sig relative to the JVM's working directory",
}];

impl Generated {
    /// Problems with this path, given the files git reports as tracked under
    /// it and the contents of `.gitignore`.
    fn problems(&self, tracked: &[String], gitignore: &str) -> Vec<String> {
        let mut errors = Vec::new();
        if !tracked.is_empty() {
            let listed: Vec<&str> = tracked
                .iter()
                .take(MAX_LISTED)
                .map(String::as_str)
                .collect();
            let rest = tracked.len().saturating_sub(listed.len());
            let more = if rest == 0 {
                String::new()
            } else {
                format!(" (+{rest} more)")
            };
            errors.push(format!(
                "`{}/` is tracked by git ({} file{}: {}{}). It is written by {}. \
                 Remove it with `git rm -r --cached {}`",
                self.path,
                tracked.len(),
                if tracked.len() == 1 { "" } else { "s" },
                listed.join(", "),
                more,
                self.produced_by,
                self.path,
            ));
        }
        if !ignored_by(gitignore, self.path) {
            errors.push(format!(
                "{GITIGNORE_PATH} has no rule for `{0}/`, which is written by {1}. \
                 Add `/{0}/` so a stray `git add -A` cannot commit it",
                self.path, self.produced_by,
            ));
        }
        errors
    }
}

/// Whether `.gitignore` carries a rule naming `path` as a directory at the
/// root. Deliberately narrow: only a literal `ai`, `/ai`, `ai/` or `/ai/`
/// counts, because anything cleverer is not what we want a hygiene check to
/// guess at.
fn ignored_by(gitignore: &str, path: &str) -> bool {
    gitignore.lines().any(|line| {
        let rule = line.trim();
        if rule.is_empty() || rule.starts_with('#') || rule.starts_with('!') {
            return false;
        }
        rule.trim_start_matches('/').trim_end_matches('/') == path
    })
}

/// Files git tracks under `path`, sorted as git reports them.
fn tracked_under(root: &Path, path: &str) -> Result<Vec<String>, String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["ls-files", "-z", "--"])
        .arg(path)
        .output()
        .map_err(|e| format!("cannot run `git ls-files`: {e}"))?;
    if !output.status.success() {
        return Err(format!(
            "`git ls-files -- {path}` failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout)
        .split('\0')
        .filter(|s| !s.is_empty())
        .map(ToOwned::to_owned)
        .collect())
}

/// Whether `root` is inside a git work tree. A source tarball is not, and
/// there is nothing to check there.
fn in_work_tree(root: &Path) -> bool {
    Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["rev-parse", "--is-inside-work-tree"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// Entry point for `cargo xtask check-artifacts`.
pub fn run() -> ExitCode {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask lives one level below the workspace root");

    if !in_work_tree(root) {
        println!("artifact check: skipped (not a git work tree)");
        return ExitCode::SUCCESS;
    }

    let gitignore = match std::fs::read_to_string(root.join(GITIGNORE_PATH)) {
        Ok(text) => text,
        Err(e) => {
            eprintln!("artifact check: cannot read {GITIGNORE_PATH}: {e}");
            return ExitCode::FAILURE;
        }
    };

    let mut errors = Vec::new();
    for entry in GENERATED {
        match tracked_under(root, entry.path) {
            Ok(tracked) => errors.extend(entry.problems(&tracked, &gitignore)),
            Err(e) => errors.push(e),
        }
    }

    if errors.is_empty() {
        println!(
            "artifact check: ok ({} generated path{} untracked and ignored)",
            GENERATED.len(),
            if GENERATED.len() == 1 { "" } else { "s" }
        );
        return ExitCode::SUCCESS;
    }
    for e in &errors {
        eprintln!("artifact check: {e}");
    }
    ExitCode::FAILURE
}

#[cfg(test)]
mod tests {
    use super::*;

    const AI: Generated = Generated {
        path: "ai",
        produced_by: "the upstream compiler",
    };

    #[test]
    fn an_untracked_ignored_path_is_clean() {
        assert!(AI.problems(&[], "/target\n/ai/\n").is_empty());
    }

    /// The bug this task exists for: 387 files of JVM output committed by a
    /// `git add -A` at the repository root.
    #[test]
    fn tracked_output_is_rejected() {
        let tracked: Vec<String> = (0..387).map(|i| format!("ai/AI_{i}.class")).collect();
        let errors = AI.problems(&tracked, "/ai/\n");
        assert_eq!(errors.len(), 1, "{errors:?}");
        assert!(
            errors[0].contains("tracked by git (387 files"),
            "{errors:?}"
        );
        assert!(errors[0].contains("(+382 more)"), "{errors:?}");
    }

    #[test]
    fn a_missing_ignore_rule_is_rejected() {
        let errors = AI.problems(&[], "/target\n");
        assert_eq!(errors.len(), 1, "{errors:?}");
        assert!(errors[0].contains("has no rule"), "{errors:?}");
    }

    #[test]
    fn a_single_tracked_file_reads_singular_and_lists_it() {
        let errors = AI.problems(&["ai/AI_1.class".to_owned()], "/ai/\n");
        assert_eq!(errors.len(), 1, "{errors:?}");
        assert!(errors[0].contains("(1 file: ai/AI_1.class)"), "{errors:?}");
    }

    #[test]
    fn ignore_rules_are_recognised_in_every_spelling() {
        for rule in ["ai", "/ai", "ai/", "/ai/", "  /ai/  "] {
            assert!(ignored_by(&format!("/target\n{rule}\n"), "ai"), "{rule}");
        }
    }

    #[test]
    fn comments_negations_and_near_misses_do_not_count_as_rules() {
        for line in ["# /ai/", "!/ai/", "ai.txt", "crates/ai/", "/aio/"] {
            assert!(!ignored_by(&format!("/target\n{line}\n"), "ai"), "{line}");
        }
    }

    /// The repository itself keeps every generated path out of git.
    #[test]
    fn the_repo_tracks_no_generated_output() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("workspace root");
        if !in_work_tree(root) {
            return;
        }
        let gitignore = std::fs::read_to_string(root.join(GITIGNORE_PATH)).expect(GITIGNORE_PATH);
        for entry in GENERATED {
            let tracked = tracked_under(root, entry.path).expect("git ls-files");
            let errors = entry.problems(&tracked, &gitignore);
            assert!(errors.is_empty(), "{errors:#?}");
        }
    }
}
