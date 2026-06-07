//! Line-tracking string builder. Mirrors `JavaWriter.java` —
//! every newline bumps a counter so we can emit the `.lines`
//! sidecar mapping Java lines back to Leek source lines.
//!
//! Indentation in clean mode is purely cosmetic — the Java compiler
//! doesn't care. Exact mode emits with a single space per nesting
//! level to keep diffs against the reference noisy-by-line not
//! noisy-by-column; the reference itself uses tab-like spacing.

use std::fmt::Write as _;

#[derive(Debug, Default)]
pub struct JavaWriter {
    code: String,
    lines_file: String,
    line: u32,
    indent: u32,
    /// Map `java_line` → `leek_line`. Sparse: only populated when
    /// the caller calls `addLine_with_loc`.
    line_map: Vec<(u32, u32)>,
}

#[allow(dead_code)]
impl JavaWriter {
    pub fn new() -> Self {
        Self {
            code: String::new(),
            lines_file: String::new(),
            line: 1,
            indent: 0,
            line_map: Vec::new(),
        }
    }

    pub fn code(&self) -> &str {
        &self.code
    }

    pub fn lines(&self) -> &str {
        &self.lines_file
    }

    pub fn into_parts(mut self) -> (String, String) {
        // Materialize the line map now if it hasn't been yet.
        if self.lines_file.is_empty() && !self.line_map.is_empty() {
            for (java_line, leek_line) in &self.line_map {
                // `<javaLine> <fileIndex> <leekLine>\n`. Single-file
                // emission ⇒ fileIndex always 0.
                writeln!(self.lines_file, "{java_line} 0 {leek_line}").unwrap();
            }
        }
        (self.code, self.lines_file)
    }

    /// Append already-formatted text. No newline added.
    pub fn add_code(&mut self, s: &str) {
        for ch in s.chars() {
            if ch == '\n' {
                self.line += 1;
            }
            self.code.push(ch);
        }
    }

    /// Emit indentation for the current nesting level. Skipped at
    /// indent 0 to keep top-level lines flush-left.
    fn write_indent(&mut self) {
        if self.indent == 0 {
            return;
        }
        for _ in 0..self.indent {
            self.code.push('\t');
        }
    }

    /// Emit `text\n`, bumping the line counter.
    pub fn add_line(&mut self, text: &str) {
        self.write_indent();
        self.add_code(text);
        self.code.push('\n');
        self.line += 1;
    }

    /// Same as `add_line`, but also records a `java_line → leek_line`
    /// entry for the `.lines` sidecar.
    pub fn add_line_at(&mut self, text: &str, leek_line: u32) {
        let java_line = self.line;
        self.write_indent();
        self.add_code(text);
        self.code.push('\n');
        self.line_map.push((java_line, leek_line));
        self.line += 1;
    }

    /// Emit an empty line.
    pub fn newline(&mut self) {
        self.code.push('\n');
        self.line += 1;
    }

    /// Open a brace and push indent. Caller must have placed the
    /// `{` themselves; this just bumps the level.
    pub fn push_indent(&mut self) {
        self.indent += 1;
    }

    pub fn pop_indent(&mut self) {
        self.indent = self.indent.saturating_sub(1);
    }

    pub fn current_line(&self) -> u32 {
        self.line
    }
}
