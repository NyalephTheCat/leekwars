//! Small adapters between CLI flags and shared library types.
//!
//! The `Reporter` itself is built by [`leek_driver::reporter_for`], so
//! every subcommand picks up the manifest's `[lint]` levels the same way.

use leek_diagnostics::{ColorWhen as DiagColor, MessageFormat as DiagFormat};

use crate::cli::{ColorWhen, MessageFormat};

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
