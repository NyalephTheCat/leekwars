//! Diagnostic emission for CLI tools (`miku`, test runners, etc.).

use std::io::IsTerminal;

use leek_span::LineTable;

use crate::{Code, Diagnostic, Renderer, Severity, SeverityConfig, codes::CATALOG};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ColorWhen {
    #[default]
    Auto,
    Always,
    Never,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MessageFormat {
    #[default]
    Human,
    Json,
    Junit,
}

/// Manifest `[lint]` table levels (string codes).
#[derive(Clone, Copy)]
pub struct LintLevels<'a> {
    pub deny: &'a [String],
    pub warn: &'a [String],
    pub allow: &'a [String],
}

/// Render config for one tool invocation.
pub struct Reporter {
    severity: SeverityConfig,
    renderer: Renderer,
    format: MessageFormat,
}

impl Reporter {
    pub fn new(
        color_when: ColorWhen,
        format: MessageFormat,
        lint: LintLevels<'_>,
    ) -> Result<Self, String> {
        let mut severity = SeverityConfig::new();
        for raw in lint.deny {
            severity.deny(resolve_code(raw)?);
        }
        for raw in lint.warn {
            severity.warn(resolve_code(raw)?);
        }
        for raw in lint.allow {
            severity.allow(resolve_code(raw)?);
        }
        let want_color = matches!(format, MessageFormat::Human) && should_color(color_when);
        let renderer = if want_color {
            Renderer::ansi()
        } else {
            Renderer::default()
        };
        Ok(Self {
            severity,
            renderer,
            format,
        })
    }

    pub fn emit_run(
        &self,
        diagnostics: &[Diagnostic],
        source_text: &str,
        file_label: &str,
    ) -> bool {
        let line_table = LineTable::new(source_text);
        let mut had_error = false;
        for diag in diagnostics {
            let mut adjusted = diag.clone();
            if !self.severity.apply_mut(&mut adjusted) {
                continue;
            }
            match self.format {
                MessageFormat::Human | MessageFormat::Junit => {
                    let rendered =
                        self.renderer
                            .render(&adjusted, source_text, file_label, &line_table);
                    eprint!("{rendered}");
                }
                MessageFormat::Json => {
                    #[cfg(feature = "serde")]
                    {
                        match serde_json::to_string(&adjusted) {
                            Ok(json) => println!("{json}"),
                            Err(e) => eprintln!("failed to encode diagnostic as JSON: {e}"),
                        }
                    }
                    #[cfg(not(feature = "serde"))]
                    {
                        let _ = adjusted;
                        eprintln!("JSON diagnostics require the `serde` feature");
                    }
                }
            }
            had_error |= matches!(adjusted.severity, Severity::Error);
        }
        had_error
    }
}

fn should_color(when: ColorWhen) -> bool {
    match when {
        ColorWhen::Always => true,
        ColorWhen::Never => false,
        ColorWhen::Auto => {
            if std::env::var_os("NO_COLOR").is_some() {
                return false;
            }
            std::io::stderr().is_terminal()
        }
    }
}

fn resolve_code(raw: &str) -> Result<Code, String> {
    if let Some(meta) = CATALOG.iter().find(|m| m.id == raw || m.name == raw) {
        Ok(Code(meta.id))
    } else {
        Err(format!("unknown diagnostic code `{raw}`"))
    }
}
