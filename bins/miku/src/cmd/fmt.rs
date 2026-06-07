//! `miku fmt` — format every `.leek` source.

use std::path::Path;
use std::process::ExitCode;

use anyhow::{Context, Result};
use leek_span::SourceId;
use leek_syntax::Version;

use crate::cli::Fmt;
use leek_backends::version_from_byte;
use leek_project::Project;

pub fn run(args: Fmt, manifest_path: Option<&Path>, quiet: bool) -> Result<ExitCode> {
    let project = Project::discover(manifest_path)?;
    for w in &project.warnings {
        eprintln!("warning: {w}");
    }
    let opts = project.manifest.format.clone();

    let mut sources = project.walk_sources();
    sources.extend(project.walk_tests());
    if sources.is_empty() {
        if !quiet {
            eprintln!("miku: no .leek sources found");
        }
        return Ok(ExitCode::SUCCESS);
    }

    let mut any_changes = false;
    let mut changed_files = 0usize;
    for (i, path) in sources.iter().enumerate() {
        let original =
            std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        let source = SourceId::new((i + 1).try_into().unwrap()).unwrap();
        let version = detect_version(&original, source);
        let formatted = leek_fmt::format_source(&original, source, version, &opts);
        if formatted == original {
            continue;
        }
        any_changes = true;
        changed_files += 1;
        if args.check {
            if !quiet {
                eprintln!("would reformat {}", path.display());
            }
        } else {
            std::fs::write(path, &formatted)
                .with_context(|| format!("writing {}", path.display()))?;
            if !quiet {
                eprintln!("reformatted {}", path.display());
            }
        }
    }

    if args.check && any_changes {
        if !quiet {
            eprintln!(
                "miku: {changed_files} file{} would be reformatted",
                if changed_files == 1 { "" } else { "s" }
            );
        }
        return Ok(ExitCode::from(1));
    }
    if !quiet && !args.check && changed_files > 0 {
        eprintln!(
            "miku: reformatted {changed_files} file{}",
            if changed_files == 1 { "" } else { "s" }
        );
    }
    Ok(ExitCode::SUCCESS)
}

fn detect_version(text: &str, source: SourceId) -> Version {
    let (pragmas, _) = leek_syntax::parse_pragmas(text, source);
    version_from_byte(match pragmas.version {
        Version::V1 => 1,
        Version::V2 => 2,
        Version::V3 => 3,
        Version::V4 => 4,
    })
}
