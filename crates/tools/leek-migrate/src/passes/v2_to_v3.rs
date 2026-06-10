//! v2 → v3 migration.
//!
//! v1/v2 match keywords case-insensitively (`TRUE`, `Return`, `NOT`
//! all work); v3+ is fully case-sensitive, so a non-lowercase keyword
//! stops being a keyword after the bump — usually a compile error
//! (`Return 1`), occasionally a silent behavior change (`Null`
//! becomes a fresh variable that reads as null anyway, `TRUE` an
//! unknown name). The pass lexes under v2 and lowercases every
//! keyword token that isn't already lowercase.
//!
//! `class` is case-sensitive even at v2 (`LexicalParser.java:449`),
//! so `Class` lexes as a plain identifier there and is correctly left
//! alone.

use leek_diagnostics::Diagnostic;
use leek_rewrite::EditSet;
use leek_span::SourceId;
use leek_syntax::Version;

use crate::MigrationPass;

pub struct V2ToV3;

impl MigrationPass for V2ToV3 {
    fn name(&self) -> &'static str {
        "v2-to-v3"
    }
    fn from_version(&self) -> Version {
        Version::V2
    }
    fn to_version(&self) -> Version {
        Version::V3
    }
    fn collect_edits(
        &self,
        source: &str,
        source_id: SourceId,
        edits: &mut EditSet,
        _diagnostics: &mut Vec<Diagnostic>,
    ) {
        let lexed = leek_lexer::lex(source, source_id, Version::V2);
        for tok in &lexed.tokens {
            if !tok.kind.is_keyword() {
                continue;
            }
            let text = &source[tok.span.range()];
            if text.bytes().any(|b| b.is_ascii_uppercase()) {
                let _ = edits.replace_span(tok.span, text.to_ascii_lowercase());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::run_pass;

    fn migrate(src: &str) -> String {
        run_pass(&V2ToV3, src, SourceId::new(1).unwrap()).text
    }

    #[test]
    fn body_unchanged_pragma_bumped() {
        let out = migrate("// @version:2\nvar x = 1\n");
        assert_eq!(out, "// @version:3\nvar x = 1\n");
    }

    #[test]
    fn lowercases_mis_cased_keywords() {
        let out = migrate("Return TRUE\n");
        assert_eq!(out, "// @version:3\nreturn true\n");
    }

    #[test]
    fn lowercases_null_and_not() {
        let out = migrate("VAR x = Null\nreturn NOT x\n");
        assert_eq!(out, "// @version:3\nvar x = null\nreturn not x\n");
    }

    #[test]
    fn leaves_identifiers_and_strings_alone() {
        // `Class` is case-sensitive even at v2 → a plain identifier.
        // Keyword-looking words inside strings/comments stay put.
        let out = migrate("var Class = 1\nvar s = 'TRUE'\n// Return TRUE\nreturn Class\n");
        assert!(out.contains("var Class = 1"), "got: {out}");
        assert!(out.contains("'TRUE'"), "got: {out}");
        assert!(out.contains("// Return TRUE"), "got: {out}");
        assert!(out.contains("return Class"), "got: {out}");
    }

    #[test]
    fn already_lowercase_is_untouched() {
        let out = migrate("var x = true\nreturn x\n");
        assert!(out.contains("var x = true\nreturn x\n"));
    }
}
