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

/// One additional source file diagnostics may point into (an included
/// file), for [`Reporter::emit_run_sources`].
#[derive(Clone, Copy)]
pub struct RunSource<'a> {
    pub source: leek_span::SourceId,
    pub text: &'a str,
    pub label: &'a str,
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
        self.emit_run_sources(diagnostics, source_text, file_label, &[])
    }

    /// Like [`emit_run`](Self::emit_run), but with extra named sources
    /// (included files) so a diagnostic raised in an included file
    /// renders against *that* file's text and label instead of the
    /// entry's. A diagnostic whose `SourceId` matches none of the
    /// extras falls back to the entry text — the single-file behavior.
    pub fn emit_run_sources(
        &self,
        diagnostics: &[Diagnostic],
        source_text: &str,
        file_label: &str,
        extra_sources: &[RunSource<'_>],
    ) -> bool {
        let line_table = LineTable::new(source_text);
        let extra_tables: Vec<LineTable> = extra_sources
            .iter()
            .map(|s| LineTable::new(s.text))
            .collect();
        let mut had_error = false;
        for diag in diagnostics {
            let mut adjusted = diag.clone();
            if !self.severity.apply_mut(&mut adjusted) {
                continue;
            }
            match self.format {
                MessageFormat::Human | MessageFormat::Junit => {
                    let (text, label, table) = extra_sources
                        .iter()
                        .position(|s| s.source == adjusted.span.source)
                        .map_or((source_text, file_label, &line_table), |i| {
                            (
                                extra_sources[i].text,
                                extra_sources[i].label,
                                &extra_tables[i],
                            )
                        });
                    let rendered = self.renderer.render(&adjusted, text, label, table);
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
