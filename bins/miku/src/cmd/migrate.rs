//! `miku migrate` — rewrite `.leek` sources from one language
//! version to another while preserving comments and layout.
//!
//! ## Behaviour
//!
//! - Without positional paths: walks `src/` + `tests/` from the
//!   discovered project root (ignoring `build/` and `target/`).
//! - With positional paths: each entry is a `.leek` file or a
//!   directory; directories are walked the same way.
//! - For each file, the source version is the file's own
//!   `@version:N` pragma if present, else the manifest's
//!   `[project].language` (resolved by [`Project::pipeline_input`]
//!   for the rest of the toolchain — we mirror that resolution
//!   here so a freshly-migrated file's pragma matches what every
//!   other miku subcommand will read next time).
//! - `--from` overrides per-file detection — useful when a file
//!   has the wrong pragma.
//! - `--dry-run` reports what would change but writes nothing,
//!   and the process exits non-zero if anything would change.
//!   Identical contract to `miku fmt --check` and
//!   `miku fix --dry-run`.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Context, Result};
use leek_migrate::migrate_text;
use leek_span::SourceId;
use leek_syntax::{Version, parse_pragmas};

use crate::cli::{Migrate, MigrateVersion};
use leek_project::Project;

pub fn run(args: &Migrate, manifest_path: Option<&Path>, quiet: bool) -> Result<ExitCode> {
    let project = Project::discover(manifest_path)?;
    for w in &project.warnings {
        eprintln!("warning: {w}");
    }

    let files = collect_files(&project, &args.paths)?;
    if files.is_empty() {
        if !quiet {
            eprintln!("miku migrate: no .leek sources found");
        }
        return Ok(ExitCode::SUCCESS);
    }

    let target = args.to.to_syntax();
    let mut changed = 0usize;
    let mut skipped = 0usize;
    let mut warnings = 0usize;

    for (i, path) in files.iter().enumerate() {
        let source_id = SourceId::new((i + 1).try_into().unwrap()).unwrap();
        let text =
            std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;

        let from = match args.from {
            Some(v) => v.to_syntax(),
            None => detect_version(&text, source_id),
        };

        if from == target {
            skipped += 1;
            if !quiet {
                eprintln!(
                    "{}: already at {} — skipped",
                    display_relative(&project.root, path).display(),
                    version_label(target),
                );
            }
            continue;
        }

        let out = migrate_text(&text, source_id, from, target);
        if out.text == text {
            // Migration chained through but produced no edits
            // (e.g. v2 → v3 on a file with no v3-affecting
            // constructs). Skip without claiming a change.
            skipped += 1;
            continue;
        }

        for diag in &out.diagnostics {
            warnings += 1;
            if !quiet {
                eprintln!(
                    "{}: {}",
                    display_relative(&project.root, path).display(),
                    diag.message,
                );
            }
        }

        changed += 1;
        if !quiet {
            eprintln!(
                "{} {}: {} → {}",
                if args.dry_run {
                    "would migrate"
                } else {
                    "migrated"
                },
                display_relative(&project.root, path).display(),
                version_label(from),
                version_label(target),
            );
        }
        if !args.dry_run {
            std::fs::write(path, &out.text)
                .with_context(|| format!("writing {}", path.display()))?;
        }
    }

    if !quiet {
        let verb = if args.dry_run {
            "would migrate"
        } else {
            "migrated"
        };
        let mut parts = vec![format!(
            "miku migrate: {verb} {changed} file{}",
            if changed == 1 { "" } else { "s" },
        )];
        if skipped > 0 {
            parts.push(format!("{skipped} skipped",));
        }
        if warnings > 0 {
            parts.push(format!(
                "{warnings} warning{}",
                if warnings == 1 { "" } else { "s" },
            ));
        }
        eprintln!("{}", parts.join(", "));
    }

    Ok(if args.dry_run && changed > 0 {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    })
}

/// Resolve the set of `.leek` files to migrate.
///
/// If `requested` is empty we fall back to the project's standard
/// `src/` + `tests/` walks. Otherwise each entry is taken as
/// either a single `.leek` file or a directory walked with the
/// same `.gitignore`-respecting policy.
fn collect_files(project: &Project, requested: &[PathBuf]) -> Result<Vec<PathBuf>> {
    if requested.is_empty() {
        let mut out = project.walk_sources();
        out.extend(project.walk_tests());
        return Ok(out);
    }
    let mut out = Vec::new();
    for entry in requested {
        let p = if entry.is_absolute() {
            entry.clone()
        } else {
            std::env::current_dir()
                .context("determining current directory")?
                .join(entry)
        };
        if p.is_file() {
            if p.extension().is_some_and(|e| e == "leek") {
                out.push(p);
            } else {
                anyhow::bail!("{}: not a .leek file", p.display());
            }
        } else if p.is_dir() {
            out.extend(walk_dir(&p));
        } else {
            anyhow::bail!("{}: not a file or directory", p.display());
        }
    }
    out.sort();
    out.dedup();
    Ok(out)
}

/// `.gitignore`-aware walk for an arbitrary directory. Mirrors
/// the policy in `Project::walk_sources` (skip `build/`,
/// `target/`, hidden, respect `.gitignore`/`.ignore`).
fn walk_dir(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut builder = ignore::WalkBuilder::new(dir);
    builder
        .standard_filters(true)
        .hidden(true)
        .git_ignore(true)
        .git_exclude(true)
        .git_global(false)
        .parents(true)
        .require_git(false);
    let mut overrides = ignore::overrides::OverrideBuilder::new(dir);
    let _ = overrides.add("!build/");
    let _ = overrides.add("!target/");
    if let Ok(ov) = overrides.build() {
        builder.overrides(ov);
    }
    for entry in builder.build().flatten() {
        let path = entry.path();
        if path.is_file() && path.extension().is_some_and(|e| e == "leek") {
            out.push(path.to_path_buf());
        }
    }
    out
}

/// Read the file's `@version:N` pragma if present (defaults to v4
/// per the rest of the toolchain).
fn detect_version(text: &str, source_id: SourceId) -> Version {
    let (pragmas, _) = parse_pragmas(text, source_id);
    pragmas.version
}

fn version_label(v: Version) -> &'static str {
    match v {
        Version::V1 => "v1",
        Version::V2 => "v2",
        Version::V3 => "v3",
        Version::V4 => "v4",
    }
}

fn display_relative(root: &Path, p: &Path) -> PathBuf {
    p.strip_prefix(root)
        .map_or_else(|_| p.to_path_buf(), std::path::Path::to_path_buf)
}

// Suppress an unused-import warning if `MigrateVersion` is not
// referenced directly in the body of this module — clap derives
// reach it transitively via `args.to`.
#[allow(dead_code)]
type _Phantom = MigrateVersion;
