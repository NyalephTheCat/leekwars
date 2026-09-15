//! What every driver needs to *reach* a backend, in one place.
//!
//! Which backend to use ([`resolve_backend`]), where it writes
//! ([`pick_out_dir`]), which mode ([`java_clean_mode`]), what
//! optimization level its input is lowered at ([`opt_level`]), and the
//! option assembly the two source backends share ([`java_options`],
//! [`leekscript_options`]).
//!
//! `leekc` and `miku build` emit the same programs through the same
//! backends and each used to answer all of that separately, which meant
//! they could disagree — and about the optimization level they did, so
//! one source file produced two different Java programs with two
//! different `ops(…)` counts depending on which driver compiled it
//! (ARCH-13). Everything here is a question exactly one function answers.
//!
//! What stays at a call site is what genuinely differs between the
//! drivers: where the output goes, and the flags only one of them has.

mod linked;
mod options;

use std::path::{Path, PathBuf};

pub use linked::{LINKED, is_linked};
pub use options::{java_options, leekscript_options};

use anyhow::{Result, bail};
use leek_manifest::{BackendKind, BackendSettings, JavaMode, Manifest};
use leek_project::Project;
use leek_query::OptLevel;

/// Resolve which backend to use from the manifest and an optional CLI override.
pub fn resolve_backend(manifest: &Manifest, cli_backend: Option<&str>) -> Result<BackendKind> {
    if let Some(raw) = cli_backend {
        return BackendKind::parse(raw).ok_or_else(|| {
            anyhow::anyhow!(
                "unknown backend `{raw}` (expected one of: java, jar, native, wasm, leekscript)"
            )
        });
    }
    manifest.backend.default_kind().ok_or_else(|| {
        anyhow::anyhow!(
            "no backend selected — set `[backend.<kind>].default = true` in Miku.toml or pass --backend"
        )
    })
}

/// `miku run` executes via the native JIT. The optional `--backend` override is
/// accepted only when it names the native backend; any other backend belongs to
/// `miku build`.
pub fn resolve_run_backend(cli_backend: Option<&str>) -> Result<()> {
    match cli_backend {
        None | Some("native") => Ok(()),
        Some(raw) => match BackendKind::parse(raw) {
            Some(_) => bail!(
                "`miku run` executes via the native backend; `--backend {raw}` is not supported (use `miku build --backend {raw}`)"
            ),
            None => bail!("unknown backend `{raw}`"),
        },
    }
}

/// Where a backend writes: the `--out-dir` override, else the manifest's
/// `[backend.<kind>].out_dir`, else `default`.
///
/// A relative path from either override resolves against the project root,
/// never the process CWD — `miku build --out-dir out` has to land in the
/// same place whichever directory of the project it was run from, and
/// whichever backend it selected.
pub fn pick_out_dir(
    project: &Project,
    cli_out_dir: Option<&Path>,
    settings: &BackendSettings,
    default: PathBuf,
) -> PathBuf {
    if let Some(dir) = cli_out_dir {
        return if dir.is_absolute() {
            dir.to_path_buf()
        } else {
            project.root.join(dir)
        };
    }
    if let Some(dir) = &settings.out_dir {
        return if dir.is_absolute() {
            dir.clone()
        } else {
            project.root.join(dir)
        };
    }
    default
}

/// Output directory for emitted Java sources — [`pick_out_dir`] with the
/// Java backend's `<build>/java` default.
pub fn pick_java_out_dir(
    project: &Project,
    cli_out_dir: Option<&Path>,
    settings: &BackendSettings,
) -> PathBuf {
    pick_out_dir(
        project,
        cli_out_dir,
        settings,
        project.build_dir().join("java"),
    )
}

/// Whether to use clean Java emission mode.
pub fn java_clean_mode(args_clean: bool, settings: &BackendSettings) -> bool {
    args_clean || settings.java_mode == Some(JavaMode::Clean)
}

/// The [`OptLevel`] a backend's input is lowered at.
///
/// One policy, because two drivers emitting the same program through the
/// same backend have to agree. They did not: `miku build --backend java`
/// with `mode = "clean"` folded, and `leekc --emit java --clean` — which
/// compiled with the default parameters and never consulted this rule —
/// did not, so the two emitted different Java, with different `ops(…)`
/// counts, for one source file (ARCH-13).
///
/// The rule:
///
/// * **Java *exact*** keeps the IR source-faithful. Its whole contract is
///   reproducing the upstream reference compiler's emission shape, and an
///   optimizer that is right about the program is still wrong about the
///   bytes.
/// * **The LeekScript source backend** keeps it too: it runs its own
///   semantics-preserving passes under `--optimize`, and folding first
///   would emit source the author did not write whether or not they
///   asked.
/// * **Everything else** — Java *clean*, native, and the backends not
///   implemented yet — folds constants, which shrinks the program's
///   static op budget. That budget is what LeekWars charges for, so this
///   is the difference between a program that fits its turn and one that
///   does not.
///
/// `java_clean` is only read for [`BackendKind::Java`]; pass
/// [`java_clean_mode`]'s answer.
#[must_use]
pub fn opt_level(backend: BackendKind, java_clean: bool) -> OptLevel {
    match backend {
        BackendKind::Java if !java_clean => OptLevel::O0,
        BackendKind::LeekScript => OptLevel::O0,
        _ => OptLevel::O1,
    }
}

#[cfg(test)]
mod opt_level_tests {
    use super::{BackendKind, OptLevel, opt_level};

    #[test]
    fn only_the_byte_faithful_backends_keep_the_ir_as_written() {
        assert_eq!(opt_level(BackendKind::Java, false), OptLevel::O0);
        assert_eq!(opt_level(BackendKind::LeekScript, false), OptLevel::O0);
        assert_eq!(opt_level(BackendKind::LeekScript, true), OptLevel::O0);
    }

    #[test]
    fn clean_java_folds_where_exact_java_does_not() {
        // The divergence this exists to close: one flag, two answers, and
        // the driver that forgot to ask emitted the other one.
        assert_eq!(opt_level(BackendKind::Java, true), OptLevel::O1);
        assert_ne!(
            opt_level(BackendKind::Java, true),
            opt_level(BackendKind::Java, false)
        );
    }

    #[test]
    fn the_executing_backends_fold() {
        for backend in [BackendKind::Native, BackendKind::Jar, BackendKind::Wasm] {
            assert_eq!(opt_level(backend, false), OptLevel::O1, "{backend:?}");
        }
    }
}
