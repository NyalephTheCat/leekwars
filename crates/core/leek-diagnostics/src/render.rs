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
        out.push_str(&self.faint(&format!("  --> {file}:{}:{}\n", lc.line, lc.col)));
        self.snippet(
            diag.span,
            &diag.message,
            source,
            lines,
            &mut out,
            /*is_primary=*/ true,
            diag.severity,
        );

        for label in &diag.labels {
            out.push('\n');
            let lc = lines.line_col(label.span.start);
            out.push_str(&self.faint(&format!("  --> {file}:{}:{}\n", lc.line, lc.col)));
            self.snippet(
                label.span,
                &label.message,
                source,
                lines,
                &mut out,
                /*is_primary=*/ false,
                Severity::Info,
            );
        }

        for note in &diag.notes {
            writeln!(out, "  {} {note}", self.bold("note:")).unwrap();
        }

        for sug in &diag.suggestions {
            self.suggestion(sug, source, lines, &mut out);
        }

        // Point at the extended write-up when one exists, mirroring
        // rustc's "For more information about this error, try ...".
        if diag.code.explain().is_some() {
            out.push_str(&self.faint(&format!(
                "  = for more information, try `miku explain {}`\n",
                diag.code.0
            )));
        }

        out
    }

    fn header(&self, diag: &Diagnostic, out: &mut String) {
        let sev = match diag.severity {
            Severity::Error => self.color("error", AnsiColor::Red),
            Severity::Warning => self.color("warning", AnsiColor::Yellow),
            Severity::Info => self.color("info", AnsiColor::Blue),
            Severity::Hint => self.color("hint", AnsiColor::Cyan),
        };
        writeln!(
            out,
            "{sev}[{}]: {}",
            self.bold(diag.code.0),
            self.bold(&diag.message),
        )
        .unwrap();
    }

    #[allow(clippy::too_many_arguments)]
    fn snippet(
        &self,
        span: Span,
        label: &str,
        source: &str,
        lines: &LineTable,
        out: &mut String,
        is_primary: bool,
        severity: Severity,
    ) {
        let lc = lines.line_col(span.start);
        let line_idx = (lc.line - 1) as usize;
        let col_idx = (lc.col - 1) as usize;
        let line_text = lines.line_text(source, line_idx).unwrap_or("");
        let gutter_w = line_no_width(lc.line);
        let gutter = " ".repeat(gutter_w);
        // Top rule.
        out.push_str(&self.faint(&format!("{gutter} |\n")));
        // Source line.
        out.push_str(&self.faint(&format!("{:>w$} | ", lc.line, w = gutter_w)));
        out.push_str(line_text);
        out.push('\n');
        // Caret underline.
        let span_len = (span.end - span.start) as usize;
        let span_len = span_len.max(1);
        // Clamp underline so it doesn't run past the end of the line.
        let remaining = line_text.len().saturating_sub(col_idx);
        let underline_len = span_len.min(remaining.max(1));
        let caret_char = if is_primary { '^' } else { '-' };
        let caret_str: String = std::iter::repeat_n(caret_char, underline_len).collect();
        let pad = " ".repeat(col_idx);
        let colored_caret = if is_primary {
            self.severity_color(&caret_str, severity)
        } else {
            self.faint(&caret_str)
        };
        out.push_str(&self.faint(&format!("{gutter} | ")));
        out.push_str(&pad);
        out.push_str(&colored_caret);
        if !label.is_empty() {
            out.push(' ');
            let label_colored = if is_primary {
                self.severity_color(label, severity)
            } else {
                self.faint(label)
            };
            out.push_str(&label_colored);
        }
        out.push('\n');
    }

    fn suggestion(&self, sug: &Suggestion, source: &str, lines: &LineTable, out: &mut String) {
        writeln!(out, "  {} {}", self.bold("help:"), sug.message).unwrap();
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
            out.push_str(&self.faint(&format!("{:>w$} | {after}\n", lc.line, w = gutter_w)));
        }
    }

    // ---- color helpers ----

    fn color(&self, s: &str, c: AnsiColor) -> String {
        if self.style.ansi {
            format!("\x1b[{}m{}\x1b[0m", c.code(), s)
        } else {
            s.to_string()
        }
    }

    fn bold(&self, s: &str) -> String {
        if self.style.ansi {
            format!("\x1b[1m{s}\x1b[0m")
        } else {
            s.to_string()
        }
    }

    fn faint(&self, s: &str) -> String {
        if self.style.ansi {
            format!("\x1b[2m{s}\x1b[0m")
        } else {
            s.to_string()
        }
    }

    fn severity_color(&self, s: &str, sev: Severity) -> String {
        let c = match sev {
            Severity::Error => AnsiColor::Red,
            Severity::Warning => AnsiColor::Yellow,
            Severity::Info => AnsiColor::Blue,
            Severity::Hint => AnsiColor::Cyan,
        };
        self.color(s, c)
    }
}

#[derive(Debug, Clone, Copy)]
enum AnsiColor {
    Red,
    Yellow,
    Blue,
    Cyan,
}

impl AnsiColor {
    fn code(self) -> &'static str {
        match self {
            AnsiColor::Red => "31",
            AnsiColor::Yellow => "33",
            AnsiColor::Blue => "34",
            AnsiColor::Cyan => "36",
        }
    }
}

fn line_no_width(line_no: u32) -> usize {
    let mut n = line_no.max(1);
    let mut w = 0;
    while n > 0 {
        w += 1;
        n /= 10;
    }
    w.max(2)
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
}
