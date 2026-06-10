//! v1 → v2 migration.
//!
//! ## Rewrites
//!
//! - **`^=` was power-assign in v1; it's xor-assign in v2+.** Any
//!   v1 source that meant `x = x ** n` must be rewritten as
//!   `x **= n`. The `^` operator itself is unchanged — bitwise XOR
//!   in every version — so a bare `x ^ 2` is left alone (it always
//!   meant XOR, even in v1 the upstream lexer treated naked `^`
//!   as XOR; only the `=`-compound form differed).
//! - **The `/*/` short comment.** v1 reads `/*/` as a complete
//!   block comment; v2+ reads it as an unterminated opener that
//!   swallows the rest of the file. Rewritten to `/**/`.
//! - **Escaped delimiters in strings.** v1 keeps `\<matching-delim>`
//!   verbatim in the string's content (`"a\"b"` is four chars at
//!   v1); v2+ consumes the escape. Each one is rewritten to
//!   `\\\<delim>` — escaped backslash plus escaped delimiter — which
//!   spells the same content at v2.
//! - **Constant division by zero.** `lit / 0` (or `/ null`) yields
//!   `null` at v1 but `∞`/NaN at v2+; a constant lhs lets us fold
//!   the whole expression to `null`. A non-constant lhs is only
//!   flagged — rewriting would drop its evaluation.
//!
//! ## Flags (no faithful rewrite exists)
//!
//! Copy-semantics and builtin drift shared with the downgrade
//! direction — see [`super::boundary12`].

use leek_diagnostics::Diagnostic;
use leek_parser::ast::{AstNode, SourceFile};
use leek_parser::parse;
use leek_rewrite::EditSet;
use leek_span::SourceId;
use leek_syntax::language::NodeOrToken;
use leek_syntax::{SyntaxKind, SyntaxNode, Version};

use super::boundary12;
use super::util::behavior_change;
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
        diagnostics: &mut Vec<Diagnostic>,
    ) {
        // ---- Lexer-level quirks --------------------------------
        let lexed = leek_lexer::lex(source, source_id, Version::V1);
        for tok in &lexed.tokens {
            if tok.kind == SyntaxKind::BlockComment && &source[tok.span.range()] == "/*/" {
                // v1's three-char comment; at v2 it would swallow
                // the rest of the file.
                let _ = edits.replace_span(tok.span, "/**/".to_string());
            }
        }
        boundary12::for_each_delim_escape(source, source_id, Version::V1, |span| {
            // v1 reads `\"` as backslash + quote, both in the string's
            // content (the escape is not consumed). Spell that content
            // explicitly for v2: `\\` (escaped backslash) + `\"`
            // (escaped delimiter) — i.e. the lone `\` becomes `\\\`.
            let _ = edits.replace_span(span, "\\\\\\".to_string());
        });

        // ---- CST-level rewrites --------------------------------
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

        let Some(file) = SourceFile::cast(root.clone()) else {
            return;
        };

        // Constant division by zero: fold to v1's result.
        boundary12::for_each_div_by_zero(&file, |bin, lhs_is_literal| {
            if lhs_is_literal {
                let _ = edits.replace_node(bin.syntax(), "null".to_string());
            } else {
                diagnostics.push(behavior_change(
                    leek_syntax::node_span(bin.syntax(), source_id),
                    "division by a literal zero or null yields null at v1 but ∞/NaN at v2+"
                        .to_string(),
                    "the dividend isn't a constant, so the expression can't be folded to \
                     null automatically — rewrite it by hand",
                ));
            }
        });

        // Drift with no faithful rewrite — flag for manual review.
        boundary12::flag_param_semantics(&file, source_id, diagnostics);
        boundary12::flag_builtin_drift(&file, source_id, diagnostics);
        boundary12::flag_aliasing_drift(&file, source_id, diagnostics);
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

    #[test]
    fn short_comment_becomes_empty_block_comment() {
        // `/*/` is a complete comment at v1, an unterminated opener
        // at v2 — `return 1` must survive the migration.
        let out = migrate("/*/ return 1\n");
        assert_eq!(out.trim_start_matches("// @version:2\n"), "/**/ return 1\n");
    }

    #[test]
    fn escaped_delimiter_keeps_v1_content() {
        // v1: `"abc\"def"` has content `abc\"def` (8 chars — the
        // backslash AND the quote). The v2 spelling of that content
        // is `"abc\\\"def"`.
        let out = migrate("return length(\"abc\\\"def\")\n");
        assert!(out.contains("\"abc\\\\\\\"def\""), "got: {out:?}");
    }

    #[test]
    fn double_backslash_is_not_re_escaped() {
        let src = "return \"a\\\\b\"\n"; // "a\\b"
        let out = migrate(src);
        assert!(out.contains("\"a\\\\b\""), "got: {out:?}");
    }

    #[test]
    fn constant_division_by_zero_folds_to_null() {
        let out = migrate("return 1 / 0\n");
        assert!(out.contains("return null"), "got: {out}");
        let out = migrate("return 8 / null\n");
        assert!(out.contains("return null"), "got: {out}");
    }

    #[test]
    fn dynamic_division_by_zero_is_flagged_not_rewritten() {
        let res = run_pass(
            &V1ToV2,
            "var x = 4\nreturn x / 0\n",
            SourceId::new(1).unwrap(),
        );
        assert!(res.text.contains("x / 0"), "got: {}", res.text);
        assert!(
            res.diagnostics
                .iter()
                .any(|d| d.message.contains("division")),
            "missing flag: {:?}",
            res.diagnostics
        );
    }

    #[test]
    fn ordinary_division_untouched() {
        let out = migrate("return 6 / 2\n");
        assert!(out.contains("6 / 2"), "got: {out}");
    }

    #[test]
    fn function_with_params_is_flagged() {
        let res = run_pass(
            &V1ToV2,
            "function f(a) { return a }\nreturn f(1)\n",
            SourceId::new(1).unwrap(),
        );
        assert!(
            res.diagnostics
                .iter()
                .any(|d| d.message.contains("argument passing")),
            "missing flag: {:?}",
            res.diagnostics
        );
    }

    #[test]
    fn sort_call_is_flagged() {
        let res = run_pass(
            &V1ToV2,
            "var a = [3, null, 1]\nsort(a)\nreturn a\n",
            SourceId::new(1).unwrap(),
        );
        assert!(
            res.diagnostics.iter().any(|d| d.message.contains("sort")),
            "missing flag: {:?}",
            res.diagnostics
        );
    }

    #[test]
    fn lambda_with_params_is_flagged() {
        let res = run_pass(
            &V1ToV2,
            "var f = function(a) { return a + 1 }\nreturn f(1)\n",
            SourceId::new(1).unwrap(),
        );
        assert!(
            res.diagnostics
                .iter()
                .any(|d| d.message.contains("argument passing")),
            "missing flag: {:?}",
            res.diagnostics
        );
    }

    #[test]
    fn returned_name_in_function_is_flagged() {
        let res = run_pass(
            &V1ToV2,
            "var x = [1]\nvar f = function() { return x }\npush(f(), 5)\nreturn x\n",
            SourceId::new(1).unwrap(),
        );
        assert!(
            res.diagnostics
                .iter()
                .any(|d| d.message.contains("returned")),
            "missing flag: {:?}",
            res.diagnostics
        );
    }

    #[test]
    fn top_level_return_is_not_flagged_as_aliased() {
        let res = run_pass(
            &V1ToV2,
            "var x = [1]\nreturn x\n",
            SourceId::new(1).unwrap(),
        );
        assert!(
            !res.diagnostics
                .iter()
                .any(|d| d.message.contains("returned")),
            "spurious flag: {:?}",
            res.diagnostics
        );
    }

    #[test]
    fn at_reference_is_flagged() {
        let res = run_pass(
            &V1ToV2,
            "var a = 1\nvar t = @a\nt++\nreturn a\n",
            SourceId::new(1).unwrap(),
        );
        assert!(
            res.diagnostics.iter().any(|d| d.message.contains("`@`")),
            "missing flag: {:?}",
            res.diagnostics
        );
    }

    #[test]
    fn self_referential_push_is_flagged() {
        let res = run_pass(
            &V1ToV2,
            "var a = []\npush(a, [a])\nreturn a\n",
            SourceId::new(1).unwrap(),
        );
        assert!(
            res.diagnostics.iter().any(|d| d.message.contains("itself")),
            "missing flag: {:?}",
            res.diagnostics
        );
    }

    #[test]
    fn self_referential_index_assign_is_flagged() {
        let res = run_pass(
            &V1ToV2,
            "var a = [1]\na[0] = a\nreturn a\n",
            SourceId::new(1).unwrap(),
        );
        assert!(
            res.diagnostics.iter().any(|d| d.message.contains("itself")),
            "missing flag: {:?}",
            res.diagnostics
        );
    }

    #[test]
    fn ordinary_push_is_not_flagged_as_self_referential() {
        let res = run_pass(
            &V1ToV2,
            "var a = []\nvar b = 2\npush(a, b)\nreturn a\n",
            SourceId::new(1).unwrap(),
        );
        assert!(
            !res.diagnostics.iter().any(|d| d.message.contains("itself")),
            "spurious flag: {:?}",
            res.diagnostics
        );
    }

    #[test]
    fn array_vs_null_equality_is_flagged() {
        let res = run_pass(
            &V1ToV2,
            "return [null] != null\n",
            SourceId::new(1).unwrap(),
        );
        assert!(
            res.diagnostics.iter().any(|d| d.message.contains("null")),
            "missing flag: {:?}",
            res.diagnostics
        );
    }
}
