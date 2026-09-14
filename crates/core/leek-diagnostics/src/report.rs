//! Diagnostic emission for CLI tools (`miku`, test runners, etc.).

use std::io::IsTerminal;

use leek_span::Span;

use crate::{Code, Diagnostic, Renderer, Severity, SeverityConfig, Sources};

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
    /// (included files) so a diagnostic raised in an included file — or a
    /// *label* pointing into one — renders against that file's own text
    /// and name instead of the entry's. A span whose `SourceId` matches no
    /// registered source renders no snippet at all.
    ///
    /// The entry file's own `SourceId` is inferred from the diagnostics,
    /// since this signature never carried it. Callers that know it (they
    /// have the `Input`) should build a [`Sources`] and call
    /// [`emit`](Self::emit) instead.
    pub fn emit_run_sources(
        &self,
        diagnostics: &[Diagnostic],
        source_text: &str,
        file_label: &str,
        extra_sources: &[RunSource<'_>],
    ) -> bool {
        let sources = run_sources(diagnostics, source_text, file_label, extra_sources);
        self.emit(diagnostics, &sources)
    }

    /// Apply this reporter's lint levels and render every surviving
    /// diagnostic against `sources`, returning the text [`emit`](Self::emit)
    /// would print in [`MessageFormat::Human`].
    ///
    /// Split out from printing so callers — and tests asserting that a
    /// diagnostic points at the right file and column — can inspect the
    /// rendered form instead of capturing stderr.
    pub fn render_all(&self, diagnostics: &[Diagnostic], sources: &Sources) -> String {
        let mut out = String::new();
        for diag in diagnostics {
            let mut adjusted = diag.clone();
            if !self.severity.apply_mut(&mut adjusted) {
                continue;
            }
            out.push_str(&self.renderer.render(&adjusted, sources));
        }
        out
    }

    /// Emit `diagnostics` against `sources` in this reporter's format,
    /// returning whether any survived at error level.
    pub fn emit(&self, diagnostics: &[Diagnostic], sources: &Sources) -> bool {
        let mut had_error = false;
        for diag in diagnostics {
            let mut adjusted = diag.clone();
            if !self.severity.apply_mut(&mut adjusted) {
                continue;
            }
            // Emitting is this type's whole job, and the streams it writes to
            // are part of its contract: rendered diagnostics on stderr,
            // `--message-format=json` records on stdout so a caller can pipe
            // them. The two `eprintln!`s below report a failure *to emit*,
            // which has nowhere better to go — `emit` answers `bool`, not a
            // `Result`. Giving it a `&mut dyn Write` sink instead is the
            // right fix and reaches every caller; see #178.
            #[allow(clippy::print_stdout, clippy::print_stderr)]
            match self.format {
                MessageFormat::Human => {
                    eprint!("{}", self.renderer.render(&adjusted, sources));
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

/// The [`Sources`] behind [`Reporter::emit_run_sources`]: every extra, plus
/// the entry text under the entry's own `SourceId`.
///
/// That id is not a parameter of the old signature, so it is recovered from
/// the diagnostics: the first span that belongs to neither an extra nor a
/// sentinel. Registering the entry under a sentinel instead would put a
/// caret at byte 0 of the entry file for every location-less diagnostic,
/// which is the failure `Span::MANIFEST_SOURCE` exists to prevent.
fn run_sources(
    diagnostics: &[Diagnostic],
    source_text: &str,
    file_label: &str,
    extra_sources: &[RunSource<'_>],
) -> Sources {
    let mut sources = Sources::new();
    for extra in extra_sources {
        sources.push(extra.source, extra.label, extra.text);
    }
    let entry_id = diagnostics
        .iter()
        .flat_map(|d| std::iter::once(d.span.source).chain(d.labels.iter().map(|l| l.span.source)))
        .find(|id| {
            *id != Span::SYNTHETIC_SOURCE
                && *id != Span::MANIFEST_SOURCE
                && !extra_sources.iter().any(|s| s.source == *id)
        });
    if let Some(id) = entry_id {
        sources.push(id, file_label, source_text);
    } else if diagnostics
        .iter()
        .any(|d| d.span.source == Span::MANIFEST_SOURCE)
    {
        // Manifest diagnostics: `source_text` *is* the manifest.
        sources.push(Span::MANIFEST_SOURCE, file_label, source_text);
    }
    sources
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::SourceMap;
    use leek_span::SourceId;

    const NO_LINT: LintLevels<'static> = LintLevels {
        deny: &[],
        warn: &[],
        allow: &[],
    };

    fn reporter() -> Reporter {
        Reporter::new(ColorWhen::Never, MessageFormat::Human, NO_LINT).unwrap()
    }

    fn id(n: u32) -> SourceId {
        SourceId::new(n).unwrap()
    }

    #[test]
    fn render_all_points_a_label_at_the_included_file_it_belongs_to() {
        let entry = "include('inc')\nvar x = 1\n";
        let include = "var x = 2\n";
        let sources = Sources::single(id(1), "main.leek", entry).with(id(2), "inc.leek", include);
        let diag = Diagnostic::error(
            crate::codes::REDECLARED_SYMBOL,
            Span::new(id(1), 19, 20),
            "`x` is already declared",
        )
        .with_label(Span::new(id(2), 4, 5), "first declared here");
        let out = reporter().render_all(&[diag], &sources);
        assert!(out.contains("--> main.leek:2:5"), "{out}");
        assert!(out.contains("--> inc.leek:1:5"), "{out}");
        assert!(out.contains("var x = 2"), "{out}");
    }

    #[test]
    fn run_sources_recovers_the_entry_id_without_claiming_a_sentinel() {
        let diags = [Diagnostic::error(
            Code("E0200"),
            Span::new(id(7), 0, 1),
            "boom",
        )];
        let sources = run_sources(&diags, "abc\n", "main.leek", &[]);
        assert!(sources.get(id(7)).is_some());
        assert!(sources.get(Span::SYNTHETIC_SOURCE).is_none());

        // Nothing but a synthetic span: the entry is not registered under
        // the sentinel, so the diagnostic renders no misplaced caret.
        let diags = [Diagnostic::error(Code("E0200"), Span::synthetic(), "boom")];
        let sources = run_sources(&diags, "abc\n", "main.leek", &[]);
        assert!(sources.is_empty());
        let out = reporter().render_all(&diags, &sources);
        assert!(!out.contains("-->"), "{out}");
        assert!(!out.contains("abc"), "{out}");
    }

    #[test]
    fn run_sources_registers_the_manifest_under_its_sentinel() {
        let diags = [Diagnostic::error(
            Code("E0200"),
            Span::new(Span::MANIFEST_SOURCE, 0, 4),
            "bad key",
        )];
        let sources = run_sources(&diags, "[lint]\n", "Miku.toml", &[]);
        assert!(sources.get(Span::MANIFEST_SOURCE).is_some());
        let out = reporter().render_all(&diags, &sources);
        assert!(out.contains("--> Miku.toml:1:1"), "{out}");
    }
}
