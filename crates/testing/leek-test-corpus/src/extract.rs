//! Extract test cases from upstream Java JUnit sources.
//!
//! Scans `src/test/java/test/Test*.java` for calls of the form
//!
//! ```java
//! code_v4_("source").equals("expected");
//! code_strict_v2_("source").error(Error.UNKNOWN_VARIABLE);
//! code_v1_3("source").warning(Error.X);
//! ```
//!
//! Each call expands into one [`TestCase`](super::cases::TestCase) per
//! language version in its range.
//!
//! The extractor is intentionally tolerant: anything it can't parse
//! gets recorded in [`Manifest::skipped`](super::cases::Manifest::skipped)
//! rather than failing extraction. We aim for ~90%+ coverage of the
//! upstream corpus on the first pass and iterate from there.

use std::path::Path;

use leek_test_driver::cases::{Expectation, Manifest, SkippedCall, TestCase};

/// Latest version we recognize (mirrors `WordCompiler.LATEST_VERSION`).
/// Used to expand open-ended ranges like `code_v2_` (v2..=LATEST).
const LATEST_VERSION: u8 = 4;

/// One row in the helper-prefix lookup table.
struct HelperPrefix {
    /// Without the trailing `(`.
    name: &'static str,
    version_min: u8,
    version_max: u8,
    strict: bool,
    enabled: bool,
}

/// All helper-prefix forms in `TestCommon.java`. Ordered longest-first
/// so prefix-matching picks the most specific name.
const PREFIXES: &[HelperPrefix] = &[
    HelperPrefix {
        name: "DISABLED_code_v4_",
        version_min: 4,
        version_max: LATEST_VERSION,
        strict: false,
        enabled: false,
    },
    HelperPrefix {
        name: "DISABLED_code_v2_",
        version_min: 2,
        version_max: LATEST_VERSION,
        strict: false,
        enabled: false,
    },
    HelperPrefix {
        name: "DISABLED_code_v1",
        version_min: 1,
        version_max: 1,
        strict: false,
        enabled: false,
    },
    HelperPrefix {
        name: "DISABLED_code",
        version_min: 1,
        version_max: LATEST_VERSION,
        strict: false,
        enabled: false,
    },
    HelperPrefix {
        name: "code_strict_v4_",
        version_min: 4,
        version_max: LATEST_VERSION,
        strict: true,
        enabled: true,
    },
    HelperPrefix {
        name: "code_strict_v2_",
        version_min: 2,
        version_max: LATEST_VERSION,
        strict: true,
        enabled: true,
    },
    HelperPrefix {
        name: "code_strict_v1",
        version_min: 1,
        version_max: 1,
        strict: true,
        enabled: true,
    },
    HelperPrefix {
        name: "code_strict",
        version_min: 1,
        version_max: LATEST_VERSION,
        strict: true,
        enabled: true,
    },
    HelperPrefix {
        name: "code_v1_2",
        version_min: 1,
        version_max: 2,
        strict: false,
        enabled: true,
    },
    HelperPrefix {
        name: "code_v1_3",
        version_min: 1,
        version_max: 3,
        strict: false,
        enabled: true,
    },
    HelperPrefix {
        name: "code_v1_4",
        version_min: 1,
        version_max: 4,
        strict: false,
        enabled: true,
    },
    HelperPrefix {
        name: "code_v2_3",
        version_min: 2,
        version_max: 3,
        strict: false,
        enabled: true,
    },
    HelperPrefix {
        name: "code_v2_4",
        version_min: 2,
        version_max: 4,
        strict: false,
        enabled: true,
    },
    HelperPrefix {
        name: "code_v1",
        version_min: 1,
        version_max: 1,
        strict: false,
        enabled: true,
    },
    HelperPrefix {
        name: "code_v2_",
        version_min: 2,
        version_max: LATEST_VERSION,
        strict: false,
        enabled: true,
    },
    HelperPrefix {
        name: "code_v2",
        version_min: 2,
        version_max: 2,
        strict: false,
        enabled: true,
    },
    HelperPrefix {
        name: "code_v3_",
        version_min: 3,
        version_max: LATEST_VERSION,
        strict: false,
        enabled: true,
    },
    HelperPrefix {
        name: "code_v3",
        version_min: 3,
        version_max: 3,
        strict: false,
        enabled: true,
    },
    HelperPrefix {
        name: "code_v4_",
        version_min: 4,
        version_max: LATEST_VERSION,
        strict: false,
        enabled: true,
    },
    HelperPrefix {
        name: "code_v4",
        version_min: 4,
        version_max: 4,
        strict: false,
        enabled: true,
    },
    HelperPrefix {
        name: "code",
        version_min: 1,
        version_max: LATEST_VERSION,
        strict: false,
        enabled: true,
    },
];

/// Setter methods on `Case` that don't terminate the chain.
/// We skip past them looking for the real expectation.
const CHAIN_SETTERS: &[&str] = &["debug", "max_ops", "max_ram"];

/// Extract every `Test*.java` file in `dir` into one manifest.
pub fn extract_all(dir: &Path) -> anyhow::Result<Manifest> {
    let mut manifest = Manifest::empty();
    let mut entries: Vec<_> = std::fs::read_dir(dir)?
        .filter_map(Result::ok)
        .filter(|e| {
            let name = e.file_name();
            let s = name.to_string_lossy();
            s.starts_with("Test") && s.ends_with(".java")
        })
        .collect();
    entries.sort_by_key(std::fs::DirEntry::file_name);

    for entry in entries {
        let name = entry.file_name().to_string_lossy().to_string();
        manifest.source_files.push(name.clone());
        let text = std::fs::read_to_string(entry.path())?;
        extract_file(&name, &text, &mut manifest);
    }
    manifest
        .cases
        .sort_by(|a, b| (a.source_file.as_str(), a.line).cmp(&(b.source_file.as_str(), b.line)));
    Ok(manifest)
}

/// Extract one Java file's contents into `out`.
pub fn extract_file(source_file: &str, text: &str, out: &mut Manifest) {
    let mut scanner = Scanner::new(text);
    let mut current_method: Option<String> = None;
    let mut call_index: u32 = 0;

    while let Some(c) = scanner.peek() {
        // Track method-name context: look for `void <name>(`.
        if scanner.starts_with("void ") {
            scanner.bump_n("void ".len());
            scanner.skip_ws();
            let name = scanner.consume_ident();
            if !name.is_empty() {
                scanner.skip_ws();
                if scanner.peek() == Some('(') {
                    current_method = Some(name);
                    call_index = 0;
                }
            }
            continue;
        }

        // Skip line comments and block comments — they may contain
        // false matches for `code(`.
        if c == '/' && scanner.peek_at(1) == Some('/') {
            scanner.skip_line_comment();
            continue;
        }
        if c == '/' && scanner.peek_at(1) == Some('*') {
            scanner.skip_block_comment();
            continue;
        }
        // Skip Java string and char literals.
        if c == '"' {
            scanner.skip_java_string();
            continue;
        }
        if c == '\'' {
            scanner.skip_java_char();
            continue;
        }

        // Identifier at a word boundary?
        if c.is_ascii_alphabetic() || c == '_' {
            if scanner.prev_is_word_boundary()
                && let Some(prefix) = find_prefix(scanner.remaining())
            {
                let line = scanner.line();
                let call_start = scanner.pos();
                let helper = prefix.name.to_string();
                scanner.bump_n(prefix.name.len());
                match parse_call(&mut scanner) {
                    Ok((code, expectation)) => {
                        let java_line = scanner.slice(call_start, scanner.pos()).trim().to_string();
                        for version in prefix.version_min..=prefix.version_max {
                            let id = format!(
                                "{}::{}::{}@v{}",
                                source_file,
                                current_method.as_deref().unwrap_or("<top>"),
                                call_index,
                                version,
                            );
                            out.cases.push(TestCase {
                                id,
                                source_file: source_file.to_string(),
                                method_name: current_method
                                    .clone()
                                    .unwrap_or_else(|| "<top>".into()),
                                line,
                                call_index,
                                helper: helper.clone(),
                                java_line: java_line.clone(),
                                version,
                                strict: prefix.strict,
                                enabled: prefix.enabled,
                                code: code.clone(),
                                expected: expectation.clone(),
                                audit: None,
                            });
                        }
                        call_index += 1;
                    }
                    Err(reason) => {
                        out.skipped.push(SkippedCall {
                            source_file: source_file.to_string(),
                            line,
                            reason,
                            snippet: scanner.peek_snippet(80),
                        });
                    }
                }
                continue;
            }
            // Skip the identifier so we don't re-examine its tail.
            scanner.consume_ident();
            continue;
        }

        scanner.bump();
    }
}

/// Longest-match against the prefix table at the scanner's cursor.
fn find_prefix(remaining: &str) -> Option<&'static HelperPrefix> {
    for p in PREFIXES {
        if remaining.starts_with(p.name) {
            let after = remaining.as_bytes().get(p.name.len()).copied();
            // The prefix must be a complete word — next char can't be
            // an ident continuation. Then `(` follows after whitespace.
            if !matches!(after, Some(b) if (b as char).is_ascii_alphanumeric() || b == b'_') {
                // Lookahead for `(`, allowing whitespace.
                let mut idx = p.name.len();
                while remaining
                    .as_bytes()
                    .get(idx)
                    .is_some_and(u8::is_ascii_whitespace)
                {
                    idx += 1;
                }
                if remaining.as_bytes().get(idx) == Some(&b'(') {
                    return Some(p);
                }
            }
        }
    }
    None
}

/// After consuming the helper prefix, read `("code"[+"code"]...)` and
/// the chained `.X(...)` expectation.
fn parse_call(scanner: &mut Scanner) -> Result<(String, Expectation), String> {
    scanner.skip_ws();
    if scanner.peek() != Some('(') {
        return Err("expected '(' after helper prefix".into());
    }
    scanner.bump(); // '('
    scanner.skip_ws();

    let code = parse_string_concat(scanner)?;
    scanner.skip_ws();
    if scanner.peek() != Some(')') {
        return Err(format!(
            "expected ')' after code argument, found {:?}",
            scanner.peek()
        ));
    }
    scanner.bump(); // ')'

    // Walk the chain, skipping setters.
    loop {
        scanner.skip_ws();
        if scanner.peek() != Some('.') {
            return Err("missing chained expectation".into());
        }
        scanner.bump(); // '.'
        scanner.skip_ws();
        let name = scanner.consume_ident();
        scanner.skip_ws();
        if scanner.peek() != Some('(') {
            return Err(format!("expected '(' after .{name}"));
        }
        scanner.bump(); // '('
        let inner = read_until_balanced_close(scanner)?;
        let exp = match name.as_str() {
            "equals" => Some(Expectation::Equals {
                value: extract_first_string(&inner)
                    .or_else(|| evaluate_java_literal(inner.trim()))
                    .unwrap_or(inner.trim().to_string()),
            }),
            "error" => Some(Expectation::Error {
                code: extract_error_code(&inner).unwrap_or_else(|| inner.trim().to_string()),
            }),
            "warning" => Some(Expectation::Warning {
                code: extract_error_code(&inner).unwrap_or_else(|| inner.trim().to_string()),
            }),
            "noWarning" => Some(Expectation::NoWarning),
            "any_error" => Some(Expectation::AnyError),
            "almost" => Some(Expectation::Almost {
                value: evaluate_java_literal(inner.trim())
                    .unwrap_or_else(|| inner.trim().to_string()),
            }),
            "ops" => inner
                .trim()
                .trim_end_matches('L') // Java long literal suffix
                .parse::<u64>()
                .ok()
                .map(|count| Expectation::Ops { count }),
            "equalsOps" => parse_equals_ops_args(&inner)
                .map(|(value, count)| Expectation::EqualsOps { value, count }),
            _ if CHAIN_SETTERS.contains(&name.as_str()) => None, // setter — keep walking
            _ => Some(Expectation::Unknown { detail: name }),
        };
        if let Some(e) = exp {
            return Ok((code, e));
        }
    }
}

/// Parse `"foo" + "bar"` style concatenations (one or more string
/// literals separated by `+`). Returns the concatenation as one Rust
/// string with escapes interpreted.
fn parse_string_concat(scanner: &mut Scanner) -> Result<String, String> {
    let mut out = String::new();
    let mut first = true;
    loop {
        scanner.skip_ws();
        match scanner.peek() {
            Some('"') => {
                let part = scanner.read_java_string()?;
                out.push_str(&part);
            }
            Some(c) if first => {
                return Err(format!("expected string literal, found {c:?}"));
            }
            _ => return Ok(out),
        }
        first = false;
        scanner.skip_ws();
        if scanner.peek() == Some('+') {
            scanner.bump();
        } else {
            return Ok(out);
        }
    }
}

/// Read tokens up to the matching `)` of the previously-consumed `(`,
/// stripping the closing paren. Tracks nested parens, strings, chars.
fn read_until_balanced_close(scanner: &mut Scanner) -> Result<String, String> {
    let mut depth: i32 = 1;
    let mut out = String::new();
    while let Some(c) = scanner.peek() {
        match c {
            '(' => {
                depth += 1;
                out.push(c);
                scanner.bump();
            }
            ')' => {
                depth -= 1;
                if depth == 0 {
                    scanner.bump();
                    return Ok(out);
                }
                out.push(c);
                scanner.bump();
            }
            '"' => {
                let pos_before = scanner.pos;
                let _ = scanner.read_java_string()?;
                let pos_after = scanner.pos;
                out.push_str(&scanner.source[pos_before..pos_after]);
            }
            '\'' => {
                let pos_before = scanner.pos;
                scanner.skip_java_char();
                let pos_after = scanner.pos;
                out.push_str(&scanner.source[pos_before..pos_after]);
            }
            _ => {
                out.push(c);
                scanner.bump();
            }
        }
    }
    Err("unterminated chained-method argument".into())
}

/// Extract the first string literal from a chained-method argument
/// Evaluate a small set of Java helper expressions that appear
/// inside `.equals(...)` arguments (e.g.
/// `String.valueOf(LeekConstants.TYPE_NUMBER.getIntValue())`,
/// `String.valueOf(0xFF00FF)`). Returns `None` if the input
/// isn't a recognised shape — the caller falls back to the raw
/// expression text in that case.
fn evaluate_java_literal(s: &str) -> Option<String> {
    let s = s.trim();
    // String.valueOf(<inner>)
    if let Some(rest) = s.strip_prefix("String.valueOf(")
        && let Some(inner) = rest.strip_suffix(')')
    {
        return evaluate_java_literal(inner.trim()).or_else(|| Some(inner.trim().to_string()));
    }
    // LeekConstants.TYPE_FOO.getIntValue() — fixed integer tags.
    if let Some(rest) = s.strip_prefix("LeekConstants.")
        && let Some(name) = rest.strip_suffix(".getIntValue()")
    {
        return match name {
            "TYPE_NULL" => Some("0".into()),
            "TYPE_NUMBER" => Some("1".into()),
            "TYPE_BOOLEAN" => Some("2".into()),
            "TYPE_STRING" => Some("3".into()),
            "TYPE_ARRAY" => Some("4".into()),
            "TYPE_FUNCTION" => Some("5".into()),
            "TYPE_CLASS" => Some("6".into()),
            "TYPE_OBJECT" => Some("7".into()),
            "TYPE_MAP" => Some("8".into()),
            "TYPE_SET" => Some("9".into()),
            "TYPE_INTERVAL" => Some("10".into()),
            _ => None,
        };
    }
    // Hex / decimal integer literal — also covers `0xFF00FF`.
    if let Some(rest) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X"))
        && let Ok(n) = i64::from_str_radix(rest, 16)
    {
        return Some(n.to_string());
    }
    if let Ok(n) = s.parse::<i64>() {
        return Some(n.to_string());
    }
    None
}

/// (used by `.equals("foo")` to get `"foo"`). Iterates over `chars()`
/// so multi-byte UTF-8 sequences land as single Unicode scalars.
/// `.equalsOps("value", N)` argument list.
fn parse_equals_ops_args(inner: &str) -> Option<(String, u64)> {
    let value = extract_first_string(inner)?;
    let tail = tail_after_first_string_literal(inner)?;
    let count = tail
        .trim()
        .strip_prefix(',')?
        .trim()
        .trim_end_matches('L')
        .parse::<u64>()
        .ok()?;
    Some((value, count))
}

fn tail_after_first_string_literal(s: &str) -> Option<&str> {
    let mut chars = s.char_indices().peekable();
    while chars.peek().is_some_and(|(_, c)| c.is_ascii_whitespace()) {
        chars.next();
    }
    if chars.peek().map(|(_, c)| *c) != Some('"') {
        return None;
    }
    chars.next();
    while let Some((i, c)) = chars.next() {
        match c {
            '\\' => {
                chars.next();
            }
            '"' => return Some(&s[i + 1..]),
            _ => {}
        }
    }
    None
}

fn extract_first_string(s: &str) -> Option<String> {
    let mut chars = s.chars().peekable();
    // Skip leading whitespace.
    while let Some(&c) = chars.peek() {
        if c.is_ascii_whitespace() {
            chars.next();
        } else {
            break;
        }
    }
    if chars.peek() != Some(&'"') {
        return None;
    }
    chars.next(); // opening quote
    let mut out = String::new();
    while let Some(c) = chars.next() {
        match c {
            '\\' => {
                let esc = chars.next()?;
                out.push(decode_escape(esc));
            }
            '"' => return Some(out),
            other => out.push(other),
        }
    }
    None
}

/// Extract `X` from `Error.X` or just `X`. Returns `None` if neither
/// shape matches.
fn extract_error_code(s: &str) -> Option<String> {
    let t = s.trim();
    if let Some(rest) = t.strip_prefix("Error.") {
        let end = rest
            .find(|c: char| !c.is_ascii_alphanumeric() && c != '_')
            .unwrap_or(rest.len());
        Some(rest[..end].to_string())
    } else if t.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') && !t.is_empty() {
        Some(t.to_string())
    } else {
        None
    }
}

fn decode_escape(c: char) -> char {
    match c {
        'n' => '\n',
        't' => '\t',
        'r' => '\r',
        '"' => '"',
        '\'' => '\'',
        '\\' => '\\',
        '0' => '\0',
        other => other,
    }
}

/// Lightweight character-cursor over a Java source file.
struct Scanner<'a> {
    source: &'a str,
    pos: usize,
    line: u32,
}

impl<'a> Scanner<'a> {
    fn new(source: &'a str) -> Self {
        Self {
            source,
            pos: 0,
            line: 1,
        }
    }

    fn peek(&self) -> Option<char> {
        self.source[self.pos..].chars().next()
    }

    fn peek_at(&self, offset: usize) -> Option<char> {
        self.source[self.pos..].chars().nth(offset)
    }

    fn line(&self) -> u32 {
        self.line
    }

    fn pos(&self) -> usize {
        self.pos
    }

    fn slice(&self, start: usize, end: usize) -> &'a str {
        &self.source[start..end.min(self.source.len())]
    }

    fn remaining(&self) -> &'a str {
        &self.source[self.pos..]
    }

    fn peek_snippet(&self, max_len: usize) -> String {
        let rest = self.remaining();
        let end = rest
            .char_indices()
            .nth(max_len)
            .map_or(rest.len(), |(i, _)| i);
        rest[..end].replace('\n', " ")
    }

    fn bump(&mut self) {
        if let Some(c) = self.peek() {
            if c == '\n' {
                self.line += 1;
            }
            self.pos += c.len_utf8();
        }
    }

    fn bump_n(&mut self, n: usize) {
        for _ in 0..n {
            if self.peek().is_none() {
                return;
            }
            self.bump();
        }
    }

    fn skip_ws(&mut self) {
        while let Some(c) = self.peek() {
            if c.is_whitespace() {
                self.bump();
            } else {
                break;
            }
        }
    }

    fn starts_with(&self, s: &str) -> bool {
        self.remaining().starts_with(s)
    }

    fn consume_ident(&mut self) -> String {
        let mut out = String::new();
        while let Some(c) = self.peek() {
            if c.is_ascii_alphanumeric() || c == '_' {
                out.push(c);
                self.bump();
            } else {
                break;
            }
        }
        out
    }

    fn prev_is_word_boundary(&self) -> bool {
        if self.pos == 0 {
            return true;
        }
        let prev = self.source[..self.pos].chars().next_back();
        match prev {
            None => true,
            Some(c) => !(c.is_ascii_alphanumeric() || c == '_'),
        }
    }

    fn skip_line_comment(&mut self) {
        while let Some(c) = self.peek() {
            if c == '\n' {
                self.bump();
                return;
            }
            self.bump();
        }
    }

    fn skip_block_comment(&mut self) {
        self.bump(); // '/'
        self.bump(); // '*'
        while let Some(c) = self.peek() {
            if c == '*' && self.peek_at(1) == Some('/') {
                self.bump();
                self.bump();
                return;
            }
            self.bump();
        }
    }

    /// Skip a `"…"` Java string literal at the cursor.
    fn skip_java_string(&mut self) {
        debug_assert_eq!(self.peek(), Some('"'));
        self.bump(); // opening "
        while let Some(c) = self.peek() {
            if c == '\\' {
                self.bump();
                if self.peek().is_some() {
                    self.bump();
                }
            } else if c == '"' {
                self.bump();
                return;
            } else {
                self.bump();
            }
        }
    }

    fn skip_java_char(&mut self) {
        debug_assert_eq!(self.peek(), Some('\''));
        self.bump(); // opening '
        while let Some(c) = self.peek() {
            if c == '\\' {
                self.bump();
                if self.peek().is_some() {
                    self.bump();
                }
            } else if c == '\'' {
                self.bump();
                return;
            } else {
                self.bump();
            }
        }
    }

    /// Read a `"…"` Java string literal, returning its decoded content.
    fn read_java_string(&mut self) -> Result<String, String> {
        if self.peek() != Some('"') {
            return Err(format!(
                "expected '\"' at start of string literal, found {:?}",
                self.peek()
            ));
        }
        self.bump();
        let mut out = String::new();
        while let Some(c) = self.peek() {
            if c == '\\' {
                self.bump();
                match self.peek() {
                    Some(esc) => {
                        out.push(decode_escape(esc));
                        self.bump();
                    }
                    None => return Err("unterminated escape sequence".into()),
                }
            } else if c == '"' {
                self.bump();
                return Ok(out);
            } else {
                out.push(c);
                self.bump();
            }
        }
        Err("unterminated string literal".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn extract_one(src: &str) -> Manifest {
        let mut m = Manifest::empty();
        extract_file("Test.java", src, &mut m);
        m
    }

    #[test]
    fn simple_code_equals() {
        let m = extract_one(
            r#"
@Test
public void hello() {
    code("return 1").equals("1");
}
"#,
        );
        assert_eq!(m.cases.len(), 4); // v1..=v4 expansion
        let case = &m.cases[0];
        assert_eq!(case.code, "return 1");
        assert_eq!(case.expected, Expectation::Equals { value: "1".into() });
        assert_eq!(case.method_name, "hello");
        assert_eq!(case.helper, "code");
        assert!(case.java_line.contains("code(\"return 1\").equals"));
        assert!(case.enabled);
    }

    #[test]
    fn version_range_expands() {
        let m = extract_one(
            r#"
@Test
public void multi() {
    code_v1_3("return 2").equals("2");
}
"#,
        );
        assert_eq!(m.cases.len(), 3);
        let versions: Vec<u8> = m.cases.iter().map(|c| c.version).collect();
        assert_eq!(versions, vec![1, 2, 3]);
    }

    #[test]
    fn error_expectation() {
        let m = extract_one(
            r#"
@Test
public void boom() {
    code_v3_("foo bar").error(Error.UNKNOWN_VARIABLE_OR_FUNCTION);
}
"#,
        );
        assert!(matches!(
            m.cases[0].expected,
            Expectation::Error { ref code } if code == "UNKNOWN_VARIABLE_OR_FUNCTION"
        ));
    }

    #[test]
    fn no_warning_expectation() {
        let m = extract_one(
            r#"
@Test
public void clean() {
    code_strict_v4_("var x = 1").noWarning();
}
"#,
        );
        assert_eq!(m.cases[0].expected, Expectation::NoWarning);
        assert!(m.cases[0].strict);
    }

    #[test]
    fn setters_dont_terminate_chain() {
        let m = extract_one(
            r#"
@Test
public void with_debug() {
    code("var x = 1").debug().equals("null");
}
"#,
        );
        assert_eq!(
            m.cases[0].expected,
            Expectation::Equals {
                value: "null".into()
            }
        );
    }

    #[test]
    fn disabled_calls_marked_not_enabled() {
        let m = extract_one(
            r#"
@Test
public void dis() {
    DISABLED_code("return broken").equals("...");
}
"#,
        );
        assert!(!m.cases[0].enabled);
    }

    #[test]
    fn comments_dont_trigger_extraction() {
        let m = extract_one(
            r#"
@Test
public void commented() {
    // code("not a test").equals("...");
    /* code("also not") */
    code("real").equals("1");
}
"#,
        );
        // Only the un-commented call counts → 4 cases (v1..v4).
        assert_eq!(m.cases.len(), 4);
        assert_eq!(m.cases[0].code, "real");
    }

    #[test]
    fn string_concatenation_works() {
        let m = extract_one(
            r#"
@Test
public void concat() {
    code("part one " + "part two").equals("ok");
}
"#,
        );
        assert_eq!(m.cases[0].code, "part one part two");
    }
}
