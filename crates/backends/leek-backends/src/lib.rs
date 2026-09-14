//! Backend selection helpers shared by `miku` and other drivers.

mod linked;

use std::path::{Path, PathBuf};

pub use linked::{LINKED, is_linked};

use anyhow::{Result, bail};
use leek_manifest::{BackendKind, BackendSettings, JavaMode, Manifest};
use leek_project::Project;
use leek_syntax::Version;

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

/// Map a pipeline version byte to [`Version`].
pub fn version_from_byte(byte: u8) -> Version {
    Version::from_byte(byte)
}

/// Whether to use clean Java emission mode.
pub fn java_clean_mode(args_clean: bool, settings: &BackendSettings) -> bool {
    args_clean || settings.java_mode == Some(JavaMode::Clean)
}
