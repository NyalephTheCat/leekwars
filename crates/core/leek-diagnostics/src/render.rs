//! Pretty-printed rendering for diagnostics.
//!
//! No external dependencies — hand-rolled snippet rendering with line
//! numbers, carets pointing at the span, secondary labels, notes, and
//! suggestion previews. ANSI color is optional via [`Style`].
//!
//! Every span a diagnostic carries — the primary, each label, each
//! suggestion edit — names its own [`SourceId`], and a diagnostic may mix
//! them: a "previously declared here" label can point into an included
//! file. So the renderer resolves each span through a [`SourceMap`] rather
//! than being handed one file's text. A span whose source the map does not
//! know renders no snippet at all; borrowing another file's text to show
//! *something* is how a manifest error once grew a caret in `main.leek`.

use std::fmt::Write as _;

use leek_span::{LineTable, SourceId, Span, expand_tabs_in, offset};

use crate::{Diagnostic, Severity, Suggestion};

/// One file the renderer can resolve spans against.
#[derive(Clone, Copy)]
pub struct SourceEntry<'a> {
    /// What to print in the `--> ` header — a path, usually.
    pub name: &'a str,
    pub text: &'a str,
    pub lines: &'a LineTable,
}

/// Resolves the [`SourceId`] on a span to the file it belongs to.
///
/// Implemented by [`Sources`] for the common owned case; callers that
/// already hold the text and a [`LineTable`] can implement it over their own
/// storage instead of copying.
pub trait SourceMap {
    /// The file `id` names, or `None` when this map does not know it —
    /// a synthetic span, the manifest sentinel, or an include the caller
    /// forgot to register.
    fn get(&self, id: SourceId) -> Option<SourceEntry<'_>>;
}

/// An owned set of files to render against: the entry file plus every
/// include whose diagnostics may appear.
///
/// Built once per tool invocation and shared by every diagnostic, so a
/// label pointing into an include resolves the same way the primary does.
#[derive(Debug, Clone, Default)]
pub struct Sources {
    rows: Vec<SourceRow>,
}

#[derive(Debug, Clone)]
struct SourceRow {
    id: SourceId,
    name: String,
    text: String,
    lines: LineTable,
}

impl Sources {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The one-file map: what a tool rendering a standalone source uses.
    #[must_use]
    pub fn single(id: SourceId, name: impl Into<String>, text: impl Into<String>) -> Self {
        let mut s = Self::new();
        s.push(id, name, text);
        s
    }

    /// Register `id`. A second registration of the same id replaces the
    /// first, so a caller can add the entry file unconditionally without
    /// checking whether an include already claimed that id.
    pub fn push(&mut self, id: SourceId, name: impl Into<String>, text: impl Into<String>) {
        let text = text.into();
        let row = SourceRow {
            id,
            name: name.into(),
            lines: LineTable::new(&text),
            text,
        };
        match self.rows.iter_mut().find(|r| r.id == id) {
            Some(existing) => *existing = row,
            None => self.rows.push(row),
        }
    }

    /// Builder form of [`push`](Self::push).
    #[must_use]
    pub fn with(mut self, id: SourceId, name: impl Into<String>, text: impl Into<String>) -> Self {
        self.push(id, name, text);
        self
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.rows.len()
    }
}

impl SourceMap for Sources {
    fn get(&self, id: SourceId) -> Option<SourceEntry<'_>> {
        self.rows.iter().find(|r| r.id == id).map(|r| SourceEntry {
            name: &r.name,
            text: &r.text,
            lines: &r.lines,
        })
    }
}

/// A borrowed one-file map, for callers that already hold a [`LineTable`]
/// and would rather not copy the source to build a [`Sources`].
#[derive(Clone, Copy)]
pub struct SingleSource<'a> {
    pub id: SourceId,
    pub entry: SourceEntry<'a>,
}

impl SourceMap for SingleSource<'_> {
    fn get(&self, id: SourceId) -> Option<SourceEntry<'_>> {
        (id == self.id).then_some(self.entry)
    }
}

/// Renderer for diagnostics. Defaults to no color (works in
/// pipelines and tests); enable [`Style::ansi`] for terminal output.
#[derive(Debug, Clone, Copy, Default)]
pub struct Renderer {
    pub style: Style,
}

#[derive(Debug, Clone, Copy)]
pub struct Style {
    /// Emit ANSI color escapes.
    pub ansi: bool,
    /// Columns a tab advances to. Source lines are printed with tabs
    /// expanded to this width so the caret pad, which counts display
    /// columns, lands under the character it points at.
    pub tab_width: u32,
}

impl Default for Style {
    fn default() -> Self {
        Self {
            ansi: false,
            tab_width: DEFAULT_TAB_WIDTH,
        }
    }
}

/// Tab stops every four columns — the width the formatter indents with.
const DEFAULT_TAB_WIDTH: u32 = 4;

/// Source lines shown for a multi-line span before the middle is elided
/// with `...`. Two at the head plus the tail line.
const MULTILINE_HEAD: u32 = 2;

/// SGR parameters used by the renderer. Bold/faint are structural;
/// colors are chosen per [`Severity`].
const BOLD: &str = "1";
const FAINT: &str = "2";

/// One span to underline, with the message that goes beside its caret.
#[derive(Clone, Copy)]
struct Ann<'a> {
    span: Span,
    message: &'a str,
    is_primary: bool,
    severity: Severity,
}

/// The annotations sharing one source line, rendered as a single snippet
/// block: one source line, one caret row per annotation.
struct Block<'a> {
    source: SourceId,
    /// 1-based line of `anns[0]`'s start.
    line: u32,
    /// 1-based line of the widest annotation's end — equal to `line`
    /// unless some annotation spans several lines.
    end_line: u32,
    anns: Vec<Ann<'a>>,
}

// `Renderer` is `Copy`, but methods take `&self` by Rust API convention.
#[allow(clippy::trivially_copy_pass_by_ref)]
impl Renderer {
    #[must_use]
    pub fn ansi() -> Self {
        Self {
            style: Style {
                ansi: true,
                ..Style::default()
            },
        }
    }

    /// Render `diag`, resolving every span — primary, labels and
    /// suggestion edits — through `sources`.
    pub fn render(&self, diag: &Diagnostic, sources: &dyn SourceMap) -> String {
        let mut out = String::new();
        self.header(diag, &mut out);

        let blocks = Self::blocks(diag, sources);
        let gutter_w = Self::gutter_width(diag, sources, &blocks);

        let mut last_source: Option<SourceId> = None;
        for block in &blocks {
            let Some(entry) = sources.get(block.source) else {
                // Unresolvable source: say what the label said, but never
                // draw a caret into a file it does not belong to. The
                // primary's own message is already in the header.
                for ann in block.anns.iter().filter(|a| !a.is_primary) {
                    let _ = writeln!(out, "  = {}", ann.message);
                }
                continue;
            };
            if last_source != Some(block.source) {
                let lc = entry.lines.line_col(block.anns[0].span.start);
                let col = entry.lines.display_col(
                    entry.text,
                    block.anns[0].span.start,
                    self.style.tab_width,
                );
                self.styled(&mut out, FAINT, |o| {
                    let _ = writeln!(o, "  --> {}:{}:{col}", entry.name, lc.line);
                });
                last_source = Some(block.source);
            }
            self.block(&mut out, block, entry, gutter_w);
        }

        for note in &diag.notes {
            out.push_str("  ");
            self.styled_str(&mut out, BOLD, "note:");
            out.push(' ');
            out.push_str(note);
            out.push('\n');
        }

        for sug in &diag.suggestions {
            self.suggestion(sug, sources, gutter_w, &mut out);
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

    /// [`render`](Self::render) against a single known file — the shape
    /// callers used before spans were resolved per file. A label or edit
    /// belonging to another file renders no snippet rather than being
    /// mis-anchored to this one.
    pub fn render_single(
        &self,
        diag: &Diagnostic,
        source: &str,
        file: &str,
        lines: &LineTable,
    ) -> String {
        self.render(
            diag,
            &SingleSource {
                id: diag.span.source,
                entry: SourceEntry {
                    name: file,
                    text: source,
                    lines,
                },
            },
        )
    }

    /// Split a diagnostic's annotations into snippet blocks: the primary
    /// first, then the labels ordered by file and line, with everything
    /// sharing a file *and* a start line merged into one block.
    fn blocks<'a>(diag: &'a Diagnostic, sources: &dyn SourceMap) -> Vec<Block<'a>> {
        let line_of = |span: Span| {
            sources
                .get(span.source)
                .map_or(0, |e| e.lines.line_col(span.start).line)
        };
        let end_line_of = |span: Span| {
            sources
                .get(span.source)
                .map_or(0, |e| e.lines.line_col(span.end).line)
        };

        let primary = Ann {
            span: diag.span,
            message: &diag.message,
            is_primary: true,
            severity: diag.severity,
        };
        let mut rest: Vec<Ann<'a>> = diag
            .labels
            .iter()
            .map(|l| Ann {
                span: l.span,
                message: &l.message,
                is_primary: false,
                severity: Severity::Info,
            })
            .collect();
        // Stable, so labels on one line keep the order the producer added
        // them; the primary's file sorts first so its snippet leads.
        let primary_source = diag.span.source;
        rest.sort_by_key(|a| {
            (
                a.span.source != primary_source,
                a.span.source.get(),
                line_of(a.span),
                a.span.start,
            )
        });

        let mut blocks: Vec<Block<'a>> = Vec::with_capacity(1 + rest.len());
        for ann in std::iter::once(primary).chain(rest) {
            let line = line_of(ann.span);
            let end_line = end_line_of(ann.span).max(line);
            // A multi-line span owns its block: its gutter art has no room
            // for a second caret row.
            let mergeable = end_line == line;
            match blocks.iter_mut().find(|b| {
                mergeable && b.source == ann.span.source && b.line == line && b.end_line == line
            }) {
                Some(b) => b.anns.push(ann),
                None => blocks.push(Block {
                    source: ann.span.source,
                    line,
                    end_line,
                    anns: vec![ann],
                }),
            }
        }
        blocks
    }

    /// Width of the line-number gutter: wide enough for every line number
    /// this diagnostic prints, so blocks and suggestion previews align
    /// even when they straddle a power of ten.
    fn gutter_width(diag: &Diagnostic, sources: &dyn SourceMap, blocks: &[Block<'_>]) -> usize {
        let mut max_line = 1;
        for b in blocks {
            max_line = max_line.max(b.end_line);
        }
        for sug in &diag.suggestions {
            for edit in &sug.edits {
                if let Some(e) = sources.get(edit.span.source) {
                    max_line = max_line.max(e.lines.line_col(edit.span.start).line);
                }
            }
        }
        line_no_width(max_line)
    }

    fn header(&self, diag: &Diagnostic, out: &mut String) {
        self.styled_str(out, severity_sgr(diag.severity), diag.severity.as_str());
        out.push('[');
        self.styled_str(out, BOLD, diag.code.0);
        out.push_str("]: ");
        self.styled_str(out, BOLD, &diag.message);
        out.push('\n');
    }

    /// One snippet block: the top rule, the source line(s), and a caret
    /// row per annotation.
    fn block(&self, out: &mut String, block: &Block<'_>, entry: SourceEntry<'_>, gutter_w: usize) {
        let gutter = " ".repeat(gutter_w);
        self.styled(out, FAINT, |o| {
            let _ = writeln!(o, "{gutter} |");
        });
        if block.end_line > block.line {
            self.multiline_block(out, block, entry, gutter_w);
            return;
        }
        self.source_line(out, entry, block.line, gutter_w, "");
        for ann in &block.anns {
            let (pad, caret_str) = self.caret_row(entry, ann.span, ann.is_primary);
            let caret_sgr = if ann.is_primary {
                severity_sgr(ann.severity)
            } else {
                FAINT
            };
            self.styled(out, FAINT, |o| {
                let _ = write!(o, "{gutter} | ");
            });
            out.push_str(&pad);
            self.styled_str(out, caret_sgr, &caret_str);
            if !ann.message.is_empty() {
                out.push(' ');
                self.styled_str(out, caret_sgr, ann.message);
            }
            out.push('\n');
        }
    }

    /// A span covering several lines: `/` on the opening line, `|` down
    /// the side, and a closing row whose underscores reach the end column.
    /// The middle is elided past [`MULTILINE_HEAD`] lines so a span over a
    /// whole function does not print the function.
    fn multiline_block(
        &self,
        out: &mut String,
        block: &Block<'_>,
        entry: SourceEntry<'_>,
        gutter_w: usize,
    ) {
        let gutter = " ".repeat(gutter_w);
        let ann = block.anns[0];
        let (first, last) = (block.line, block.end_line);
        let elide_from = first + MULTILINE_HEAD;
        let mut elided = false;
        for line in first..=last {
            if line > elide_from && line < last {
                if !elided {
                    self.styled(out, FAINT, |o| {
                        let _ = writeln!(o, "...");
                    });
                    elided = true;
                }
                continue;
            }
            let bar = if line == first { "/ " } else { "| " };
            self.source_line(out, entry, line, gutter_w, bar);
        }
        let end_col = entry
            .lines
            .display_col(entry.text, ann.span.end, self.style.tab_width)
            .max(1) as usize;
        let caret_sgr = if ann.is_primary {
            severity_sgr(ann.severity)
        } else {
            FAINT
        };
        self.styled(out, FAINT, |o| {
            let _ = write!(o, "{gutter} | ");
        });
        let closer = format!("|{}^", "_".repeat(end_col.saturating_sub(1)));
        self.styled_str(out, caret_sgr, &closer);
        if !ann.message.is_empty() {
            out.push(' ');
            self.styled_str(out, caret_sgr, ann.message);
        }
        out.push('\n');
    }

    /// `NNN | <prefix><source line>`, with tabs expanded so the caret pad
    /// below it counts the same columns.
    fn source_line(
        &self,
        out: &mut String,
        entry: SourceEntry<'_>,
        line: u32,
        gutter_w: usize,
        prefix: &str,
    ) {
        let text = entry
            .lines
            .expand_tabs(entry.text, (line - 1) as usize, self.style.tab_width)
            .unwrap_or_default();
        self.styled(out, FAINT, |o| {
            let _ = write!(o, "{line:>gutter_w$} | {prefix}");
        });
        out.push_str(&text);
        out.push('\n');
    }

    /// The pad and caret run underlining `span` on its own line: both in
    /// display columns, so they agree with the tab-expanded source line.
    fn caret_row(&self, entry: SourceEntry<'_>, span: Span, is_primary: bool) -> (String, String) {
        let tab = self.style.tab_width;
        let start_col = entry.lines.display_col(entry.text, span.start, tab);
        // Clamp the underline to the end of the line: a span may run to the
        // newline, and a zero-length span still gets one caret.
        let line_idx = (entry.lines.line_col(span.start).line - 1) as usize;
        let line_end_col = entry
            .lines
            .line_text(entry.text, line_idx)
            .map_or(start_col, |t| {
                offset(expand_tabs_in(t, tab).chars().count()) + 1
            });
        let end_col = entry
            .lines
            .display_col(entry.text, span.end, tab)
            .clamp(start_col, line_end_col.max(start_col));
        let len = ((end_col - start_col) as usize).max(1);
        let caret_char = if is_primary { '^' } else { '-' };
        (
            " ".repeat((start_col - 1) as usize),
            std::iter::repeat_n(caret_char, len).collect(),
        )
    }

    /// The `help:` line and a preview of the fixed source.
    ///
    /// Every edit is applied, not just the first: a suggestion that wraps
    /// an expression is two edits (an opening and a closing one), and
    /// previewing only the opening one showed source the fix never writes.
    fn suggestion(
        &self,
        sug: &Suggestion,
        sources: &dyn SourceMap,
        gutter_w: usize,
        out: &mut String,
    ) {
        out.push_str("  ");
        self.styled_str(out, BOLD, "help:");
        out.push(' ');
        out.push_str(&sug.message);
        out.push('\n');

        // Group the edits by the line they land on, in the order the lines
        // first appear, so a multi-line fix previews every line it touches.
        let mut lines: Vec<(SourceId, u32, Vec<&crate::TextEdit>)> = Vec::new();
        for edit in &sug.edits {
            let Some(entry) = sources.get(edit.span.source) else {
                continue;
            };
            let line = entry.lines.line_col(edit.span.start).line;
            match lines
                .iter_mut()
                .find(|(s, l, _)| *s == edit.span.source && *l == line)
            {
                Some((_, _, v)) => v.push(edit),
                None => lines.push((edit.span.source, line, vec![edit])),
            }
        }

        for (source, line, mut edits) in lines {
            let Some(entry) = sources.get(source) else {
                continue;
            };
            let line_idx = (line - 1) as usize;
            let line_text = entry.lines.line_text(entry.text, line_idx).unwrap_or("");
            let line_start = entry.lines.line_start(line_idx).unwrap_or(0);
            // Back to front, so an earlier edit's offsets stay valid.
            edits.sort_by_key(|e| std::cmp::Reverse(e.span.start));
            let mut after = line_text.to_string();
            for edit in edits {
                let s = clamp_boundary(&after, edit.span.start.saturating_sub(line_start) as usize);
                let e = clamp_boundary(&after, edit.span.end.saturating_sub(line_start) as usize)
                    .max(s);
                after.replace_range(s..e, &edit.replacement);
            }
            let after = expand_tabs_in(&after, self.style.tab_width).into_owned();
            self.styled(out, FAINT, |o| {
                let _ = writeln!(o, "{line:>gutter_w$} | {after}");
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

/// The largest char boundary of `s` at or below `i`, clamped to `s.len()`.
///
/// Suggestion spans come from producers that computed them against another
/// snapshot of the text, or against a whole file rather than one line, so
/// they are not trusted to land on a boundary — slicing on one that does
/// not is a panic.
fn clamp_boundary(s: &str, i: usize) -> usize {
    let mut i = i.min(s.len());
    while i > 0 && !s.is_char_boundary(i) {
        i -= 1;
    }
    i
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
    use super::{Renderer, SingleSource, SourceEntry, SourceMap, Sources, offset};
    use crate::{Code, Diagnostic, codes};
    use leek_span::{LineTable, SourceId, Span};

    fn src() -> SourceId {
        SourceId::new(1).unwrap()
    }

    fn inc() -> SourceId {
        SourceId::new(2).unwrap()
    }

    /// Render against one file, the way a standalone tool does.
    fn render1(diag: &Diagnostic, text: &str, file: &str) -> String {
        let lines = LineTable::new(text);
        Renderer::default().render_single(diag, text, file, &lines)
    }

    #[test]
    fn renders_simple_error() {
        let text = "var a = 2\na = 'hello'\n";
        let span = Span::new(src(), 10, 11); // the `a` on line 2
        let diag = Diagnostic::error(codes::ASSIGNMENT_INCOMPATIBLE_TYPE, span, "type mismatch");
        let out = render1(&diag, text, "main.leek");
        assert!(out.contains("error[E0250]"));
        assert!(out.contains("main.leek:2:1"));
        assert!(out.contains("a = 'hello'"));
        assert!(out.contains('^'));
    }

    #[test]
    fn renders_label_and_note() {
        let text = "var x = 1\nvar x = 2\n";
        let primary = Span::new(src(), 14, 15); // second `x`
        let prev = Span::new(src(), 4, 5);
        let diag = Diagnostic::error(codes::REDECLARED_SYMBOL, primary, "`x` is already declared")
            .with_label(prev, "first declared here")
            .with_note("Leekscript v3+ forbids shadowing in the same scope.");
        let out = render1(&diag, text, "fight.leek");
        assert!(out.contains("first declared here"));
        assert!(out.contains("note:"));
    }

    #[test]
    fn renders_suggestion() {
        let text = "return damge\n";
        let span = Span::new(src(), 7, 12);
        let diag =
            Diagnostic::error(Code("E0200"), span, "unknown variable `damge`").with_suggestion(
                crate::Suggestion::replace("did you mean `damage`?", span, "damage"),
            );
        let out = render1(&diag, text, "fight.leek");
        assert!(out.contains("help:"));
        assert!(out.contains("damage"));
    }

    /// The bug in FRONT-09: a label pointing into an included file used to
    /// be drawn against the *primary's* text under the primary's name.
    #[test]
    fn label_in_another_file_renders_against_that_file() {
        let main = "include('inc')\nvar x = 1\nuse(x)\n";
        let include = "// helper\nvar x = 2\n";
        let sources = Sources::single(src(), "main.leek", main).with(inc(), "inc.leek", include);
        let diag = Diagnostic::error(
            codes::REDECLARED_SYMBOL,
            Span::new(src(), 19, 20), // `x` on main.leek line 2
            "`x` is already declared",
        )
        .with_label(Span::new(inc(), 14, 15), "first declared here");
        let out = Renderer::default().render(&diag, &sources);
        assert!(out.contains("--> main.leek:2:5"), "{out}");
        assert!(out.contains("--> inc.leek:2:5"), "{out}");
        // The include's own line 2, not the entry's.
        assert!(out.contains("var x = 2"), "{out}");
        assert!(!out.contains("var x = 1\n   | -"), "{out}");
        assert!(out.contains("first declared here"), "{out}");
    }

    #[test]
    fn a_span_in_an_unmapped_source_renders_no_snippet() {
        let text = "var a = 1\n";
        let sources = Sources::single(src(), "main.leek", text);
        // A synthetic primary: nothing to point at, so no header and no
        // caret — never a caret at byte 0 of whatever file is at hand.
        let synthetic = Diagnostic::error(Code("E0200"), Span::synthetic(), "no location");
        let out = Renderer::default().render(&synthetic, &sources);
        assert!(!out.contains("-->"), "{out}");
        assert!(!out.contains("var a = 1"), "{out}");
        assert!(out.contains("no location"));

        // A label in the manifest sentinel keeps its message but grows no
        // snippet in `main.leek`.
        let manifest_label = Diagnostic::error(Code("E0200"), Span::new(src(), 4, 5), "bad")
            .with_label(
                Span::new(leek_span::Span::MANIFEST_SOURCE, 0, 4),
                "configured here",
            );
        let out = Renderer::default().render(&manifest_label, &sources);
        assert_eq!(out.matches("-->").count(), 1, "{out}");
        assert!(out.contains("= configured here"), "{out}");
    }

    #[test]
    fn caret_aligns_under_a_latin1_identifier() {
        // `é` is two bytes: padding by the byte column put the caret one
        // cell right of the `1` it points at.
        let text = "var café = 1\n";
        let at_one = offset(text.find('1').unwrap());
        let diag = Diagnostic::error(
            Code("E0200"),
            Span::new(src(), at_one, at_one + 1),
            "not a string",
        );
        let out = render1(&diag, text, "main.leek");
        assert!(out.contains("--> main.leek:1:12"), "{out}");
        assert!(
            out.contains("\n   |            ^ not a string\n"),
            "caret row misaligned:\n{out}"
        );
    }

    #[test]
    fn caret_aligns_on_a_tab_indented_line() {
        let text = "function f() {\n\treturn x\n}\n";
        let at_x = offset(text.find('x').unwrap());
        let diag = Diagnostic::error(Code("E0200"), Span::new(src(), at_x, at_x + 1), "unknown");
        let out = render1(&diag, text, "main.leek");
        // The tab prints as four spaces and the caret counts the same four.
        assert!(out.contains("\n 2 |     return x\n"), "{out}");
        assert!(out.contains("\n   |            ^ unknown\n"), "{out}");
        assert!(out.contains("--> main.leek:2:12"), "{out}");
    }

    #[test]
    fn multi_line_span_shows_both_ends() {
        let text = "if (a &&\n    b &&\n    c) {\n}\n";
        let end = offset(text.find("c)").unwrap()) + 2;
        let diag = Diagnostic::error(Code("E0200"), Span::new(src(), 0, end), "condition");
        let out = render1(&diag, text, "main.leek");
        assert!(out.contains("\n 1 | / if (a &&\n"), "{out}");
        assert!(out.contains("\n 3 | |     c) {\n"), "{out}");
        // The closing row underscores across to the span's end column.
        assert!(out.contains("\n   | |______^ condition\n"), "{out}");
    }

    #[test]
    fn a_long_multi_line_span_elides_its_middle() {
        let text = "a(\n1,\n2,\n3,\n4,\n5)\n";
        let diag = Diagnostic::error(
            Code("E0200"),
            Span::new(src(), 0, offset(text.len()) - 1),
            "call",
        );
        let out = render1(&diag, text, "main.leek");
        assert!(out.contains("\n...\n"), "{out}");
        assert!(out.contains("\n 6 | | 5)\n"), "{out}");
        assert!(!out.contains("\n 4 |"), "middle not elided:\n{out}");
    }

    #[test]
    fn two_labels_on_one_line_share_one_snippet() {
        let text = "var x = a + b\n";
        let diag = Diagnostic::error(
            Code("E0200"),
            Span::new(src(), 8, 9), // `a`
            "left",
        )
        .with_label(Span::new(src(), 12, 13), "right");
        let out = render1(&diag, text, "main.leek");
        assert_eq!(out.matches("var x = a + b").count(), 1, "{out}");
        assert_eq!(out.matches("-->").count(), 1, "{out}");
        assert!(out.contains("\n   |         ^ left\n"), "{out}");
        assert!(out.contains("\n   |             - right\n"), "{out}");
    }

    /// `leek-lint`'s `redundant_boolean` wraps the expression in `!(` … `)`
    /// with two edits; previewing only the first printed unbalanced source
    /// that `miku fix` would never write.
    #[test]
    fn suggestion_previews_every_edit() {
        let text = "var y = x == false\n";
        let expr = Span::new(src(), 8, 18);
        let diag = Diagnostic::warning(Code("E0200"), expr, "redundant boolean literal")
            .with_suggestion(crate::Suggestion {
                message: "remove the comparison".into(),
                edits: vec![
                    crate::TextEdit {
                        span: Span::new(src(), 8, 8),
                        replacement: "!(".into(),
                    },
                    crate::TextEdit {
                        span: Span::new(src(), 9, 18),
                        replacement: ")".into(),
                    },
                ],
                applicability: crate::Applicability::MachineApplicable,
            });
        let out = render1(&diag, text, "main.leek");
        assert!(out.contains("var y = !(x)"), "{out}");
        assert!(!out.contains("var y = !(x == false"), "{out}");
    }

    #[test]
    fn a_suggestion_edit_inside_a_multibyte_char_does_not_panic() {
        let text = "var café = 1\n";
        // An edit whose offsets land mid-`é` (byte 8 splits it) and past
        // the line end: both must clamp rather than panic.
        let diag =
            Diagnostic::error(Code("E0200"), Span::new(src(), 4, 9), "bad name").with_suggestion(
                crate::Suggestion::replace("rename", Span::new(src(), 8, 999), "cafe"),
            );
        let out = render1(&diag, text, "main.leek");
        assert!(out.contains("help:"), "{out}");
    }

    #[test]
    fn a_suggestion_edit_in_an_unmapped_file_is_dropped_not_misapplied() {
        let text = "var a = 1\n";
        let sources = Sources::single(src(), "main.leek", text);
        let diag = Diagnostic::error(Code("E0200"), Span::new(src(), 4, 5), "bad").with_suggestion(
            crate::Suggestion::replace("fix", Span::new(inc(), 0, 3), "zzz"),
        );
        let out = Renderer::default().render(&diag, &sources);
        assert!(out.contains("help: fix"), "{out}");
        assert!(!out.contains("zzz"), "{out}");
    }

    #[test]
    fn single_source_map_resolves_only_its_own_id() {
        let text = "var a = 1\n";
        let lines = LineTable::new(text);
        let map = SingleSource {
            id: src(),
            entry: SourceEntry {
                name: "main.leek",
                text,
                lines: &lines,
            },
        };
        assert!(map.get(src()).is_some());
        assert!(map.get(inc()).is_none());
    }

    #[test]
    fn pushing_a_source_twice_replaces_it() {
        let mut sources = Sources::single(src(), "old.leek", "old\n");
        sources.push(src(), "new.leek", "new\n");
        assert_eq!(sources.len(), 1);
        assert_eq!(sources.get(src()).unwrap().name, "new.leek");
    }

    /// Locks the exact ANSI byte layout (escapes + ordering) so the
    /// buffer-based color helpers stay byte-identical across refactors.
    /// Exercises header, primary snippet, a secondary label in another
    /// file, a note, a suggestion preview, and the `explain` footer.
    #[test]
    fn ansi_output_is_byte_stable() {
        let text = "var a = 2\na = 'hello'\n";
        let include = "var a = 0\n";
        let sources = Sources::single(src(), "main.leek", text).with(inc(), "inc.leek", include);
        let span = Span::new(src(), 10, 11);
        let diag = Diagnostic::error(codes::ASSIGNMENT_INCOMPATIBLE_TYPE, span, "type mismatch")
            .with_label(Span::new(inc(), 4, 5), "previously")
            .with_note("a note")
            .with_suggestion(crate::Suggestion::replace("fix it", span, "b"));
        let out = Renderer::ansi().render(&diag, &sources);
        let expected = "\x1b[31merror\x1b[0m[\x1b[1mE0250\x1b[0m]: \x1b[1mtype mismatch\x1b[0m\n\
\x1b[2m  --> main.leek:2:1\n\x1b[0m\
\x1b[2m   |\n\x1b[0m\
\x1b[2m 2 | \x1b[0ma = 'hello'\n\
\x1b[2m   | \x1b[0m\x1b[31m^\x1b[0m \x1b[31mtype mismatch\x1b[0m\n\
\x1b[2m  --> inc.leek:1:5\n\x1b[0m\
\x1b[2m   |\n\x1b[0m\
\x1b[2m 1 | \x1b[0mvar a = 0\n\
\x1b[2m   | \x1b[0m    \x1b[2m-\x1b[0m \x1b[2mpreviously\x1b[0m\n  \
\x1b[1mnote:\x1b[0m a note\n  \
\x1b[1mhelp:\x1b[0m fix it\n\
\x1b[2m 2 | b = 'hello'\n\x1b[0m\
\x1b[2m  = for more information, try `miku explain E0250`\n\x1b[0m";
        assert_eq!(out, expected);
    }
}
