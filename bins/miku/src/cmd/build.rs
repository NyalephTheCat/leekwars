//! `miku build` — compile via the manifest's selected backend.

use std::path::Path;
use std::process::ExitCode;

use anyhow::{Context, Result, bail};
use leek_backends::{java_clean_mode, pick_java_out_dir, pick_out_dir, resolve_backend};
use leek_hir::pipeline::HirArtifact;
use leek_manifest::BackendKind;
use leek_project::Project;
use leek_session::{DriverConfig, RecipeParams, Target, run_entry};
use leek_syntax::version::version_from_byte;

use crate::cli::{Build, ColorWhen, MessageFormat};

pub fn run(
    args: &Build,
    manifest_path: Option<&Path>,
    color: ColorWhen,
    format: MessageFormat,
    quiet: bool,
    verbose: bool,
    environment: Option<&std::sync::Arc<dyn leek_environment::EnvironmentCatalog>>,
) -> Result<ExitCode> {
    let project = Project::discover(manifest_path)?;
    if leek_session::report_manifest(&project, color.into(), format.into()) {
        return Ok(ExitCode::from(1));
    }

    let backend = resolve_backend(&project.manifest, args.backend.as_deref())?;

    // Java *exact* mode must mirror the upstream reference compiler's emission
    // shape, so it keeps the IR source-faithful (O0). Every other build path —
    // Java clean, native — folds constants to shrink the program's op budget.
    let clean_java = matches!(backend, BackendKind::Java)
        && java_clean_mode(
            args.clean,
            &project.manifest.backend.java.clone().unwrap_or_default(),
        );
    // Java *exact* and the LeekScript source backend keep the IR
    // source-faithful (O0); the LeekScript backend runs its own opt passes
    // under `--optimize` instead. Everything else folds constants (O1).
    let opt = if (matches!(backend, BackendKind::Java) && !clean_java)
        || matches!(backend, BackendKind::LeekScript)
    {
        leek_session::OptLevel::O0
    } else {
        leek_session::OptLevel::O1
    };

    // `--verbose` times the very pipeline the plain build runs: the sink
    // rides along on the config rather than selecting a separate entry point.
    let sink = verbose.then(leek_pipeline::TimingSink::new);
    let config = DriverConfig {
        target: Target::Linted,
        params: RecipeParams::default().with_opt(opt),
        color: color.into(),
        format: format.into(),
        timing: sink.clone(),
    };
    let driver_run = run_entry(&project, &config)?;
    if let Some(sink) = &sink {
        eprintln!(
            "miku build: pipeline timings for {}:",
            project.entry_path().display()
        );
        let mut total = std::time::Duration::ZERO;
        for entry in sink.entries() {
            total += entry.duration;
            eprintln!("  {:>14}: {:?}", entry.step, entry.duration);
        }
        eprintln!("  {:>14}: {:?}", "total", total);
    }
    if driver_run.had_error {
        return Ok(ExitCode::from(1));
    }

    let version = version_from_byte(driver_run.run.input().version_byte);

    match backend {
        BackendKind::Java => emit_java(
            &project,
            &driver_run.run,
            version,
            args,
            quiet,
            environment,
            color,
            format,
        ),
        BackendKind::Native => emit_native(&project, &driver_run.run, args, quiet),
        BackendKind::LeekScript => emit_leekscript(
            &project,
            &driver_run.run,
            version,
            args,
            quiet,
            color,
            format,
        ),
        BackendKind::Jar => {
            bail!("jar backend not yet supported in this toolchain");
        }
        BackendKind::Wasm => {
            bail!("wasm backend not yet supported in this toolchain");
        }
    }
}

/// Render a backend's own diagnostics through the project's reporter — the
/// same codes, carets and `[lint]` levels a frontend diagnostic gets — and
/// report whether any of them was error-level.
///
/// Both source-emitting backends produce output *and* complaints (unlike the
/// native backend, which fails outright), so this takes a slice rather than
/// an error. Falls back to a plain one-line form when the reporter can't be
/// built from a broken `[lint]` table, exactly as `miku run` does for a
/// native failure.
fn report_backend_diagnostics(
    project: &Project,
    result: &leek_pipeline::Run<'_>,
    diagnostics: &[leek_diagnostics::Diagnostic],
    color: ColorWhen,
    format: MessageFormat,
) -> bool {
    if diagnostics.is_empty() {
        return false;
    }
    // The same source map the driver rendered the frontend diagnostics
    // against, so a backend complaint raised inside an included file points
    // at *that* file.
    let entry_label = project.entry_path().display().to_string();
    let entry_text = std::fs::read_to_string(project.entry_path()).unwrap_or_default();
    let sources = leek_session::run_sources(result, &entry_text, &entry_label);
    if let Ok(reporter) = leek_session::reporter_for(project, color.into(), format.into()) {
        return reporter.emit(diagnostics, &sources);
    }
    for d in diagnostics {
        eprintln!("{}: {}", d.severity, d.message);
    }
    diagnostics
        .iter()
        .any(|d| d.severity == leek_diagnostics::Severity::Error)
}

/// AOT-compile the project to a standalone native executable. The output path
/// is `--out-dir` if given, else `[backend.native].out_dir`, else
/// `[backend.native].out`, else `<project root>/<project name>`.
fn emit_native(
    project: &Project,
    result: &leek_pipeline::Run<'_>,
    args: &Build,
    quiet: bool,
) -> Result<ExitCode> {
    let hir = result
        .get::<HirArtifact>()
        .ok_or_else(|| anyhow::anyhow!("lowering produced no HIR"))?;
    let input = result.input();
    let settings = project.manifest.backend.native.clone().unwrap_or_default();
    let out = native_out_path(project, args.out_dir.as_deref(), &settings);
    // `out` may name a path in a directory that does not exist yet
    // (`out = "bin/app"`); the linker will not create it.
    if let Some(parent) = out.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }

    let mut opts = leek_backend_native::NativeOptions::release()
        .with_lang(input.version_byte, input.strict)
        // A standalone binary runs unbounded — no per-turn op budget.
        .with_op_limit(u64::MAX);
    crate::util::apply_native_settings(&mut opts, &project.manifest);
    leek_backend_native::aot::compile_to_executable(hir.0.as_ref(), &opts, &out, quiet)
        .with_context(|| format!("compiling native executable to {}", out.display()))?;
    Ok(ExitCode::SUCCESS)
}

/// Where the standalone native executable goes: [`pick_out_dir`]'s
/// `--out-dir` / `out_dir` precedence, with `[backend.native].out` — the
/// single-file spelling — ahead of the `<root>/<name>` default. A relative
/// `out` resolves against the project root, like every other manifest path.
fn native_out_path(
    project: &Project,
    cli_out_dir: Option<&Path>,
    settings: &leek_manifest::BackendSettings,
) -> std::path::PathBuf {
    let default = match settings.out.as_deref() {
        Some(p) if p.is_absolute() => p.to_path_buf(),
        Some(p) => project.root.join(p),
        None => project.root.join(&project.manifest.project.name),
    };
    pick_out_dir(project, cli_out_dir, settings, default)
}

/// Emit desugared official LeekScript source for the project. Writes
/// `<entry-stem>.leek` to `--out-dir`, else `[backend.leekscript].out_dir`,
/// else `<build>/leekscript`.
fn emit_leekscript(
    project: &Project,
    result: &leek_pipeline::Run<'_>,
    version: leek_syntax::Version,
    args: &Build,
    quiet: bool,
    color: ColorWhen,
    format: MessageFormat,
) -> Result<ExitCode> {
    let hir = result
        .get::<HirArtifact>()
        .ok_or_else(|| anyhow::anyhow!("lowering produced no HIR"))?;
    let input = result.input();

    let mut opts = if args.compact {
        leek_backend_leekscript::Options::compact(version)
    } else {
        leek_backend_leekscript::Options::pretty(version).with_source_text(input.text.clone())
    };
    opts = opts
        .with_optimize(args.optimize)
        .with_user_source(input.source);

    let out = leek_backend_leekscript::emit(hir.0.as_ref(), &opts);
    // A semantic this backend cannot carry across is a warning: the emitted
    // program is valid, it just means slightly less than the input did. It
    // still has to be *said* — dropping it silently is #154.
    if report_backend_diagnostics(project, result, &out.diagnostics, color, format) {
        return Ok(ExitCode::from(1));
    }

    let settings = project
        .manifest
        .backend
        .leekscript
        .clone()
        .unwrap_or_default();
    let out_dir = pick_out_dir(
        project,
        args.out_dir.as_deref(),
        &settings,
        project.build_dir().join("leekscript"),
    );
    std::fs::create_dir_all(&out_dir).with_context(|| format!("creating {}", out_dir.display()))?;

    let stem = project
        .entry_path()
        .file_stem()
        .map_or_else(|| "main".to_string(), |s| s.to_string_lossy().into_owned());
    let path = out_dir.join(format!("{stem}.leek"));
    std::fs::write(&path, &out.source).with_context(|| format!("writing {}", path.display()))?;
    if !quiet {
        eprintln!("wrote {}", path.display());
    }
    Ok(ExitCode::SUCCESS)
}

fn emit_java(
    project: &Project,
    result: &leek_pipeline::Run<'_>,
    version: leek_syntax::Version,
    args: &Build,
    quiet: bool,
    environment: Option<&std::sync::Arc<dyn leek_environment::EnvironmentCatalog>>,
    color: ColorWhen,
    format: MessageFormat,
) -> Result<ExitCode> {
    let hir = result
        .get::<HirArtifact>()
        .ok_or_else(|| anyhow::anyhow!("lowering produced no HIR"))?;

    let settings = project.manifest.backend.java.clone().unwrap_or_default();

    let clean = java_clean_mode(args.clean, &settings);
    let mut opts = if clean {
        leek_backend_java::Options::clean(version, 0)
    } else {
        leek_backend_java::Options::exact(version, 0)
    }
    .with_source_path(project.entry_path().display().to_string());
    if let Some(env) = environment {
        opts = opts.with_environment(env.clone());
    }

    let out = leek_backend_java::emit(hir.0.as_ref(), &opts);
    // A construct the emitter has no shape for produces Java that javac
    // rejects — or, worse, that compiles to something else. Say so against
    // the Leek source and stop, instead of writing the file and reporting
    // success (#152). The `.java` is deliberately not written: it is known
    // not to be a translation of this program.
    //
    // Whether a diagnostic is fatal is the reporter's call, not
    // `out.has_errors()`: a project that has decided it knows better can
    // demote `E0610` through the manifest's `[lint]` table, and then the
    // build proceeds as it did before.
    if report_backend_diagnostics(project, result, &out.diagnostics, color, format) {
        return Ok(ExitCode::from(1));
    }

    let out_dir = pick_java_out_dir(project, args.out_dir.as_deref(), &settings);
    std::fs::create_dir_all(&out_dir).with_context(|| format!("creating {}", out_dir.display()))?;

    let java_path = out_dir.join(format!("{}.java", out.class_name));
    std::fs::write(&java_path, &out.java)
        .with_context(|| format!("writing {}", java_path.display()))?;
    if settings.emit_lines {
        let lines_path = out_dir.join(format!("{}.lines", out.class_name));
        std::fs::write(&lines_path, &out.lines)
            .with_context(|| format!("writing {}", lines_path.display()))?;
        if !quiet {
            eprintln!("wrote {} and {}", java_path.display(), lines_path.display());
        }
    } else if !quiet {
        eprintln!("wrote {}", java_path.display());
    }
    Ok(ExitCode::SUCCESS)
}

#[cfg(test)]
mod tests {
    use super::native_out_path;
    use leek_project::Project;
    use std::path::{Path, PathBuf};

    /// A throwaway project on disk — `Project` keeps a private index, so it
    /// can only be built by discovery.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new(label: &str, backend_native: &str) -> Self {
            let root = std::env::temp_dir().join(format!(
                "miku-out-path-{label}-{}-{:?}",
                std::process::id(),
                std::thread::current().id()
            ));
            let _ = std::fs::remove_dir_all(&root);
            std::fs::create_dir_all(root.join("src")).expect("scratch dir");
            std::fs::write(
                root.join("Miku.toml"),
                format!("[project]\nname = \"demo\"\nversion = \"0.1.0\"\n{backend_native}"),
            )
            .expect("write manifest");
            std::fs::write(root.join("src/main.leek"), "return 1;\n").expect("write entry");
            Self(root)
        }

        fn project(&self) -> Project {
            Project::discover(Some(&self.0.join("Miku.toml"))).expect("discover")
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn settings(p: &Project) -> leek_manifest::BackendSettings {
        p.manifest.backend.native.clone().unwrap_or_default()
    }

    #[test]
    fn defaults_to_the_project_name_at_the_root() {
        let s = Scratch::new("default", "");
        let p = s.project();
        assert_eq!(
            native_out_path(&p, None, &settings(&p)),
            p.root.join("demo")
        );
    }

    #[test]
    fn a_relative_manifest_out_resolves_against_the_root() {
        let s = Scratch::new(
            "relative",
            "[backend.native]\nenable = true\nout = \"bin/app\"\n",
        );
        let p = s.project();
        assert_eq!(
            native_out_path(&p, None, &settings(&p)),
            p.root.join("bin/app")
        );
    }

    #[test]
    fn an_absolute_manifest_out_is_taken_as_is() {
        let s = Scratch::new(
            "absolute",
            "[backend.native]\nenable = true\nout = \"/elsewhere/app\"\n",
        );
        let p = s.project();
        assert_eq!(
            native_out_path(&p, None, &settings(&p)),
            PathBuf::from("/elsewhere/app")
        );
    }

    #[test]
    fn out_dir_and_the_cli_flag_both_win_over_out() {
        let s = Scratch::new(
            "precedence",
            "[backend.native]\nenable = true\nout = \"bin/app\"\nout_dir = \"dist/app\"\n",
        );
        let p = s.project();
        assert_eq!(
            native_out_path(&p, None, &settings(&p)),
            p.root.join("dist/app")
        );
        assert_eq!(
            native_out_path(&p, Some(Path::new("cli/out")), &settings(&p)),
            p.root.join("cli/out")
        );
    }
}
