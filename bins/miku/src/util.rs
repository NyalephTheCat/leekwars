//! Small adapters between CLI flags and shared library types.
//!
//! The `Reporter` itself is built by [`leek_driver::reporter_for`], so
//! every subcommand picks up the manifest's `[lint]` levels the same way.

use leek_backend_native::{NativeOptions, OptLevel};
use leek_diagnostics::{ColorWhen as DiagColor, Diagnostic, MessageFormat as DiagFormat, Sources};
use leek_manifest::{Manifest, NativeOptLevel};
use leek_project::Project;

use crate::cli::{ColorWhen, MessageFormat};

/// Fold `[backend.native]` into `opts`.
///
/// `build`, `run` and `test` each build their own `NativeOptions` profile and
/// each used to copy the `max_call_depth` block. Folding them here is what
/// makes a new knob — `opt_level` is the first — reach all three instead of
/// two out of three.
///
/// A key the manifest leaves out leaves `opts` alone, so each caller's
/// profile (`release()` for AOT, `jit_for_input` for the JIT) is unchanged
/// for a project that configures nothing.
pub fn apply_native_settings(opts: &mut NativeOptions, manifest: &Manifest) {
    let Some(settings) = manifest.backend.native.as_ref() else {
        return;
    };
    if let Some(depth) = settings.max_call_depth {
        opts.max_call_depth = depth;
    }
    if let Some(level) = settings.opt_level {
        opts.opt_level = match level {
            NativeOptLevel::None => OptLevel::None,
            NativeOptLevel::Speed => OptLevel::Speed,
            NativeOptLevel::SpeedAndSize => OptLevel::SpeedAndSize,
        };
    }
}

/// Render `diagnostics` through the project's `Reporter`, so a backend
/// finding gets the same `-->` header, source line and caret a frontend
/// diagnostic gets — and the same `[lint]` levels applied to it.
///
/// Returns `false` when the reporter could not be built (a broken `[lint]`
/// table, which `report_manifest` has already complained about) and nothing
/// was printed, leaving the caller to say what it wants in that case.
#[must_use]
pub fn report_diagnostics(
    project: &Project,
    diagnostics: &[Diagnostic],
    sources: &Sources,
    color: ColorWhen,
    format: MessageFormat,
) -> bool {
    match leek_driver::reporter_for(project, color.into(), format.into()) {
        Ok(reporter) => {
            reporter.emit(diagnostics, sources);
            true
        }
        Err(_) => false,
    }
}

impl From<ColorWhen> for DiagColor {
    fn from(c: ColorWhen) -> Self {
        match c {
            ColorWhen::Auto => DiagColor::Auto,
            ColorWhen::Always => DiagColor::Always,
            ColorWhen::Never => DiagColor::Never,
        }
    }
}

impl From<MessageFormat> for DiagFormat {
    fn from(f: MessageFormat) -> Self {
        match f {
            MessageFormat::Human => DiagFormat::Human,
            MessageFormat::Json => DiagFormat::Json,
            // JUnit is a test-report format produced by `miku test`
            // itself; per-diagnostic rendering uses the human renderer.
            MessageFormat::Junit => DiagFormat::Human,
        }
    }
}
