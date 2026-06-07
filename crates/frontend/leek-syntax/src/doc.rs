//! Documentation comments preceding a declaration, plus the
//! `@<backend>-backend:` directives a signature file embeds in them.
//!
//! Grammar:
//!   - A **line-comment block** is a run of consecutive `//` lines
//!     directly above the declaration, separated only by whitespace
//!     on each line. A blank line (one or more empty lines) ends
//!     the block.
//!   - A **block comment** (`/* … */`) directly above the
//!     declaration counts as a single block. JavaDoc-style `/** …
//!     */` is detected; the leading `*` on continuation lines is
//!     stripped.
//!
//! Returns markdown-formatted text or `None` when no doc comment
//! attaches. The output is body text only — callers wrap it in
//! whatever shape they need.
//!
//! This lives in `leek-syntax` (rather than the IDE layer) so the HIR
//! lowerer and backends can read the same directives the editor shows.

/// Backend-specific implementation directives pulled out of a doc
/// comment. A signature file can pair a declaration with the code each
/// backend should emit for it, e.g.
///
/// ```text
/// /**
///  * Adds two integers.
///  * @java-backend: Math.addExact(%0, %1)
///  * @native-backend: leek_add_i64(%0, %1)
///  */
/// function add(integer a, integer b) -> integer;
/// ```
///
/// The directive lines are stripped from the rendered documentation
/// (so users see only "Adds two integers.") and collected here keyed
/// by backend id. Repeated lines for the same backend join with
/// newlines, allowing multi-line bodies.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BackendDirectives {
    by_backend: std::collections::BTreeMap<String, String>,
}

impl BackendDirectives {
    pub fn is_empty(&self) -> bool {
        self.by_backend.is_empty()
    }

    /// The directive body for `backend` (e.g. `"java"`), if any.
    pub fn get(&self, backend: &str) -> Option<&str> {
        self.by_backend.get(backend).map(String::as_str)
    }

    /// Iterate `(backend, body)` pairs in deterministic order.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &str)> {
        self.by_backend.iter().map(|(k, v)| (k.as_str(), v.as_str()))
    }

    /// Build directly from `(backend, body)` pairs — used by the HIR
    /// lowerer to reconstruct directives carried on a function.
    pub fn from_pairs(pairs: impl IntoIterator<Item = (String, String)>) -> Self {
        Self {
            by_backend: pairs.into_iter().collect(),
        }
    }

    /// Owned `(backend, body)` pairs, for storage in lower IRs.
    pub fn into_pairs(self) -> Vec<(String, String)> {
        self.by_backend.into_iter().collect()
    }

    /// Render `backend`'s directive, substituting positional arguments:
    /// `%0`, `%1`, … become `args[n]` and `%%` is a literal `%`. This is
    /// what a backend emitter calls at a call site — e.g. the Java
    /// directive `Math.addExact(%0, %1)` with `args = ["x", "y"]`
    /// renders to `Math.addExact(x, y)`. Returns `None` when no
    /// directive is defined for `backend`.
    pub fn render<S: AsRef<str>>(&self, backend: &str, args: &[S]) -> Option<String> {
        self.get(backend).map(|body| substitute(body, args))
    }
}

/// Substitute `%0`/`%1`/… placeholders in `body` with `args` (`%%`
/// renders a literal `%`). An out-of-range index renders as empty, and
/// a `%` not followed by a digit or `%` is kept verbatim.
pub fn substitute<S: AsRef<str>>(body: &str, args: &[S]) -> String {
    let mut out = String::with_capacity(body.len());
    let mut chars = body.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '%' {
            out.push(c);
            continue;
        }
        match chars.peek().copied() {
            Some('%') => {
                chars.next();
                out.push('%');
            }
            Some(d) if d.is_ascii_digit() => {
                let mut n = 0usize;
                while let Some(d) = chars.peek().and_then(|c| c.to_digit(10)) {
                    n = n * 10 + d as usize;
                    chars.next();
                }
                if let Some(a) = args.get(n) {
                    out.push_str(a.as_ref());
                }
            }
            _ => out.push('%'),
        }
    }
    out
}

/// Whether `@<backend>-backend:` directives are recognized for a file
/// with this `source`. They are a *signature-file* feature: enabled
/// per-file by the `function_signatures` / `signatures` experimental
/// pragma, or by the threaded `function_signatures` feature flag (formerly
/// the `LEEK_EXPERIMENTAL_FN_SIGNATURES` env var, now passed as data). In
/// ordinary Leekscript code directives are inert — a `@java-backend:` line is
/// treated as plain documentation prose.
pub fn directives_enabled(source: &str, function_signatures_flag: bool) -> bool {
    if function_signatures_flag {
        return true;
    }
    let Some(id) = leek_span::SourceId::new(1) else {
        return false;
    };
    let (pragmas, _) = crate::parse_pragmas(source, id);
    pragmas
        .experimental
        .iter()
        .any(|f| f == "function_signatures" || f == "signatures")
}

/// Split a doc-comment body into its human-visible text and any
/// `@<backend>-backend:` directives. Directive lines are removed from
/// the returned documentation string.
pub fn extract_backend_directives(doc: &str) -> (String, BackendDirectives) {
    use std::collections::BTreeMap;
    let mut visible: Vec<&str> = Vec::new();
    let mut collected: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for line in doc.lines() {
        match parse_directive_line(line) {
            Some((id, body)) => collected.entry(id).or_default().push(body),
            None => visible.push(line),
        }
    }
    // Trim blank lines left behind once directives are removed.
    while visible.first().is_some_and(|l| l.trim().is_empty()) {
        visible.remove(0);
    }
    while visible.last().is_some_and(|l| l.trim().is_empty()) {
        visible.pop();
    }
    let by_backend = collected
        .into_iter()
        .map(|(k, lines)| (k, lines.join("\n")))
        .collect();
    (visible.join("\n"), BackendDirectives { by_backend })
}

/// Like [`doc_comment_before`] but also returns the extracted
/// [`BackendDirectives`], with directive lines hidden from the doc
/// text. The doc string may be empty when the comment held *only*
/// directives.
pub fn doc_and_directives_before(
    source: &str,
    decl_offset: u32,
) -> Option<(String, BackendDirectives)> {
    let raw = doc_comment_before(source, decl_offset)?;
    Some(extract_backend_directives(&raw))
}

/// Parse a single `@<id>-backend: <body>` directive line. `id` must be
/// a simple identifier (`java`, `native`, …). Returns `None` for any
/// other line.
fn parse_directive_line(line: &str) -> Option<(String, String)> {
    let rest = line.trim().strip_prefix('@')?;
    // `@<id>-backend:` is a code template (`%0`, `%1`, … substitution).
    // `@<id>-dispatch:` is a host-environment dispatch target, keyed as
    // `<id>-dispatch` (e.g. `@java-dispatch: FightClass`), which a backend
    // expands to `Class.method(ai, coerced-args)`.
    for (marker, suffix) in [("-backend:", ""), ("-dispatch:", "-dispatch")] {
        let Some(idx) = rest.find(marker) else {
            continue;
        };
        let id = &rest[..idx];
        if id.is_empty() || !id.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
            continue;
        }
        let body = rest[idx + marker.len()..].trim().to_string();
        return Some((format!("{id}{suffix}"), body));
    }
    None
}

/// Find the doc-comment block ending immediately before
/// `decl_offset` in `source`. Walks backward through whitespace
/// and stops at the first blank line / non-comment token.
pub fn doc_comment_before(source: &str, decl_offset: u32) -> Option<String> {
    let bytes = source.as_bytes();
    let start = (decl_offset as usize).min(bytes.len());
    // Walk backward past whitespace on the same line as `decl_offset`
    // to land at the end of the preceding line (or at file start).
    let mut i = start;
    while i > 0 && matches!(bytes[i - 1], b' ' | b'\t') {
        i -= 1;
    }
    // We expect a newline here (or to be at file start). If the
    // declaration is on the very first byte of a line with no
    // preceding line, no doc comment can exist.
    if i == 0 || bytes[i - 1] != b'\n' {
        return None;
    }
    i -= 1; // step past the newline

    // First try a block comment immediately above.
    if let Some(block) = try_block_comment_ending_at(source, i) {
        return Some(block);
    }
    // Otherwise, gather consecutive line-comment lines.
    let mut lines: Vec<&str> = Vec::new();
    loop {
        // Find the start of this line: the byte after the previous
        // newline (or 0).
        let line_start = source[..i].rfind('\n').map_or(0, |p| p + 1);
        let line = source[line_start..i].trim_end_matches('\r');
        let trimmed = line.trim_start();
        if let Some(rest) = trimmed.strip_prefix("//") {
            // `///`-style docs (TSDoc/Rust convention) strip an
            // extra `/` so it lands cleanly in the rendered text.
            let rest = rest.strip_prefix('/').unwrap_or(rest);
            // Strip one leading space if present so `// foo` →
            // `foo`, not ` foo`.
            let rest = rest.strip_prefix(' ').unwrap_or(rest);
            lines.push(rest);
            // Continue walking up.
            if line_start == 0 {
                break;
            }
            i = line_start - 1; // step past the preceding newline
        } else if trimmed.is_empty() {
            // Blank line — stops the block.
            break;
        } else {
            // Non-comment, non-blank line — block doesn't extend
            // here. Stop.
            break;
        }
    }
    if lines.is_empty() {
        None
    } else {
        lines.reverse();
        Some(lines.join("\n"))
    }
}

/// `source[..end]` ends at a `\n`. Look for a `*/` immediately
/// preceding (possibly with whitespace) and walk back to its
/// matching `/*`. Returns the body (with `/**` JavaDoc continuation
/// `*` markers stripped) when a block comment is found.
fn try_block_comment_ending_at(source: &str, end: usize) -> Option<String> {
    // Strip trailing whitespace on the line where `*/` should sit.
    let mut j = end;
    while j > 0 && matches!(source.as_bytes()[j - 1], b' ' | b'\t') {
        j -= 1;
    }
    // Need at least `*/`.
    if j < 2 || &source[j - 2..j] != "*/" {
        return None;
    }
    let close = j - 2;
    // Find the matching `/*` (no nesting in Leekscript).
    let open = source[..close].rfind("/*")?;
    let body = &source[open + 2..close];
    // Detect JavaDoc-style and strip continuation `*` from each
    // line. Drop a single leading space too.
    let javadoc = body.starts_with('*');
    let cleaned: Vec<String> = body
        .lines()
        .map(|line| {
            let trimmed = line.trim_start();
            let stripped = if javadoc {
                trimmed.trim_start_matches('*').trim_start_matches(' ')
            } else {
                trimmed
            };
            stripped.trim_end().to_string()
        })
        .collect();
    // Drop leading / trailing blank lines for tidiness.
    let mut start = 0;
    while start < cleaned.len() && cleaned[start].is_empty() {
        start += 1;
    }
    let mut endl = cleaned.len();
    while endl > start && cleaned[endl - 1].is_empty() {
        endl -= 1;
    }
    if start == endl {
        return None;
    }
    Some(cleaned[start..endl].join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(src: &str, marker: &str) -> u32 {
        src.find(marker).expect("marker") as u32
    }

    #[test]
    fn line_comment_block() {
        let src = "// First line\n// Second line\nfunction foo() {}\n";
        let got = doc_comment_before(src, at(src, "function")).unwrap();
        assert_eq!(got, "First line\nSecond line");
    }

    #[test]
    fn javadoc_block_comment() {
        let src = "/**\n * First\n * Second\n */\nfunction foo() {}\n";
        let got = doc_comment_before(src, at(src, "function")).unwrap();
        assert_eq!(got, "First\nSecond");
    }

    #[test]
    fn extracts_backend_directives_and_hides_them() {
        let doc = "Adds two integers.\n@java-backend: Math.addExact(%0, %1)\n@native-backend: leek_add(%0, %1)";
        let (visible, directives) = extract_backend_directives(doc);
        assert_eq!(visible, "Adds two integers.");
        assert_eq!(directives.get("java"), Some("Math.addExact(%0, %1)"));
        assert_eq!(directives.get("native"), Some("leek_add(%0, %1)"));
        assert_eq!(directives.get("interp"), None);
    }

    #[test]
    fn directive_only_doc_has_empty_visible_text() {
        let doc = "@java-backend: foo()";
        let (visible, directives) = extract_backend_directives(doc);
        assert!(visible.is_empty());
        assert_eq!(directives.get("java"), Some("foo()"));
    }

    #[test]
    fn repeated_backend_lines_join() {
        let doc = "@java-backend: {\n@java-backend:   return a + b;\n@java-backend: }";
        let (_, directives) = extract_backend_directives(doc);
        assert_eq!(directives.get("java"), Some("{\nreturn a + b;\n}"));
    }

    #[test]
    fn non_directive_at_lines_stay_visible() {
        let doc = "See @other for details.\n@param x the value";
        let (visible, directives) = extract_backend_directives(doc);
        assert_eq!(visible, "See @other for details.\n@param x the value");
        assert!(directives.is_empty());
    }

    #[test]
    fn substitute_positional_args() {
        assert_eq!(
            substitute("Math.addExact(%0, %1)", &["x", "y"]),
            "Math.addExact(x, y)"
        );
    }

    #[test]
    fn substitute_escapes_double_percent_and_keeps_stray() {
        assert_eq!(substitute("100%% of %0", &["it"]), "100% of it");
        assert_eq!(substitute("a %z b", &["x"]), "a %z b");
    }

    #[test]
    fn substitute_multi_digit_and_out_of_range() {
        let args: Vec<String> = (0..=10).map(|n| format!("a{n}")).collect();
        assert_eq!(substitute("%10/%0", &args), "a10/a0");
        assert_eq!(substitute("[%5]", &["only0"]), "[]");
    }

    #[test]
    fn render_uses_named_backend() {
        let (_, directives) =
            extract_backend_directives("@java-backend: f(%0)\n@native-backend: g(%0)");
        assert_eq!(directives.render("java", &["a"]), Some("f(a)".to_string()));
        assert_eq!(directives.render("native", &["b"]), Some("g(b)".to_string()));
        assert_eq!(directives.render("interp", &["c"]), None);
    }

    #[test]
    fn round_trip_pairs() {
        let (_, directives) = extract_backend_directives("@java-backend: x\n@native-backend: y");
        let pairs = directives.clone().into_pairs();
        assert_eq!(BackendDirectives::from_pairs(pairs), directives);
    }
}
