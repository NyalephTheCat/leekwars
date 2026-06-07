//! Small adapters between CLI flags and shared library types.

use anyhow::Result;
use leek_diagnostics::{ColorWhen as DiagColor, LintLevels, MessageFormat as DiagFormat, Reporter};
use leek_manifest::LintTable;

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
            MessageFormat::Junit => DiagFormat::Junit,
        }
    }
}

pub fn reporter_from_cli(
    color: ColorWhen,
    format: MessageFormat,
    lint: &LintTable,
) -> Result<Reporter> {
    let levels = LintLevels {
        deny: &lint.deny,
        warn: &lint.warn,
        allow: &lint.allow,
    };
    Reporter::new(color.into(), format.into(), levels).map_err(|e| anyhow::anyhow!("{e}"))
}
