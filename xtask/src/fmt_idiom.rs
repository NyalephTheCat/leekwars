//! `cargo xtask check-fmt-idiom`: one spelling for "format into a `String`".
//!
//! `write!` and `writeln!` return a `fmt::Result`, so a call used as a
//! statement has to do *something* with it. Two spellings were in use:
//! `write!(buf, "…").unwrap()` and `let _ = write!(buf, "…")`. Every sink in
//! this workspace is a `String` (via `std::fmt::Write`), and
//! `impl Write for String` is infallible — `String::write_str` returns
//! `Ok(())` unconditionally. So the `Result` is never an error, and the
//! choice is purely about which noise to read.
//!
//! The workspace settles on `let _ = write!(…)`, for three reasons:
//!
//! 1. It was already the overwhelming majority — 127 sites to 35.
//! 2. `.unwrap()` on an infallible call inflates the unwrap count, which is
//!    how you audit for the unwraps that *can* fire. 35 of the workspace's
//!    unwraps carried no risk at all yet answered the same grep as the ones
//!    that do.
//! 3. `let _ =` says "there is no error here" at the site. `.unwrap()` says
//!    "this could fail and I accept the panic", which is not what is meant.
//!
//! So the rule is: **no `.unwrap()` or `.expect(…)` on a `write!` /
//! `writeln!`**. It is stated in the ban direction because that is the half a
//! text check can decide without types — a bare `write!(f, …)` inside a
//! `fmt::Display` impl is *correct* (it propagates with `?`, or is the tail
//! expression) and is left alone, whereas an unwrap tail is wrong in every
//! context this workspace has.
//!
//! This is not a clippy lint because none exists. `clippy::unwrap_used` is
//! the closest, and it bans *every* unwrap — 400-odd sites workspace-wide,
//! almost all unrelated to formatting. Same reasoning as [`crate::errors`]:
//! `clippy.toml` is workspace-global, so it cannot express a rule this narrow.
//!
//! If a sink ever really is fallible (an `io::Write`), discarding its error
//! would be a bug and so would unwrapping it. Handle it — or, if a panic
//! genuinely is the right answer there, mark the line above with
//! `// xtask-allow(fmt-idiom): <reason>`.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

/// The opt-out marker, placed on the line above the offending call.
const ALLOW_MARKER: &str = "xtask-allow(fmt-idiom)";

/// Directories never worth descending into.
const SKIP_DIRS: &[&str] = &[".git", "target", "node_modules", ".claude"];

/// One `write!`/`writeln!` whose `fmt::Result` is unwrapped.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Violation {
    pub file: PathBuf,
    /// 1-based line of the macro name.
    pub line: usize,
    /// `write` or `writeln`, so the message can quote what was written.
    pub macro_name: String,
    /// `unwrap` or `expect`.
    pub tail: String,
}

impl std::fmt::Display for Violation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}:{}: `{}!(…).{}()` — formatting into a `String` cannot fail; \
             write `let _ = {}!(…);`",
            self.file.display(),
            self.line,
            self.macro_name,
            self.tail,
            self.macro_name
        )
    }
}

/// `src` with every string, char and comment byte replaced by a space, so a
/// plain text scan cannot be fooled by a `write!` inside a doc comment or by a
/// `)` inside a string literal. Newlines survive, so byte offsets and line
/// numbers still agree with the original.
fn blank_literals(src: &str) -> String {
    let chars: Vec<(usize, char)> = src.char_indices().collect();
    let mut out = vec![b' '; src.len()];
    let bytes = src.as_bytes();
    // Copy the bytes of `chars[k]` through verbatim.
    let keep = |out: &mut Vec<u8>, k: usize| {
        let (off, c) = chars[k];
        out[off..off + c.len_utf8()].copy_from_slice(&bytes[off..off + c.len_utf8()]);
    };
    let at = |k: usize| chars.get(k).map(|&(_, c)| c);

    let mut i = 0;
    while i < chars.len() {
        let c = chars[i].1;
        match c {
            '\n' => {
                keep(&mut out, i);
                i += 1;
            }
            '/' if at(i + 1) == Some('/') => {
                while i < chars.len() && chars[i].1 != '\n' {
                    i += 1;
                }
            }
            '/' if at(i + 1) == Some('*') => {
                // Rust block comments nest.
                let mut depth = 0usize;
                while i < chars.len() {
                    if chars[i].1 == '\n' {
                        keep(&mut out, i);
                    }
                    if chars[i].1 == '/' && at(i + 1) == Some('*') {
                        depth += 1;
                        i += 2;
                    } else if chars[i].1 == '*' && at(i + 1) == Some('/') {
                        depth -= 1;
                        i += 2;
                        if depth == 0 {
                            break;
                        }
                    } else {
                        i += 1;
                    }
                }
            }
            'r' if matches!(at(i + 1), Some('"' | '#')) => {
                // A raw string, or an identifier that merely starts with `r`.
                let mut k = i + 1;
                let mut hashes = 0usize;
                while at(k) == Some('#') {
                    hashes += 1;
                    k += 1;
                }
                if at(k) != Some('"') {
                    keep(&mut out, i);
                    i += 1;
                    continue;
                }
                i = k + 1;
                // Closes on a `"` followed by exactly `hashes` `#`.
                while i < chars.len() {
                    if chars[i].1 == '\n' {
                        keep(&mut out, i);
                    }
                    if chars[i].1 == '"' && (1..=hashes).all(|h| at(i + h) == Some('#')) {
                        i += hashes + 1;
                        break;
                    }
                    i += 1;
                }
            }
            '"' => {
                i += 1;
                while i < chars.len() {
                    match chars[i].1 {
                        '\\' => i += 2,
                        '\n' => {
                            keep(&mut out, i);
                            i += 1;
                        }
                        '"' => {
                            i += 1;
                            break;
                        }
                        _ => i += 1,
                    }
                }
            }
            '\'' => {
                // A char literal closes on the token after the opener
                // (`'a'`, `'"'`, `'\n'`); anything else is a lifetime, which
                // is ordinary code.
                let is_char = at(i + 1) == Some('\\') || at(i + 2) == Some('\'');
                if !is_char {
                    keep(&mut out, i);
                    i += 1;
                    continue;
                }
                i += 1;
                while i < chars.len() {
                    match chars[i].1 {
                        '\\' => i += 2,
                        '\'' => {
                            i += 1;
                            break;
                        }
                        _ => i += 1,
                    }
                }
            }
            _ => {
                keep(&mut out, i);
                i += 1;
            }
        }
    }
    String::from_utf8(out).expect("blanking replaces whole chars, so boundaries survive")
}

/// Offset just past the `)` closing the group that opens at `open`.
fn close_paren(code: &[u8], open: usize) -> Option<usize> {
    let mut depth = 0usize;
    for (i, &b) in code.iter().enumerate().skip(open) {
        match b {
            b'(' => depth += 1,
            b')' => {
                depth -= 1;
                if depth == 0 {
                    return Some(i + 1);
                }
            }
            _ => {}
        }
    }
    None
}

/// Every unwrapped `write!`/`writeln!` in one file, sorted.
pub fn violations_in(file: &Path, src: &str) -> Vec<Violation> {
    let code = blank_literals(src);
    let bytes = code.as_bytes();
    let mut out = Vec::new();

    for name in ["write", "writeln"] {
        let needle = format!("{name}!");
        let mut from = 0;
        while let Some(rel) = code[from..].find(&needle) {
            let start = from + rel;
            from = start + needle.len();
            // Whole token only: not `my_write!`, and `write!` must not be
            // matched inside `writeln!` (`writeln` has no `!` after `write`,
            // so this is already excluded, but the guard costs nothing).
            let leading = code[..start].chars().next_back();
            if leading.is_some_and(|c| c.is_alphanumeric() || c == '_') {
                continue;
            }
            let Some(open) = code[from..].find('(').map(|o| from + o) else {
                continue;
            };
            if !code[from..open].trim().is_empty() {
                continue;
            }
            let Some(after) = close_paren(bytes, open) else {
                continue;
            };
            let rest = code[after..].trim_start();
            let tail = if rest.starts_with(".unwrap()") {
                "unwrap"
            } else if rest.starts_with(".expect(") {
                "expect"
            } else {
                continue;
            };
            let line = src[..start].lines().count().max(1);
            if allowed_at(src, line) {
                continue;
            }
            out.push(Violation {
                file: file.to_path_buf(),
                line,
                macro_name: name.to_string(),
                tail: tail.to_string(),
            });
        }
    }
    out.sort();
    out
}

/// Whether the line above the 1-based `line` carries the opt-out marker.
fn allowed_at(src: &str, line: usize) -> bool {
    line >= 2
        && src
            .lines()
            .nth(line - 2)
            .is_some_and(|l| l.contains(ALLOW_MARKER))
}

/// Walk `dir`, collecting violations from every `.rs` file under it.
fn walk(dir: &Path, out: &mut Vec<Violation>, errors: &mut Vec<String>) {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) => {
            errors.push(format!("cannot read {}: {e}", dir.display()));
            return;
        }
    };
    let mut paths: Vec<PathBuf> = entries.filter_map(Result::ok).map(|e| e.path()).collect();
    paths.sort();
    for path in paths {
        if path.is_dir() {
            let skip = path
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| SKIP_DIRS.contains(&n));
            if !skip {
                walk(&path, out, errors);
            }
        } else if path.extension().is_some_and(|e| e == "rs") {
            match std::fs::read_to_string(&path) {
                Ok(src) => out.extend(violations_in(&path, &src)),
                Err(e) => errors.push(format!("cannot read {}: {e}", path.display())),
            }
        }
    }
}

/// Entry point for `cargo xtask check-fmt-idiom`.
pub fn run() -> ExitCode {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask lives one level below the workspace root");

    let mut violations = Vec::new();
    let mut errors = Vec::new();
    walk(root, &mut violations, &mut errors);

    for e in &errors {
        eprintln!("fmt-idiom check: {e}");
    }
    for v in &violations {
        let shown = Violation {
            file: v.file.strip_prefix(root).unwrap_or(&v.file).to_path_buf(),
            ..v.clone()
        };
        eprintln!("fmt-idiom check: {shown}");
    }
    if violations.is_empty() && errors.is_empty() {
        println!("fmt-idiom check: ok");
        return ExitCode::SUCCESS;
    }
    if !violations.is_empty() {
        eprintln!(
            "fmt-idiom check: formatting into a `String` is infallible, so the \
             `Result` is noise — use `let _ = write!(…);`. If the sink really \
             can fail, handle the error, or mark the line above with \
             `// {ALLOW_MARKER}: <reason>`"
        );
    }
    ExitCode::FAILURE
}

#[cfg(test)]
mod tests {
    use super::*;

    fn found(src: &str) -> Vec<(usize, String, String)> {
        violations_in(Path::new("t.rs"), src)
            .into_iter()
            .map(|v| (v.line, v.macro_name, v.tail))
            .collect()
    }

    #[test]
    fn flags_unwrap_and_expect_tails() {
        let src = "fn f(s: &mut String) {\n    write!(s, \"a\").unwrap();\n    \
                   writeln!(s, \"b\").expect(\"no\");\n}\n";
        assert_eq!(
            found(src),
            [
                (2, "write".into(), "unwrap".into()),
                (3, "writeln".into(), "expect".into()),
            ]
        );
    }

    #[test]
    fn accepts_the_sanctioned_spelling() {
        let src = "fn f(s: &mut String) {\n    let _ = write!(s, \"a\");\n}\n";
        assert_eq!(found(src), []);
    }

    /// A bare `write!` in a `fmt::Result` context is correct and out of scope.
    #[test]
    fn leaves_display_impls_alone() {
        let src = "impl Display for T {\n    fn fmt(&self, f: &mut Formatter) -> Result {\n\
                           write!(f, \"{}\", self.0)\n    }\n}\n";
        assert_eq!(found(src), []);
    }

    /// The closing paren is the macro's, not one inside a string literal.
    #[test]
    fn parens_inside_literals_do_not_close_the_call() {
        let src = "fn f(s: &mut String) {\n    write!(s, \"a)b(c\").unwrap();\n}\n";
        assert_eq!(found(src), [(2, "write".into(), "unwrap".into())]);
    }

    #[test]
    fn multi_line_calls_report_the_macro_line() {
        let src = "fn f(s: &mut String) {\n    write!(\n        s,\n        \"a {}\",\n\
                           x\n    )\n    .unwrap();\n}\n";
        assert_eq!(found(src), [(2, "write".into(), "unwrap".into())]);
    }

    #[test]
    fn ignores_comments_and_raw_strings() {
        let src = "fn f() {\n    // write!(s, \"x\").unwrap();\n    \
                   let a = r#\"write!(s, \"x\").unwrap();\"#;\n    \
                   /* write!(s, \"x\").unwrap(); */\n}\n";
        assert_eq!(found(src), []);
    }

    /// `'` opening a lifetime must not swallow the rest of the file.
    #[test]
    fn lifetimes_are_not_char_literals() {
        let src = "fn f<'a>(s: &'a mut String) {\n    let c = '\"';\n    \
                   write!(s, \"a\").unwrap();\n}\n";
        assert_eq!(found(src), [(3, "write".into(), "unwrap".into())]);
    }

    #[test]
    fn similarly_named_macros_are_not_matched() {
        let src = "fn f(s: &mut String) {\n    my_write!(s, \"a\").unwrap();\n}\n";
        assert_eq!(found(src), []);
    }

    #[test]
    fn the_marker_opts_one_line_out() {
        let src = "fn f(w: &mut File) {\n    // xtask-allow(fmt-idiom): real I/O\n    \
                   write!(w, \"a\").unwrap();\n}\n";
        assert_eq!(found(src), []);
    }
}
