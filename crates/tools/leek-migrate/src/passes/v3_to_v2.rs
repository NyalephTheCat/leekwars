//! v3 → v2 downgrade.
//!
//! No syntactic rewrite is needed, but v1/v2 match keywords
//! case-insensitively: an identifier like `Null` or `Return` — legal
//! at v3, where keywords are case-sensitive — collides with a keyword
//! after the downgrade. `var Return = 1` stops compiling; `return
//! Null` silently changes meaning (a v3 variable read becomes the v2
//! `null` literal). There's no faithful rename we can do without
//! whole-program knowledge, so each collision is flagged.
//!
//! Mirrors [`super::v2_to_v3`], which lowercases mis-cased keywords
//! in the upgrade direction.

use leek_diagnostics::Diagnostic;
use leek_rewrite::EditSet;
use leek_span::SourceId;
use leek_syntax::kind::keyword_lookup;
use leek_syntax::{SyntaxKind, Version};

use super::util::behavior_change;
use crate::MigrationPass;

pub struct V3ToV2;

impl MigrationPass for V3ToV2 {
    fn name(&self) -> &'static str {
        "v3-to-v2"
    }
    fn from_version(&self) -> Version {
        Version::V3
    }
    fn to_version(&self) -> Version {
        Version::V2
    }
    fn collect_edits(
        &self,
        source: &str,
        source_id: SourceId,
        _edits: &mut EditSet,
        diagnostics: &mut Vec<Diagnostic>,
    ) {
        let lexed = leek_lexer::lex(source, source_id, Version::V3);
        for tok in &lexed.tokens {
            if tok.kind != SyntaxKind::Ident {
                continue;
            }
            let text = &source[tok.span.range()];
            if keyword_lookup(text, Version::V2).is_some() {
                diagnostics.push(behavior_change(
                    tok.span,
                    format!(
                        "`{text}` is an identifier at v3 but matches the keyword \
                         `{}` case-insensitively at v2",
                        text.to_ascii_lowercase()
                    ),
                    "rename the identifier before downgrading",
                ));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::run_pass;

    #[test]
    fn body_unchanged_pragma_bumped() {
        let src = "// @version:3\nvar x = 1\nreturn x\n";
        let out = run_pass(&V3ToV2, src, SourceId::new(1).unwrap()).text;
        assert!(out.starts_with("// @version:2\n"), "got: {out}");
        assert!(out.contains("var x = 1"));
        assert!(out.contains("return x"));
    }

    #[test]
    fn keyword_colliding_identifier_is_flagged() {
        // `Null` is a plain identifier at v3 but the `null` keyword
        // (case-insensitive) at v2 — `return Null` silently changes
        // from a variable read to the null literal.
        let res = run_pass(&V3ToV2, "return Null\n", SourceId::new(1).unwrap());
        assert!(
            res.text.contains("return Null"),
            "source must be untouched: {}",
            res.text
        );
        assert!(
            res.diagnostics
                .iter()
                .any(|d| d.message.contains("`Null`") && d.message.contains("keyword")),
            "missing flag: {:?}",
            res.diagnostics
        );
    }

    #[test]
    fn ordinary_identifiers_are_not_flagged() {
        let res = run_pass(
            &V3ToV2,
            "var value = 1\nreturn value\n",
            SourceId::new(1).unwrap(),
        );
        assert!(
            res.diagnostics.is_empty(),
            "spurious flag: {:?}",
            res.diagnostics
        );
    }

    #[test]
    fn lowercase_keywords_are_not_flagged() {
        // `return`/`null` lex as keywords at v3, not identifiers.
        let res = run_pass(&V3ToV2, "return null\n", SourceId::new(1).unwrap());
        assert!(
            res.diagnostics.is_empty(),
            "spurious flag: {:?}",
            res.diagnostics
        );
    }
}
