//! Pretty-printed rendering for diagnostics.
//!
//! No external dependencies — hand-rolled snippet rendering with line
//! numbers, carets pointing at the span, secondary labels, notes, and
//! suggestion previews. ANSI color is optional via [`Style`].

use std::fmt::Write as _;

use leek_span::{LineTable, Span};

use crate::{Diagnostic, Severity, Suggestion};

/// Renderer for diagnostics. Defaults to no color (works in
/// pipelines and tests); enable [`Style::ansi`] for terminal output.
#[derive(Debug, Clone, Copy, Default)]
pub struct Renderer {
    pub style: Style,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct Style {
    /// Emit ANSI color escapes.
    pub ansi: bool,
}

/// One source line + caret underline to render — the parameters of
/// [`Renderer::snippet`] bundled so it stays a two-argument call.
#[derive(Clone, Copy)]
struct Snippet<'a> {
    span: Span,
    label: &'a str,
    is_primary: bool,
    severity: Severity,
}

/// SGR parameters used by the renderer. Bold/faint are structural;
/// colors are chosen per [`Severity`].
const BOLD: &str = "1";
const FAINT: &str = "2";

// `Renderer` is `Copy`, but methods take `&self` by Rust API convention.
#[allow(clippy::trivially_copy_pass_by_ref)]
impl Renderer {
    pub fn ansi() -> Self {
        Self {
            style: Style { ansi: true },
        }
    }

    pub fn render(&self, diag: &Diagnostic, source: &str, file: &str, lines: &LineTable) -> String {
        let mut out = String::new();
        self.header(diag, &mut out);
        let lc = lines.line_col(diag.span.start);
        self.styled(&mut out, FAINT, |o| {
            let _ = writeln!(o, "  --> {file}:{}:{}", lc.line, lc.col);
        });
        self.snippet(
            &mut out,
            Snippet {
                span: diag.span,
                label: &diag.message,
                is_primary: true,
                severity: diag.severity,
            },
            source,
            lines,
        );

        for label in &diag.labels {
            out.push('\n');
            let lc = lines.line_col(label.span.start);
            self.styled(&mut out, FAINT, |o| {
                let _ = writeln!(o, "  --> {file}:{}:{}", lc.line, lc.col);
            });
            self.snippet(
                &mut out,
                Snippet {
                    span: label.span,
                    label: &label.message,
                    is_primary: false,
                    severity: Severity::Info,
                },
                source,
                lines,
            );
        }

        for note in &diag.notes {
            out.push_str("  ");
            self.styled_str(&mut out, BOLD, "note:");
            out.push(' ');
            out.push_str(note);
            out.push('\n');
        }

        for sug in &diag.suggestions {
            self.suggestion(sug, source, lines, &mut out);
        }

        // Point at the extended write-up when one exists, mirroring
        // rustc's "For more information about this error, try ...".
        if diag.code.explain().is_some() {
            self.styled(&mut out, FAINT, |o| {
                let _ = writeln!(
                    o,
                    "  = for more information, try `miku explain {}`",
                    diag.code.0
                );
            });
        }

        out
    }

    fn header(&self, diag: &Diagnostic, out: &mut String) {
        self.styled_str(out, severity_sgr(diag.severity), diag.severity.as_str());
        out.push('[');
        self.styled_str(out, BOLD, diag.code.0);
        out.push_str("]: ");
        self.styled_str(out, BOLD, &diag.message);
        out.push('\n');
    }

    fn snippet(&self, out: &mut String, snip: Snippet<'_>, source: &str, lines: &LineTable) {
        let lc = lines.line_col(snip.span.start);
        let line_idx = (lc.line - 1) as usize;
        let col_idx = (lc.col - 1) as usize;
        let line_text = lines.line_text(source, line_idx).unwrap_or("");
        let gutter_w = line_no_width(lc.line);
        let gutter = " ".repeat(gutter_w);
        // Top rule.
        self.styled(out, FAINT, |o| {
            let _ = writeln!(o, "{gutter} |");
        });
        // Source line.
        self.styled(out, FAINT, |o| {
            let _ = write!(o, "{:>w$} | ", lc.line, w = gutter_w);
        });
        out.push_str(line_text);
        out.push('\n');
        // Caret underline.
        let span_len = ((snip.span.end - snip.span.start) as usize).max(1);
        // Clamp underline so it doesn't run past the end of the line.
        let remaining = line_text.len().saturating_sub(col_idx);
        let underline_len = span_len.min(remaining.max(1));
        let caret_char = if snip.is_primary { '^' } else { '-' };
        let caret_str: String = std::iter::repeat_n(caret_char, underline_len).collect();
        let pad = " ".repeat(col_idx);
        let caret_sgr = if snip.is_primary {
            severity_sgr(snip.severity)
        } else {
            FAINT
        };
        self.styled(out, FAINT, |o| {
            let _ = write!(o, "{gutter} | ");
        });
        out.push_str(&pad);
        self.styled_str(out, caret_sgr, &caret_str);
        if !snip.label.is_empty() {
            out.push(' ');
            self.styled_str(out, caret_sgr, snip.label);
        }
        out.push('\n');
    }

    fn suggestion(&self, sug: &Suggestion, source: &str, lines: &LineTable, out: &mut String) {
        out.push_str("  ");
        self.styled_str(out, BOLD, "help:");
        out.push(' ');
        out.push_str(&sug.message);
        out.push('\n');
        // Show the first edit as a before/after on the affected line.
        if let Some(edit) = sug.edits.first() {
            let lc = lines.line_col(edit.span.start);
            let line_idx = (lc.line - 1) as usize;
            let line_text = lines.line_text(source, line_idx).unwrap_or("");
            let line_start = lines.line_start(line_idx).unwrap_or(0);
            let s = (edit.span.start - line_start) as usize;
            let e = (edit.span.end - line_start) as usize;
            let e = e.min(line_text.len());
            let s = s.min(e);
            let mut after = String::with_capacity(line_text.len() + edit.replacement.len());
            after.push_str(&line_text[..s]);
            after.push_str(&edit.replacement);
            after.push_str(&line_text[e..]);
            let gutter_w = line_no_width(lc.line);
            self.styled(out, FAINT, |o| {
                let _ = writeln!(o, "{:>w$} | {after}", lc.line, w = gutter_w);
            });
        }
    }

    // ---- color helpers ----

    /// Write `body`'s output into `out`, wrapped in the SGR escape
    /// `sgr` when ANSI is enabled (otherwise emit it unstyled).
    fn styled(&self, out: &mut String, sgr: &str, body: impl FnOnce(&mut String)) {
        if self.style.ansi {
            out.push_str("\x1b[");
            out.push_str(sgr);
            out.push('m');
            body(out);
            out.push_str("\x1b[0m");
        } else {
            body(out);
        }
    }

    /// [`styled`](Self::styled) for the common case of a plain string.
    fn styled_str(&self, out: &mut String, sgr: &str, s: &str) {
        self.styled(out, sgr, |o| o.push_str(s));
    }
}

/// SGR color parameter for a severity's accent (header keyword and
/// primary caret).
fn severity_sgr(sev: Severity) -> &'static str {
    match sev {
        Severity::Error => "31",   // red
        Severity::Warning => "33", // yellow
        Severity::Info => "34",    // blue
        Severity::Hint => "36",    // cyan
    }
}

fn line_no_width(line_no: u32) -> usize {
    (line_no.max(1).ilog10() as usize + 1).max(2)
}

#[cfg(test)]
mod tests {
    use super::Renderer;
    use crate::{Code, Diagnostic, codes};
    use leek_span::{LineTable, SourceId, Span};

    fn src() -> SourceId {
        SourceId::new(1).unwrap()
    }

    #[test]
    fn renders_simple_error() {
        let text = "var a = 2\na = 'hello'\n";
        let lines = LineTable::new(text);
        let span = Span::new(src(), 10, 11); // the `a` on line 2
        let diag = Diagnostic::error(codes::ASSIGNMENT_INCOMPATIBLE_TYPE, span, "type mismatch");
        let out = Renderer::default().render(&diag, text, "main.leek", &lines);
        assert!(out.contains("error[E0250]"));
        assert!(out.contains("main.leek:2:1"));
        assert!(out.contains("a = 'hello'"));
        assert!(out.contains('^'));
    }

    #[test]
    fn renders_label_and_note() {
        let text = "var x = 1\nvar x = 2\n";
        let lines = LineTable::new(text);
        let primary = Span::new(src(), 14, 15); // second `x`
        let prev = Span::new(src(), 4, 5);
        let diag = Diagnostic::error(codes::REDECLARED_SYMBOL, primary, "`x` is already declared")
            .with_label(prev, "first declared here")
            .with_note("Leekscript v3+ forbids shadowing in the same scope.");
        let out = Renderer::default().render(&diag, text, "fight.leek", &lines);
        assert!(out.contains("first declared here"));
        assert!(out.contains("note:"));
    }

    #[test]
    fn renders_suggestion() {
        let text = "return damge\n";
        let lines = LineTable::new(text);
        let span = Span::new(src(), 7, 12);
        let diag =
            Diagnostic::error(Code("E0200"), span, "unknown variable `damge`").with_suggestion(
                crate::Suggestion::replace("did you mean `damage`?", span, "damage"),
            );
        let out = Renderer::default().render(&diag, text, "fight.leek", &lines);
        assert!(out.contains("help:"));
        assert!(out.contains("damage"));
    }

    /// Locks the exact ANSI byte layout (escapes + ordering) so the
    /// buffer-based color helpers stay byte-identical to the original
    /// `format!`-per-call rendering. Exercises header, primary snippet,
    /// a secondary label, a note, a suggestion preview, and the
    /// `explain` footer in one pass.
    #[test]
    fn ansi_output_is_byte_stable() {
        let text = "var a = 2\na = 'hello'\n";
        let lines = LineTable::new(text);
        let span = Span::new(src(), 10, 11);
        let diag = Diagnostic::error(codes::ASSIGNMENT_INCOMPATIBLE_TYPE, span, "type mismatch")
            .with_label(Span::new(src(), 4, 5), "previously")
            .with_note("a note")
            .with_suggestion(crate::Suggestion::replace("fix it", span, "b"));
        let out = Renderer::ansi().render(&diag, text, "main.leek", &lines);
        let expected = "\x1b[31merror\x1b[0m[\x1b[1mE0250\x1b[0m]: \x1b[1mtype mismatch\x1b[0m\n\
\x1b[2m  --> main.leek:2:1\n\x1b[0m\
\x1b[2m   |\n\x1b[0m\
\x1b[2m 2 | \x1b[0ma = 'hello'\n\
\x1b[2m   | \x1b[0m\x1b[31m^\x1b[0m \x1b[31mtype mismatch\x1b[0m\n\
\n\
\x1b[2m  --> main.leek:1:5\n\x1b[0m\
\x1b[2m   |\n\x1b[0m\
\x1b[2m 1 | \x1b[0mvar a = 2\n\
\x1b[2m   | \x1b[0m    \x1b[2m-\x1b[0m \x1b[2mpreviously\x1b[0m\n  \
\x1b[1mnote:\x1b[0m a note\n  \
\x1b[1mhelp:\x1b[0m fix it\n\
\x1b[2m 2 | b = 'hello'\n\x1b[0m\
\x1b[2m  = for more information, try `miku explain E0250`\n\x1b[0m";
        assert_eq!(out, expected);
    }
}
