//! Shared `// @name[:value]` pragma scanner and language-settings resolution.
//!
//! This is the single definition of *which* lines are pragma directives and of
//! how the effective language version and strict mode are settled for one
//! source file. It lives in `leek-span` (a dependency-free core crate) so every
//! layer can use it without a cycle: `leek-project` (which `leek-pipeline`
//! depends on) resolves project inputs with it, and `leek-syntax`'s
//! diagnostic-producing `parse_pragmas` walks the same [`directives`].
//!
//! Resolution order for a file's language settings (see
//! [`LanguageSettings::resolve`]):
//!
//! 1. an explicit override (e.g. `leekc --version-pragma`),
//! 2. the file's first `// @version:N` pragma when `N` is valid (1..=4),
//! 3. the out-of-band default (manifest `[project].language`, corpus case
//!    version, …).
//!
//! Strict mode is on when the file has a `// @strict` flag pragma **or** the
//! default says so. Once settled, the result is carried in the pipeline
//! `Input` and every pass reads it from there instead of re-scanning.

/// The lowest valid language version.
pub const MIN_VERSION: u8 = 1;
/// The highest (latest) valid language version, used when nothing else
/// selects one.
pub const LATEST_VERSION: u8 = 4;

/// One `// @name` or `// @name:value` directive found in a source text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Directive<'a> {
    /// Directive name without the `@` (`version`, `strict`, …).
    pub name: &'a str,
    /// The text after `:`, if a colon was present (may be empty).
    pub value: Option<&'a str>,
    /// Absolute byte offset of the `@`.
    pub name_start: u32,
    /// Absolute byte offset just past the name.
    pub name_end: u32,
}

/// Iterate over every pragma directive in `text`, in source order.
///
/// A directive is a line consisting of optional leading whitespace, `//`,
/// optional whitespace, `@identifier`, and an optional `:value`, with nothing
/// but whitespace after it. Pragmas may appear on any line (upstream accepts
/// them anywhere, not only in the header). Block comments are never pragmas.
pub fn directives(text: &str) -> impl Iterator<Item = Directive<'_>> {
    line_offsets(text).filter_map(|(line_offset, line)| extract_directive(line, line_offset))
}

/// The language-relevant pragmas of one file, with no diagnostics.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LanguagePragmas {
    /// The version selected by the file's **first** `@version` directive, or
    /// `None` when there is none or its value is invalid (a later duplicate
    /// never overrides the first one).
    pub version: Option<u8>,
    /// True when the file has a value-less `@strict` directive.
    pub strict: bool,
}

/// Scan the language-relevant pragmas of `text`.
#[must_use]
pub fn language_pragmas(text: &str) -> LanguagePragmas {
    let mut out = LanguagePragmas::default();
    let mut version_seen = false;
    for d in directives(text) {
        match d.name {
            "version" if !version_seen => {
                version_seen = true;
                out.version = d.value.and_then(parse_version);
            }
            "strict" if d.value.is_none() => out.strict = true,
            _ => {}
        }
    }
    out
}

/// Parse a `@version` value. Accepts `1..=4`.
#[must_use]
pub fn parse_version(raw: &str) -> Option<u8> {
    raw.parse::<u32>()
        .ok()
        .and_then(|n| u8::try_from(n).ok())
        .filter(|n| (MIN_VERSION..=LATEST_VERSION).contains(n))
}

/// Where a file's effective language version came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum VersionOrigin {
    /// An explicit caller override (a CLI flag).
    Override,
    /// The file's own `// @version:N` pragma.
    Pragma,
    /// The out-of-band default (manifest, corpus case, …).
    Default,
}

/// The settled language version and strict mode for one source file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct LanguageSettings {
    /// Effective language version (1..=4).
    pub version: u8,
    /// Which input selected [`Self::version`].
    pub version_origin: VersionOrigin,
    /// Effective strict mode.
    pub strict: bool,
}

impl LanguageSettings {
    /// Settle the language settings for `text`: `override_version`, else the
    /// file's `@version` pragma, else `default_version`; strict when the file
    /// has `@strict` or `default_strict` is set. Out-of-range override or
    /// default versions collapse to [`LATEST_VERSION`].
    #[must_use]
    pub fn resolve(
        text: &str,
        override_version: Option<u8>,
        default_version: u8,
        default_strict: bool,
    ) -> Self {
        let pragmas = language_pragmas(text);
        let (version, version_origin) = match (override_version, pragmas.version) {
            (Some(v), _) => (v, VersionOrigin::Override),
            (None, Some(v)) => (v, VersionOrigin::Pragma),
            (None, None) => (default_version, VersionOrigin::Default),
        };
        let version = if (MIN_VERSION..=LATEST_VERSION).contains(&version) {
            version
        } else {
            LATEST_VERSION
        };
        Self {
            version,
            version_origin,
            strict: pragmas.strict || default_strict,
        }
    }
}

/// Iterator over `(byte_offset, line_without_terminator)` pairs.
fn line_offsets(text: &str) -> impl Iterator<Item = (u32, &str)> {
    let mut pos = 0u32;
    text.split_inclusive('\n').map(move |chunk| {
        let start = pos;
        pos += crate::offset(chunk.len());
        let line = chunk.strip_suffix('\n').unwrap_or(chunk);
        let line = line.strip_suffix('\r').unwrap_or(line);
        (start, line)
    })
}

/// Try to extract a directive from a single line starting at `line_offset`.
///
/// Accepts arbitrary leading whitespace; rejects anything else after the
/// directive (so e.g. `// @foo trailing` is not a directive).
fn extract_directive(line: &str, line_offset: u32) -> Option<Directive<'_>> {
    let bytes = line.as_bytes();
    let skip_ws = |mut i: usize| {
        while i < bytes.len() && matches!(bytes[i], b' ' | b'\t') {
            i += 1;
        }
        i
    };

    let mut cursor = skip_ws(0);
    // Must start with `//`.
    if !line[cursor..].starts_with("//") {
        return None;
    }
    cursor = skip_ws(cursor + 2);
    // Must be an `@`.
    if bytes.get(cursor) != Some(&b'@') {
        return None;
    }
    let name_start = cursor;
    cursor += 1;

    // Identifier: [A-Za-z_][A-Za-z0-9_]*
    let id_start = cursor;
    if !bytes
        .get(cursor)
        .is_some_and(|c| c.is_ascii_alphabetic() || *c == b'_')
    {
        return None;
    }
    while cursor < bytes.len() && (bytes[cursor].is_ascii_alphanumeric() || bytes[cursor] == b'_') {
        cursor += 1;
    }
    let name = &line[id_start..cursor];
    let name_end = cursor;

    // Optional `:value` (whitespace allowed around the colon).
    let mut value = None;
    cursor = skip_ws(cursor);
    if bytes.get(cursor) == Some(&b':') {
        cursor = skip_ws(cursor + 1);
        let val_start = cursor;
        while cursor < bytes.len() && !matches!(bytes[cursor], b' ' | b'\t') {
            cursor += 1;
        }
        value = Some(&line[val_start..cursor]);
    }
    // Reject trailing junk.
    if skip_ws(cursor) != bytes.len() {
        return None;
    }

    Some(Directive {
        name,
        value,
        name_start: line_offset + crate::offset(name_start),
        name_end: line_offset + crate::offset(name_end),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_pragma_spellings_are_recognized() {
        // The real syntax is `// @version:N` (what `miku new` writes); the
        // old project pre-scan never matched it and silently fell back to
        // the manifest default.
        for text in [
            "// @version:1\nreturn 1;",
            "//@version:1\n",
            " // @version:1\n",
            "\t// @version : 1\n",
            "var x = 1;\n// @version:1\n",
        ] {
            assert_eq!(language_pragmas(text).version, Some(1), "{text:?}");
        }
    }

    #[test]
    fn non_directives_are_ignored() {
        for text in [
            "/* @version:1 */ return 1;",
            "// @version:1 trailing\n",
            "// @ version:1\n",
            "var x = 1; // @version:1\n",
        ] {
            assert_eq!(language_pragmas(text).version, None, "{text:?}");
        }
    }

    #[test]
    fn first_version_wins_and_invalid_values_are_ignored() {
        assert_eq!(
            language_pragmas("// @version:2\n// @version:3\n").version,
            Some(2)
        );
        assert_eq!(language_pragmas("// @version:9\n").version, None);
        assert_eq!(language_pragmas("// @version:abc\n").version, None);
        assert_eq!(language_pragmas("// @version\n").version, None);
        // An invalid first directive still claims the slot, like the
        // diagnostic-producing parser (the duplicate is an error there).
        assert_eq!(
            language_pragmas("// @version:0\n// @version:2\n").version,
            None
        );
    }

    #[test]
    fn strict_is_a_flag_only() {
        assert!(language_pragmas("// @strict\n").strict);
        assert!(language_pragmas("//@strict").strict);
        assert!(!language_pragmas("// @strict:true\n").strict);
        assert!(!language_pragmas("// strict\n").strict);
    }

    #[test]
    fn resolve_prefers_override_then_pragma_then_default() {
        let s = LanguageSettings::resolve("// @version:1\n", None, 4, false);
        assert_eq!((s.version, s.version_origin), (1, VersionOrigin::Pragma));

        let s = LanguageSettings::resolve("// @version:1\n", Some(3), 4, false);
        assert_eq!((s.version, s.version_origin), (3, VersionOrigin::Override));

        let s = LanguageSettings::resolve("return 1;", None, 2, false);
        assert_eq!((s.version, s.version_origin), (2, VersionOrigin::Default));

        let s = LanguageSettings::resolve("return 1;", None, 0, false);
        assert_eq!(s.version, LATEST_VERSION);
    }

    #[test]
    fn resolve_strict_is_pragma_or_default() {
        assert!(LanguageSettings::resolve("// @strict\n", None, 4, false).strict);
        assert!(LanguageSettings::resolve("return 1;", None, 4, true).strict);
        assert!(!LanguageSettings::resolve("return 1;", None, 4, false).strict);
    }

    #[test]
    fn directive_spans_are_absolute() {
        let text = "var x;\n  // @strict\n";
        let d = directives(text).next().expect("directive");
        assert_eq!(&text[d.name_start as usize..d.name_end as usize], "@strict");
    }
}
