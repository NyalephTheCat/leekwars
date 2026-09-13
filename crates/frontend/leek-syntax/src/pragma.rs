//! Pragma preprocessor.
//!
//! Pragmas are extracted by scanning the source line-by-line for
//! `// @name` or `// @name:value`. They must be resolved **before**
//! lexing because the active version determines which words are
//! keywords. Spec: `doc/lexical.md` §4, `doc/versioning.md` §2.
//!
//! Recognized pragmas (this slice):
//!
//! - `// @version:N` — selects language version (1..=4)
//! - `// @strict` — enables strict mode
//! - `// @experimental:feature_name` — opts in to an experimental feature
//!
//! Unknown pragmas produce `PRAGMA_UNKNOWN` warnings. Duplicate `@version`
//! or `@strict` produces `PRAGMA_DUPLICATE` errors. `@experimental` may
//! appear multiple times — each invocation adds to the feature set.

use leek_diagnostics::{Diagnostic, Severity};
use leek_span::{SourceId, Span};

use crate::Version;

/// Diagnostic codes produced by pragma parsing. Re-exported from the
/// central catalog in [`leek_diagnostics::codes`].
pub use leek_diagnostics::codes;

#[cfg_attr(feature = "salsa", derive(salsa::Update))]
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Pragmas {
    pub version: Version,
    /// True when an explicit `// @version:N` directive set `version`.
    /// Lets callers distinguish a real pragma from the `Default` (V4),
    /// so a pragma-less file can fall back to its out-of-band version
    /// (`Input::version_byte`) instead of always defaulting to v4.
    pub version_explicit: bool,
    pub strict: bool,
    pub experimental: Vec<String>,
}

impl Pragmas {
    /// The version an explicit `// @version:N` pragma selects, else
    /// `fallback` (the out-of-band version: manifest default, corpus case,
    /// or an included file's entry version).
    ///
    /// Use this only at an input boundary (drivers, the include walker).
    /// Passes downstream of the pipeline `Input` read `Input::version_byte`,
    /// which was already settled with this rule.
    #[must_use]
    pub fn effective_version(&self, fallback: Version) -> Version {
        if self.version_explicit {
            self.version
        } else {
            fallback
        }
    }
}

/// Parse pragmas from `text`. Returns the resolved settings and any
/// diagnostics produced. The text itself is not modified — pragmas stay
/// in the byte stream as line comments so spans remain stable.
pub fn parse_pragmas(text: &str, source: SourceId) -> (Pragmas, Vec<Diagnostic>) {
    let mut out = Pragmas::default();
    let mut diags = Vec::new();
    let mut version_set = false;
    let mut strict_set = false;

    // The directive scanner is shared with `leek_span::pragma::language_pragmas`
    // (used by project loading and drivers), so both agree on what a pragma is.
    for directive in leek_span::pragma::directives(text) {
        let name_span = Span::new(source, directive.name_start, directive.name_end);
        let (name, value) = (directive.name, directive.value);

        match name {
            "version" => {
                if version_set {
                    diags.push(Diagnostic::new(
                        codes::PRAGMA_DUPLICATE,
                        Severity::Error,
                        name_span,
                        "duplicate `@version` pragma",
                    ));
                    continue;
                }
                version_set = true;

                let Some(raw) = value else {
                    diags.push(Diagnostic::new(
                        codes::PRAGMA_BAD_VERSION,
                        Severity::Error,
                        name_span,
                        "`@version` requires a value (1..=4)",
                    ));
                    continue;
                };
                match raw.parse::<u32>().ok().and_then(Version::from_pragma) {
                    Some(v) => {
                        out.version = v;
                        out.version_explicit = true;
                    }
                    None => diags.push(Diagnostic::new(
                        codes::PRAGMA_BAD_VERSION,
                        Severity::Error,
                        name_span,
                        format!("invalid version `{raw}` (expected 1, 2, 3, or 4)"),
                    )),
                }
            }
            "strict" => {
                if value.is_some() {
                    diags.push(Diagnostic::new(
                        codes::PRAGMA_INVALID_VALUE,
                        Severity::Error,
                        name_span,
                        "`@strict` is a flag pragma — it does not take a value",
                    ));
                    continue;
                }
                if strict_set {
                    diags.push(Diagnostic::new(
                        codes::PRAGMA_DUPLICATE,
                        Severity::Error,
                        name_span,
                        "duplicate `@strict` pragma",
                    ));
                    continue;
                }
                strict_set = true;
                out.strict = true;
            }
            "experimental" => {
                let Some(feat) = value else {
                    diags.push(Diagnostic::new(
                        codes::PRAGMA_UNKNOWN,
                        Severity::Warning,
                        name_span,
                        "`@experimental` requires a feature name",
                    ));
                    continue;
                };
                out.experimental.push(feat.to_string());
            }
            _ => diags.push(Diagnostic::new(
                codes::PRAGMA_UNKNOWN,
                Severity::Warning,
                name_span,
                format!("unknown pragma `@{name}`"),
            )),
        }
    }

    (out, diags)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(text: &str) -> (Pragmas, Vec<Diagnostic>) {
        parse_pragmas(text, SourceId::new(1).unwrap())
    }

    #[test]
    fn empty_input_defaults() {
        let (p, d) = parse("");
        assert_eq!(p.version, Version::LATEST);
        assert!(!p.strict);
        assert!(p.experimental.is_empty());
        assert!(d.is_empty());
    }

    #[test]
    fn version_pragma_sets_version() {
        let (p, d) = parse("// @version:2\nvar x = 1;");
        assert_eq!(p.version, Version::V2);
        assert!(d.is_empty());
    }

    #[test]
    fn strict_pragma_sets_flag() {
        let (p, d) = parse("// @strict\n");
        assert!(p.strict);
        assert!(d.is_empty());
    }

    #[test]
    fn experimental_pragma_accumulates() {
        let (p, d) = parse("// @experimental:a\n// @experimental:b\n");
        assert_eq!(p.experimental, vec!["a".to_string(), "b".to_string()]);
        assert!(d.is_empty());
    }

    #[test]
    fn unknown_pragma_warns_but_continues() {
        let (p, d) = parse("// @nonsense\n// @version:3\n");
        assert_eq!(p.version, Version::V3);
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].code, codes::PRAGMA_UNKNOWN);
        assert_eq!(d[0].severity, Severity::Warning);
    }

    #[test]
    fn duplicate_version_is_error() {
        let (_, d) = parse("// @version:1\n// @version:2\n");
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].code, codes::PRAGMA_DUPLICATE);
    }

    #[test]
    fn duplicate_strict_is_error() {
        let (_, d) = parse("// @strict\n// @strict\n");
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].code, codes::PRAGMA_DUPLICATE);
    }

    #[test]
    fn bad_version_value_errors() {
        let (p, d) = parse("// @version:9\n");
        // Bad value leaves the version at default.
        assert_eq!(p.version, Version::LATEST);
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].code, codes::PRAGMA_BAD_VERSION);
    }

    #[test]
    fn pragma_not_at_file_top() {
        // Pragmas may appear anywhere — not just the file header.
        let (p, _) = parse("var x = 1;\n// @version:3\n");
        assert_eq!(p.version, Version::V3);
    }

    #[test]
    fn trailing_junk_disqualifies() {
        // `// @version:2 oops` is not a valid directive — the line is just
        // a normal comment.
        let (p, _) = parse("// @version:2 oops\n");
        assert_eq!(p.version, Version::LATEST);
    }

    #[test]
    fn effective_version_prefers_explicit_pragma() {
        let (p, _) = parse("// @version:2\n");
        assert_eq!(p.effective_version(Version::V4), Version::V2);
        let (p, _) = parse("return 1;\n");
        assert_eq!(p.effective_version(Version::V1), Version::V1);
        // An invalid value is not explicit: the fallback applies.
        let (p, _) = parse("// @version:9\n");
        assert_eq!(p.effective_version(Version::V3), Version::V3);
    }

    #[test]
    fn agrees_with_shared_language_scan() {
        for text in [
            "// @version:1\n// @strict\n",
            "//@version:2",
            " // @version:3\n",
            "// @version:0\n// @version:2\n",
            "// @strict:true\n",
            "/* @version:3 */ return 1;",
            "var x = 1;\n// @version:3\n",
        ] {
            let (p, _) = parse(text);
            let scan = leek_span::pragma::language_pragmas(text);
            let explicit = p.version_explicit.then(|| u8::from(p.version));
            assert_eq!(explicit, scan.version, "{text:?}");
            assert_eq!(p.strict, scan.strict, "{text:?}");
        }
    }

    #[test]
    fn allows_leading_whitespace() {
        let (p, _) = parse("    // @version:3\n");
        assert_eq!(p.version, Version::V3);
    }
}
