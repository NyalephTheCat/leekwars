//! Backend selection helpers shared by `miku` and other drivers.

mod linked;

use std::path::{Path, PathBuf};

pub use linked::{is_linked, LINKED};

use anyhow::{Result, bail};
use leek_manifest::{BackendKind, BackendSettings, JavaMode, Manifest};
use leek_project::Project;
use leek_syntax::Version;

/// Resolve which backend to use from the manifest and an optional CLI override.
pub fn resolve_backend(manifest: &Manifest, cli_backend: Option<&str>) -> Result<BackendKind> {
    if let Some(raw) = cli_backend {
        return BackendKind::parse(raw).ok_or_else(|| {
            anyhow::anyhow!(
                "unknown backend `{raw}` (expected one of: java, jar, native, interp, wasm)"
            )
        });
    }
    manifest.backend.default_kind().ok_or_else(|| {
        anyhow::anyhow!(
            "no backend selected — set `[backend.<kind>].default = true` in Miku.toml or pass --backend"
        )
    })
}

/// `miku run` only supports the interpreter today.
pub fn resolve_run_backend(cli_backend: Option<&str>) -> Result<()> {
    if let Some(raw) = cli_backend {
        match BackendKind::parse(raw) {
            Some(BackendKind::Interp) | None if raw == "interp" => Ok(()),
            Some(BackendKind::Native) => {
                bail!("native backend not yet supported in this toolchain")
            }
            Some(_) => bail!("`miku run` only supports the interpreter backend in this toolchain"),
            None => bail!("unknown backend `{raw}`"),
        }
    } else {
        Ok(())
    }
}

/// Output directory for emitted Java sources.
pub fn pick_java_out_dir(
    project: &Project,
    cli_out_dir: Option<&Path>,
    settings: &BackendSettings,
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
    project.build_dir().join("java")
}

/// Map a pipeline version byte to [`Version`].
pub fn version_from_byte(byte: u8) -> Version {
    match byte {
        1 => Version::V1,
        2 => Version::V2,
        3 => Version::V3,
        _ => Version::V4,
    }
}

/// Whether to use clean Java emission mode.
pub fn java_clean_mode(args_clean: bool, settings: &BackendSettings) -> bool {
    args_clean || settings.java_mode == Some(JavaMode::Clean)
}
