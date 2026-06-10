//! `// @version:N` pragma management for migration output.
//!
//! Leekscript's `parse_pragmas` only reads pragmas that appear in
//! the file's leading comment band — the run of trivia before the
//! first significant token. We honor that placement: an existing
//! `@version` line gets rewritten in place; an absent one gets
//! prepended at the top of the file.

use leek_syntax::Version;

/// Update (or insert) the `// @version:N` pragma so the file
/// advertises `target`. Idempotent on already-correct sources.
pub fn set_version(source: &str, target: Version) -> String {
    let n = version_byte(target);
    if let Some((start, end)) = find_existing(source) {
        // Re-render the directive verbatim so the user's leading
        // whitespace / `//` style is preserved.
        let prefix = &source[..start];
        let suffix = &source[end..];
        return format!("{prefix}// @version:{n}{suffix}");
    }
    // Insert at the very top. A trailing `\n` after the directive
    // keeps the rest of the file's first line intact.
    if source.is_empty() {
        return format!("// @version:{n}\n");
    }
    format!("// @version:{n}\n{source}")
}

/// Find an existing `// @version:N` directive's byte range
/// (inclusive start, exclusive end). Scans only the leading
/// comment band — pragmas after the first significant token are
/// invisible to the lexer.
fn find_existing(source: &str) -> Option<(usize, usize)> {
    let mut i = 0;
    let bytes = source.as_bytes();
    while i < bytes.len() {
        // Skip horizontal whitespace.
        while i < bytes.len() && matches!(bytes[i], b' ' | b'\t' | b'\r' | b'\n') {
            i += 1;
        }
        if i >= bytes.len() {
            return None;
        }
        // Only `// …` (line comment) or `/* … */` (block comment)
        // tokens can carry pragmas. Anything else exits the
        // leading band.
        if bytes[i] == b'/' && i + 1 < bytes.len() && bytes[i + 1] == b'/' {
            let line_end = source[i..].find('\n').map_or(source.len(), |p| i + p);
            if is_version_directive(&source[i..line_end]) {
                return Some((i, line_end));
            }
            i = line_end;
        } else if bytes[i] == b'/' && i + 1 < bytes.len() && bytes[i + 1] == b'*' {
            let close = source[i + 2..]
                .find("*/")
                .map_or(source.len(), |p| i + 2 + p + 2);
            if is_version_directive(&source[i..close]) {
                return Some((i, close));
            }
            i = close;
        } else {
            return None;
        }
    }
    None
}

fn is_version_directive(comment: &str) -> bool {
    // Strip comment delimiters and any whitespace, then check the
    // `@version:` prefix. Matches `// @version:3`, `//@version:3`,
    // `/* @version:1 */`, etc.
    let body = if let Some(s) = comment.strip_prefix("//") {
        s
    } else if let Some(s) = comment
        .strip_prefix("/*")
        .and_then(|s| s.strip_suffix("*/"))
    {
        s
    } else {
        return false;
    };
    body.trim().starts_with("@version")
}

fn version_byte(v: Version) -> u8 {
    match v {
        Version::V1 => 1,
        Version::V2 => 2,
        Version::V3 => 3,
        Version::V4 => 4,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inserts_when_absent() {
        let out = set_version("var x = 1\n", Version::V4);
        assert!(out.starts_with("// @version:4\n"));
        assert!(out.contains("var x = 1"));
    }

    #[test]
    fn updates_existing_directive() {
        let out = set_version("// @version:1\nvar x = 1\n", Version::V4);
        assert!(out.starts_with("// @version:4\n"));
        assert!(!out.contains("// @version:1"));
    }

    #[test]
    fn idempotent_on_matching_source() {
        let s = "// @version:3\nvar x = 1\n";
        let out = set_version(s, Version::V3);
        assert_eq!(out, s);
    }

    #[test]
    fn preserves_leading_whitespace_and_comments() {
        let s = "// header comment\n\n// @version:2\nvar x = 1\n";
        let out = set_version(s, Version::V4);
        assert!(out.starts_with("// header comment\n\n// @version:4\n"));
    }
}
