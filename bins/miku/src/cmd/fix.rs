//! `miku fix` — apply machine-applicable diagnostic suggestions.
//!
//! Files go through the same driver pipeline as `miku check` (includes
//! resolved, manifest lint groups merged) and the same manifest `[lint]`
//! levels: a suggestion attached to an `allow`ed code is never applied.
//! A file with compile errors is reported and left untouched — rewriting
//! it from a broken analysis could corrupt it — and makes the run exit
//! non-zero.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Context, Result};
use leek_diagnostics::{Applicability, Diagnostic, Reporter, Severity};
use leek_driver::DriverConfig;
use leek_pipeline::Input;
use leek_project::Project;
use leek_recipes::{RecipeParams, Target};
use leek_rewrite::EditSet;
use leek_span::SourceId;

use crate::cli::{ColorWhen, Fix, MessageFormat};

pub fn run(
    args: &Fix,
    manifest_path: Option<&Path>,
    color: ColorWhen,
    format: MessageFormat,
    quiet: bool,
) -> Result<ExitCode> {
    let project = Project::discover(manifest_path)?;
    for w in &project.warnings {
        eprintln!("warning: {w}");
    }

    let mut sources = project.walk_sources();
    sources.extend(project.walk_tests());
    if sources.is_empty() {
        if !quiet {
            eprintln!("miku: no .leek sources found");
        }
        return Ok(ExitCode::SUCCESS);
    }

    let config = DriverConfig {
        target: Target::Linted,
        params: RecipeParams::default(),
        color: color.into(),
        format: format.into(),
    };
    let reporter = leek_driver::reporter_for(&project, config.color, config.format)?;

    let mut changed_files = 0usize;
    let mut total_edits = 0usize;
    let mut skipped: Vec<PathBuf> = Vec::new();
    for (next_source, path) in (1_u32..).zip(&sources) {
        let source = SourceId::new(next_source).unwrap();
        let (src, text) = project.pipeline_input(source, path)?;
        let pipeline = leek_driver::file_pipeline(&project, path, source, &config)?;
        let result = pipeline.run(Input::from(src));

        if has_compile_error(&reporter, result.diagnostics()) {
            leek_driver::report(&result, &text, &path.display().to_string(), &reporter);
            skipped.push(path.clone());
            continue;
        }

        let diagnostics = reporter.apply_levels(result.diagnostics());
        let fixed = collect_edits(&diagnostics, source, &text);
        if fixed.edits == 0 {
            continue;
        }

        changed_files += 1;
        total_edits += fixed.edits;
        if !quiet {
            eprintln!(
                "{} {}: {} fix{}",
                if args.dry_run { "would fix" } else { "fix" },
                display_relative(&project.root, path).display(),
                fixed.edits,
                if fixed.edits == 1 { "" } else { "es" },
            );
        }
        if !args.dry_run {
            std::fs::write(path, &fixed.text)
                .with_context(|| format!("writing {}", path.display()))?;
        }
    }

    if !quiet {
        let verb = if args.dry_run {
            "would apply"
        } else {
            "applied"
        };
        eprintln!(
            "miku fix: {verb} {total_edits} suggestion{} across {changed_files} file{}",
            if total_edits == 1 { "" } else { "s" },
            if changed_files == 1 { "" } else { "s" },
        );
    }
    // Always reported, even with `--quiet`: the exit code alone doesn't say
    // which files were left alone.
    if !skipped.is_empty() {
        eprintln!(
            "miku fix: skipped {} file{} with compile errors:",
            skipped.len(),
            if skipped.len() == 1 { "" } else { "s" },
        );
        for path in &skipped {
            eprintln!("  {}", display_relative(&project.root, path).display());
        }
    }

    Ok(
        if !skipped.is_empty() || (args.dry_run && total_edits > 0) {
            ExitCode::from(1)
        } else {
            ExitCode::SUCCESS
        },
    )
}

/// Whether the run has a real compile error: a diagnostic the compiler
/// raised as an error that the manifest's `[lint]` levels neither allow nor
/// downgrade. A lint promoted to error by `deny` is not a compile error —
/// the analysis is sound, so its fix (the reason to deny it) still applies.
fn has_compile_error(reporter: &Reporter, diagnostics: &[Diagnostic]) -> bool {
    let raw_errors: Vec<Diagnostic> = diagnostics
        .iter()
        .filter(|d| d.severity == Severity::Error)
        .cloned()
        .collect();
    reporter
        .apply_levels(&raw_errors)
        .iter()
        .any(|d| d.severity == Severity::Error)
}

struct FixedFile {
    text: String,
    edits: usize,
}

/// Build an [`EditSet`] from every machine-applicable suggestion
/// attached to a diagnostic, then apply it to `text`. Drops
/// suggestions that conflict with already-staged edits — same rule
/// the LSP quick-fix surface enforces, so the in-IDE behavior and
/// the CLI behavior stay aligned. Suggestions that touch another
/// source (an included file) are skipped: their offsets are not
/// offsets into `text`.
fn collect_edits(diagnostics: &[Diagnostic], source: SourceId, text: &str) -> FixedFile {
    let mut set = EditSet::new(text.len());
    let mut count = 0usize;
    for diag in diagnostics {
        for suggestion in &diag.suggestions {
            if !matches!(suggestion.applicability, Applicability::MachineApplicable) {
                continue;
            }
            if suggestion.edits.iter().any(|e| e.span.source != source) {
                continue;
            }
            // Atomic: stage onto a clone; only commit if every edit
            // in this suggestion fits without overlap.
            let mut staged = set.clone();
            if staged.push_suggestion(suggestion).is_ok() {
                set = staged;
                count += 1;
            }
        }
    }
    FixedFile {
        text: set.apply(text),
        edits: count,
    }
}

fn display_relative(root: &Path, p: &Path) -> PathBuf {
    p.strip_prefix(root)
        .map_or_else(|_| p.to_path_buf(), std::path::Path::to_path_buf)
}
