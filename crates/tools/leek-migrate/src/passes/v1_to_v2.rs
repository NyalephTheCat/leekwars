//! v1 → v2 migration.
//!
//! ## What changes
//!
//! - **`^=` was power-assign in v1; it's xor-assign in v2+.** Any
//!   v1 source that meant `x = x ** n` must be rewritten as
//!   `x **= n`. The `^` operator itself is unchanged — bitwise XOR
//!   in every version — so a bare `x ^ 2` is left alone (it always
//!   meant XOR, even in v1 the upstream lexer treated naked `^`
//!   as XOR; only the `=`-compound form differed).
//!
//! Other v1-vs-v2 differences (string-escape quirks, the special
//! v1 `/*/` short-comment, etc.) are runtime / lexer concerns
//! that don't need source rewriting — `// @version:2` is enough.

use leek_diagnostics::Diagnostic;
use leek_parser::parse;
use leek_rewrite::EditSet;
use leek_span::SourceId;
use leek_syntax::language::NodeOrToken;
use leek_syntax::{SyntaxKind, SyntaxNode, Version};

use crate::MigrationPass;

pub struct V1ToV2;

impl MigrationPass for V1ToV2 {
    fn name(&self) -> &'static str {
        "v1-to-v2"
    }
    fn from_version(&self) -> Version {
        Version::V1
    }
    fn to_version(&self) -> Version {
        Version::V2
    }

    fn collect_edits(
        &self,
        source: &str,
        source_id: SourceId,
        edits: &mut EditSet,
        _diagnostics: &mut Vec<Diagnostic>,
    ) {
        let parsed = parse(source, source_id, Version::V1);
        let root = SyntaxNode::new_root(parsed.green);

        // Walk every token; on a `^=` (CaretEq) swap to `**=`
        // (StarStarEq). Span-replace, not text-only, so the
        // surrounding whitespace stays the way the user wrote it.
        for el in root.descendants_with_tokens() {
            let NodeOrToken::Token(tok) = el else {
                continue;
            };
            if tok.kind() == SyntaxKind::CaretEq {
                let _ = edits.replace_token(&tok, "**=".to_string());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::run_pass;

    fn migrate(src: &str) -> String {
        run_pass(&V1ToV2, src, SourceId::new(1).unwrap()).text
    }

    #[test]
    fn rewrites_caret_assign_to_starstar_assign() {
        let out = migrate("var x = 5\nx ^= 2\n");
        assert!(out.contains("x **= 2"), "got: {out:?}");
        assert!(!out.contains("^="), "got: {out:?}");
    }

    #[test]
    fn leaves_bare_xor_alone() {
        let out = migrate("var x = 5\nvar y = x ^ 2\n");
        assert!(out.contains("x ^ 2"), "got: {out:?}");
    }

    #[test]
    fn preserves_comments_and_blank_lines() {
        let src = "// header\n\n// updates the running power\nvar x = 5\nx ^= 2 // inline note\nreturn x\n";
        let out = migrate(src);
        assert!(out.contains("// header"));
        assert!(out.contains("// updates the running power"));
        assert!(out.contains("// inline note"));
        assert!(out.contains("\n\n"));
        assert!(out.contains("x **= 2"));
    }

    #[test]
    fn bumps_version_pragma() {
        let out = migrate("// @version:1\nvar x = 1\n");
        assert!(out.starts_with("// @version:2\n"));
    }
}
