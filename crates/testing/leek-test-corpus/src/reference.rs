//! Official-LeekScript reference dataset: explicit regeneration + provenance.
//!
//! The reference dataset (`data/reference.tsv`) holds one row per
//! passing upstream value-bearing assertion — `version, strict, kind,
//! value, jvm_ops, code, generated_java` — produced by running the
//! official Java suite with the `LEEK_REFERENCE` probe (see
//! `tools/java-emitter/generate-reference.sh` and the probe in
//! `TestCommon.java`).
//!
//! Regeneration is **never** a side effect of a build (#148): it takes
//! minutes of JVM time and rewrites a tracked 13 MB file, so it belongs
//! to one explicit command,
//! `cargo run -p leek-test-corpus -- extract-reference`. `build.rs` does
//! not use this module at all.
//!
//! What replaces the old mtime-based staleness guess is *provenance*:
//! the regenerating command records which upstream sources produced the
//! dataset in its first line, and `tests/reference_provenance.rs`
//! compares that against the checkout. A recorded fact beats a
//! timestamp, which a fresh clone resets to "now" in arbitrary order.

use std::path::{Path, PathBuf};
use std::process::Command;

/// The committed reference dataset under the crate's `data/` dir.
pub fn committed_path(manifest_dir: &Path) -> PathBuf {
    manifest_dir.join("data/reference.tsv")
}

/// Repo root, three levels up from the crate manifest dir
/// (`crates/testing/leek-test-corpus`).
pub fn repo_root(manifest_dir: &Path) -> PathBuf {
    manifest_dir.join("../../..")
}

/// The bash generator that drives the official JVM suite.
pub fn script_path(manifest_dir: &Path) -> PathBuf {
    repo_root(manifest_dir).join("tools/java-emitter/generate-reference.sh")
}

/// The upstream generator submodule.
fn submodule_path(manifest_dir: &Path) -> PathBuf {
    repo_root(manifest_dir).join("official-generator/leek-wars-generator")
}

/// The pristine-submodule overlay carrying the `LEEK_REFERENCE` probe,
/// relative to the repo root (see `tools/java-emitter/overlay.sh`).
const OVERLAY_REL: &str = "tools/java-emitter/overlay";

/// Upstream Java source trees whose changes invalidate the reference
/// (the test cases themselves + the compiler that emits their Java).
fn upstream_dirs(manifest_dir: &Path) -> [PathBuf; 2] {
    let leek = submodule_path(manifest_dir).join("leekscript");
    [leek.join("src/test/java"), leek.join("src/main/java")]
}

/// True when the upstream submodule is checked out.
pub fn submodule_present(manifest_dir: &Path) -> bool {
    upstream_dirs(manifest_dir).iter().all(|d| d.exists())
}

/// True when both `java` and `javac` are runnable.
pub fn jvm_available() -> bool {
    runnable("java") && runnable("javac")
}

fn runnable(bin: &str) -> bool {
    Command::new(bin)
        .arg("-version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// Run the generator script, writing the reference dataset to `out`.
/// Blocks for as long as the official JVM suite takes (minutes), which
/// is why only the `extract-reference` subcommand calls it.
pub fn regenerate(manifest_dir: &Path, out: &Path) -> Result<(), String> {
    let script = script_path(manifest_dir);
    if !script.exists() {
        return Err(format!(
            "generator script not found at {}",
            script.display()
        ));
    }
    let status = Command::new("bash")
        .arg(&script)
        .arg(out)
        .status()
        .map_err(|e| format!("spawning {}: {e}", script.display()))?;
    if !status.success() {
        return Err(format!("{} exited with {status}", script.display()));
    }
    if !out.exists() {
        return Err(format!("generator produced no file at {}", out.display()));
    }
    Ok(())
}

// ───────────────────────────── provenance ────────────────────────────

/// Marker starting the dataset's provenance line. A `#`-prefixed first
/// line is inert for every reader of the file: they all split on tabs
/// and skip rows with fewer than seven columns, or skip `#` outright.
pub const PROVENANCE_PREFIX: &str = "# provenance\t";

/// Which upstream sources a reference run was made from:
/// `upstream=<submodule commit>\toverlay=<git tree hash>[-dirty]`.
///
/// `None` when it cannot be established — no submodule checkout, or no
/// `git` — in which case the dataset is left without a provenance line
/// rather than carrying a guess.
///
/// The overlay hash is git's own tree hash of [`OVERLAY_REL`], so it
/// covers committed overlay content exactly; uncommitted edits there
/// cannot change a tree hash, so they are reported as `-dirty` instead.
pub fn current_provenance(manifest_dir: &Path) -> Option<String> {
    let root = repo_root(manifest_dir);
    let upstream = git(&submodule_path(manifest_dir), &["rev-parse", "HEAD"])?;
    let overlay = git(&root, &["rev-parse", &format!("HEAD:{OVERLAY_REL}")])?;
    let dirty = !git(&root, &["status", "--porcelain", "--", OVERLAY_REL])?.is_empty();
    let suffix = if dirty { "-dirty" } else { "" };
    Some(format!("upstream={upstream}\toverlay={overlay}{suffix}"))
}

/// The provenance recorded in a dataset, or `None` when it carries none
/// (datasets generated before #148 do not).
pub fn recorded_provenance(dataset: &str) -> Option<&str> {
    dataset.lines().next()?.strip_prefix(PROVENANCE_PREFIX)
}

/// Rewrite `path` so its first line records `provenance`, replacing any
/// line already there.
pub fn record_provenance(path: &Path, provenance: &str) -> std::io::Result<()> {
    let text = std::fs::read_to_string(path)?;
    let body = match text.split_once('\n') {
        Some((first, rest)) if first.starts_with(PROVENANCE_PREFIX) => rest,
        _ => text.as_str(),
    };
    std::fs::write(path, format!("{PROVENANCE_PREFIX}{provenance}\n{body}"))
}

fn git(dir: &Path, args: &[&str]) -> Option<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("leek-reference-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("create scratch dir");
        dir.join("reference.tsv")
    }

    #[test]
    fn provenance_reads_back_and_leaves_the_rows_alone() {
        let path = scratch("roundtrip");
        std::fs::write(&path, "# version\tstrict\n4\t-\tequals\n").unwrap();

        record_provenance(&path, "upstream=abc\toverlay=def").unwrap();
        let text = std::fs::read_to_string(&path).unwrap();

        assert_eq!(
            recorded_provenance(&text),
            Some("upstream=abc\toverlay=def")
        );
        assert!(text.ends_with("# version\tstrict\n4\t-\tequals\n"));
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn recording_twice_replaces_rather_than_stacks() {
        let path = scratch("replace");
        std::fs::write(&path, "4\t-\tequals\n").unwrap();

        record_provenance(&path, "upstream=one\toverlay=x").unwrap();
        record_provenance(&path, "upstream=two\toverlay=y").unwrap();
        let text = std::fs::read_to_string(&path).unwrap();

        assert_eq!(recorded_provenance(&text), Some("upstream=two\toverlay=y"));
        assert_eq!(text.lines().filter(|l| l.starts_with('#')).count(), 1);
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn a_dataset_without_a_provenance_line_reports_none() {
        assert_eq!(recorded_provenance("# version\tstrict\n4\t-\n"), None);
        assert_eq!(recorded_provenance(""), None);
    }
}
