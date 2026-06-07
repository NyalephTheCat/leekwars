//! `miku fix` — apply machine-applicable diagnostic suggestions.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Context, Result};
use leek_diagnostics::{Applicability, Diagnostic};
use leek_rewrite::EditSet;
use leek_span::SourceId;

use crate::cli::Fix;
use leek_pipeline::Input;
use leek_project::Project;

pub fn run(args: Fix, manifest_path: Option<&Path>, quiet: bool) -> Result<ExitCode> {
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

    let mut changed_files = 0usize;
    let mut total_edits = 0usize;
    for (i, path) in sources.iter().enumerate() {
        let source = SourceId::new((i + 1).try_into().unwrap()).unwrap();
        let (src, text) = project.pipeline_input(source, path)?;
        let input = Input::from(src);
        let pipeline =
            leek_recipes::pipeline(leek_recipes::Target::Linted, &leek_recipes::driver_params())
                .expect("recipe");
        let result = pipeline.run(input);

        let fixed = collect_edits(result.diagnostics(), &text);
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

    Ok(if args.dry_run && total_edits > 0 {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    })
}

struct FixedFile {
    text: String,
    edits: usize,
}

/// Build an [`EditSet`] from every machine-applicable suggestion
/// attached to a diagnostic, then apply it to `text`. Drops
/// suggestions that conflict with already-staged edits — same rule
/// the LSP quick-fix surface enforces, so the in-IDE behavior and
/// the CLI behavior stay aligned.
fn collect_edits(diagnostics: &[Diagnostic], text: &str) -> FixedFile {
    let mut set = EditSet::new(text.len());
    let mut count = 0usize;
    for diag in diagnostics {
        for suggestion in &diag.suggestions {
            if !matches!(suggestion.applicability, Applicability::MachineApplicable) {
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
    p.strip_prefix(root).map_or_else(|_| p.to_path_buf(), std::path::Path::to_path_buf)
}
