//! Official-LeekScript reference dataset: gated regeneration + embed.
//!
//! The reference dataset (`data/reference.tsv`) holds one row per
//! passing upstream value-bearing assertion — `version, strict, kind,
//! value, jvm_ops, code, generated_java` — produced by running the
//! official Java suite with the `LEEK_REFERENCE` probe (see
//! `tools/java-emitter/generate-reference.sh` and the probe in
//! `TestCommon.java`).
//!
//! This module is shared by `build.rs` (via `#[path]`) and the
//! `extract-reference` subcommand, so it depends on `std` only.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::SystemTime;

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

/// Upstream Java source trees whose changes invalidate the reference
/// (the test cases themselves + the compiler that emits their Java).
fn upstream_dirs(manifest_dir: &Path) -> [PathBuf; 2] {
    let leek = repo_root(manifest_dir).join("official-generator/leek-wars-generator/leekscript");
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
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Newest mtime of any `*.java` under `dir` (recursive). `None` if the
/// dir is absent or empty.
fn newest_java_mtime(dir: &Path) -> Option<SystemTime> {
    let mut newest: Option<SystemTime> = None;
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&d) else {
            continue;
        };
        for e in entries.flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else if p.extension().is_some_and(|x| x == "java")
                && let Ok(m) = e.metadata().and_then(|m| m.modified())
            {
                newest = Some(newest.map_or(m, |cur| cur.max(m)));
            }
        }
    }
    newest
}

/// Newest mtime across all upstream source trees.
fn sources_newest(manifest_dir: &Path) -> Option<SystemTime> {
    upstream_dirs(manifest_dir)
        .iter()
        .filter_map(|d| newest_java_mtime(d))
        .max()
}

/// `target` is stale if it is missing or older than the newest upstream
/// source. With no upstream sources (submodule absent) nothing is
/// considered stale — we can't do better than the committed copy.
pub fn is_stale(target: &Path, manifest_dir: &Path) -> bool {
    let Ok(target_mtime) = std::fs::metadata(target).and_then(|m| m.modified()) else {
        return true;
    };
    match sources_newest(manifest_dir) {
        Some(src) => target_mtime < src,
        None => false,
    }
}

/// Run the generator script, writing the reference dataset to `out`.
/// Blocks for as long as the official JVM suite takes (minutes).
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

/// Header for an empty/placeholder dataset (keeps the embed valid when
/// no reference can be produced).
pub const HEADER: &str = "# version\tstrict\tkind\tvalue\tjvm_ops\tcode\tjava\n";

fn mtime(p: &Path) -> Option<SystemTime> {
    std::fs::metadata(p).and_then(|m| m.modified()).ok()
}

/// build.rs entry point: regenerate the committed dataset when it is
/// stale vs the upstream sources (the only *expensive* step, gated on a
/// JDK + submodule), then stage it into `out_dir/reference.tsv` for
/// `include_str!`.
///
/// The regen decision is keyed on `committed`-vs-sources, while the
/// (cheap) copy is keyed on `committed`-vs-`embed`. Keeping those two
/// independent means a regeneration — which refreshes `committed` —
/// never triggers a rebuild loop (we never `rerun-if-changed` on the
/// committed file), yet a freshly-committed dataset is always picked up.
/// Set `LEEK_SKIP_REFERENCE_REGEN=1` to embed the committed copy as-is.
pub fn prepare_embed(manifest_dir: &Path, out_dir: &Path) {
    let committed = committed_path(manifest_dir);
    let embed = out_dir.join("reference.tsv");

    let skip = std::env::var_os("LEEK_SKIP_REFERENCE_REGEN").is_some();
    if is_stale(&committed, manifest_dir) {
        if !skip && submodule_present(manifest_dir) && jvm_available() {
            println!(
                "cargo:warning=reference dataset stale/missing — regenerating via official JVM suite (minutes)…"
            );
            match regenerate(manifest_dir, &committed) {
                Ok(()) => {
                    let rows = std::fs::read_to_string(&committed)
                        .map(|s| s.lines().filter(|l| !l.starts_with('#')).count())
                        .unwrap_or(0);
                    println!(
                        "cargo:warning=regenerated {} ({rows} rows)",
                        committed.display()
                    );
                }
                Err(e) => {
                    println!(
                        "cargo:warning=reference regen failed: {e} (embedding committed/empty)"
                    );
                }
            }
        } else if committed.exists() {
            println!(
                "cargo:warning=reference dataset may be stale (no JDK / submodule, or regen skipped) — \
                 embedding committed copy; run `cargo run -p leek-test-corpus -- extract-reference` to refresh"
            );
        }
    }

    // Stage committed → embed (cheap). Skip the copy when the embed copy
    // is already up to date with the committed file.
    let copy_needed = !embed.exists()
        || match (mtime(&committed), mtime(&embed)) {
            (Some(c), Some(e)) => c > e,
            _ => true,
        };
    if !copy_needed {
        return;
    }
    if committed.exists() {
        if let Err(e) = std::fs::copy(&committed, &embed) {
            println!("cargo:warning=failed to stage reference dataset: {e}");
            let _ = std::fs::write(&embed, HEADER);
        }
    } else {
        println!("cargo:warning=no reference dataset found — embedding empty placeholder");
        let _ = std::fs::write(&embed, HEADER);
    }
}
