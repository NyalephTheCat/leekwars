//! `miku fmt` — format every `.leek` source.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Context, Result, bail};
use leek_manifest::FormatOptions;
use leek_span::SourceId;
use leek_syntax::Version;

use crate::cli::Fmt;
use leek_backends::version_from_byte;
use leek_project::{Project, walk_leek_files};

pub fn run(args: &Fmt, manifest_path: Option<&Path>, quiet: bool) -> Result<ExitCode> {
    // Outside a project, stdin / explicit paths still work with the
    // default style; a bare `miku fmt` keeps requiring a manifest.
    let standalone = args.stdin || !args.paths.is_empty();
    let project = match Project::discover(manifest_path) {
        Ok(project) => {
            for w in &project.warnings {
                eprintln!("warning: {w}");
            }
            Some(project)
        }
        Err(e) if standalone => {
            if !quiet && manifest_path.is_some() {
                eprintln!("warning: {e}; using default format options");
            }
            None
        }
        Err(e) => return Err(e),
    };
    let opts = resolve_options(args, project.as_ref())?;

    if args.stdin {
        return run_stdin(args, &opts);
    }

    let sources = collect_sources(args, project.as_ref())?;
    if sources.is_empty() {
        if !quiet {
            eprintln!("miku: no .leek sources found");
        }
        return Ok(ExitCode::SUCCESS);
    }

    // `--diff` never writes; it behaves like `--check` with output.
    let dry_run = args.check || args.diff;
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
        if args.diff {
            print!("{}", unified_diff(&original, &formatted, path));
        }
        if dry_run {
            if args.check && !quiet {
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

    if dry_run && any_changes {
        if !quiet {
            eprintln!(
                "miku: {changed_files} file{} would be reformatted",
                if changed_files == 1 { "" } else { "s" }
            );
        }
        return Ok(ExitCode::from(1));
    }
    if !quiet && !dry_run && changed_files > 0 {
        eprintln!(
            "miku: reformatted {changed_files} file{}",
            if changed_files == 1 { "" } else { "s" }
        );
    }
    Ok(ExitCode::SUCCESS)
}

/// `--stdin`: format stdin to stdout. `--check`/`--diff` suppress the
/// formatted output and exit non-zero when the input isn't formatted.
fn run_stdin(args: &Fmt, opts: &FormatOptions) -> Result<ExitCode> {
    let original = std::io::read_to_string(std::io::stdin()).context("reading stdin")?;
    let source = SourceId::new(1).unwrap();
    let version = detect_version(&original, source);
    let formatted = leek_fmt::format_source(&original, source, version, opts);
    if args.diff {
        print!(
            "{}",
            unified_diff(&original, &formatted, Path::new("<stdin>"))
        );
        return Ok(ExitCode::from(u8::from(formatted != original)));
    }
    if args.check {
        return Ok(ExitCode::from(u8::from(formatted != original)));
    }
    print!("{formatted}");
    Ok(ExitCode::SUCCESS)
}

/// Format options: project manifest when available (defaults
/// otherwise), with `--set key=value` overrides applied on top.
fn resolve_options(args: &Fmt, project: Option<&Project>) -> Result<FormatOptions> {
    let mut opts = project.map_or_else(FormatOptions::default, |p| p.manifest.format.clone());
    for kv in &args.set {
        let Some((key, value)) = kv.split_once('=') else {
            bail!("--set expects KEY=VALUE, got {kv:?}");
        };
        opts.set(key.trim(), value.trim())
            .map_err(|e| anyhow::anyhow!("--set {key}: {e}"))?;
    }
    Ok(opts)
}

/// The files to format: explicit `paths` args (directories walked for
/// `.leek` files) or the whole project when none are given.
fn collect_sources(args: &Fmt, project: Option<&Project>) -> Result<Vec<PathBuf>> {
    if args.paths.is_empty() {
        // `run` only passes `None` for standalone runs, which always
        // have paths — but keep a clear error just in case.
        let Some(project) = project else {
            bail!("no project found; pass file or directory paths to format");
        };
        let mut sources = project.walk_sources();
        sources.extend(project.walk_tests());
        return Ok(sources);
    }
    let mut sources = Vec::new();
    for path in &args.paths {
        if path.is_dir() {
            sources.extend(walk_leek_files(path));
        } else if path.is_file() {
            sources.push(path.clone());
        } else {
            bail!("no such file or directory: {}", path.display());
        }
    }
    sources.sort();
    sources.dedup();
    Ok(sources)
}

/// Render a `--diff` hunk set for one file, `diff -u` style.
fn unified_diff(original: &str, formatted: &str, path: &Path) -> String {
    let diff = similar::TextDiff::from_lines(original, formatted);
    format!(
        "--- {p}\n+++ {p} (formatted)\n{hunks}",
        p = path.display(),
        hunks = diff.unified_diff().context_radius(3)
    )
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
