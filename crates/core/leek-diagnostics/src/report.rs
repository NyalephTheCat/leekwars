//! Diagnostic emission for CLI tools (`miku`, test runners, etc.).

use std::io::IsTerminal;

use leek_span::LineTable;

use crate::{Code, Diagnostic, Renderer, Severity, SeverityConfig};

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
}

/// Manifest `[lint]` table levels (string codes).
#[derive(Clone, Copy)]
pub struct LintLevels<'a> {
    pub deny: &'a [String],
    pub warn: &'a [String],
    pub allow: &'a [String],
}

/// A `[lint]` entry the catalog can't resolve.
///
/// Carries the raw text and the nearest catalog code so the caller can render
/// it wherever it has a span for — the manifest key it came from, or a bare
/// message when there is none.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LintLevelError {
    UnknownCode {
        /// The `deny`/`warn`/`allow` entry exactly as the manifest spelled it.
        raw: String,
        /// Nearest catalog id or name, when one is close enough to suggest.
        suggestion: Option<String>,
    },
}

impl LintLevelError {
    /// The raw entry that failed to resolve — what a caller matches against
    /// the manifest text to find the span to point at.
    pub fn raw(&self) -> &str {
        match self {
            LintLevelError::UnknownCode { raw, .. } => raw,
        }
    }
}

impl std::fmt::Display for LintLevelError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LintLevelError::UnknownCode { raw, suggestion } => {
                write!(f, "unknown diagnostic code `{raw}`")?;
                if let Some(hint) = suggestion {
                    write!(f, " (did you mean `{hint}`?)")?;
                }
                Ok(())
            }
        }
    }
}

impl std::error::Error for LintLevelError {}

impl SeverityConfig {
    /// Build the overrides for a manifest `[lint]` table. Errors on a
    /// code the catalog doesn't know.
    pub fn from_levels(lint: LintLevels<'_>) -> Result<Self, LintLevelError> {
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
        Ok(severity)
    }
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
    ) -> Result<Self, LintLevelError> {
        let severity = SeverityConfig::from_levels(lint)?;
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

    /// The diagnostics this reporter would emit, with the manifest's
    /// `[lint]` levels applied: allowed codes dropped, denied/warned
    /// codes re-leveled. Tools that act on diagnostics without
    /// rendering them (`miku fix`, `miku test` expectations) filter
    /// through this so they agree with what `check`/`lint` show.
    pub fn apply_levels(&self, diagnostics: &[Diagnostic]) -> Vec<Diagnostic> {
        self.severity.apply_all(diagnostics)
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
                MessageFormat::Human => {
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

fn resolve_code(raw: &str) -> Result<Code, LintLevelError> {
    Code::resolve(raw).ok_or_else(|| LintLevelError::UnknownCode {
        raw: raw.to_string(),
        // Match against ids *and* names — a manifest may spell either.
        suggestion: crate::best_match(
            raw,
            crate::codes::CATALOG.iter().flat_map(|m| [m.id, m.name]),
        )
        .map(str::to_string),
    })
}
