//! v2 → v1 downgrade.
//!
//! ## Operator semantics shift
//!
//! The big rule: `^=` swapped meaning between v1 and v2.
//!
//! | source token | v1 meaning         | v2+ meaning           |
//! |--------------|--------------------|-----------------------|
//! | `^=`         | power-assign       | xor-assign            |
//! | `**=`        | (does not exist)   | power-assign          |
//! | `^`          | bitwise xor        | bitwise xor (unchanged) |
//!
//! Naively renaming `**=` to `^=` would clobber the meaning of any
//! pre-existing `^=` in the source. We do BOTH rewrites in a
//! single edit pass against the original CST so they don't
//! interfere:
//!
//! - `x **= e` → `x ^= e` (the `^=` token now means power-assign
//!   because the file is about to be tagged `// @version:1`).
//! - `x ^= e` → `x = x ^ (e)` (expand the original v2+ xor-assign
//!   into its long form, which has identical semantics in every
//!   version since standalone `^` is always XOR).
//!
//! The LHS is duplicated textually. For typical lhs shapes (`x`,
//! `arr[i]`, `a.b`) this matches the upstream `^=` desugar, which
//! also re-evaluates the lhs. If a lhs contains a call expression
//! (potential side effects), we emit a `DeprecatedFeature`
//! warning and skip the expansion — the source still has `^=`,
//! which a human can rewrite by hand.
//!
//! ## Map / array convergence
//!
//! v1 doesn't separate Map from Array — both literals land on the
//! same heterogeneous container. The grammar accepts `[:]`,
//! `[k: v]`, `[]`, and `[a, b, c]` in every version, so no
//! literal-form rewrite is needed; downstream builtins
//! (`removeKey`, `mapRemove`, etc.) are sorted out by the v3→v2
//! and v4→v3 passes earlier in the chain.

use leek_diagnostics::{Diagnostic, codes};
use leek_parser::ast::{AstNode, BinaryExpr, SourceFile};
use leek_parser::parse;
use leek_rewrite::EditSet;
use leek_span::{SourceId, Span};
use leek_syntax::{SyntaxKind, SyntaxNode, Version};

use crate::MigrationPass;

pub struct V2ToV1;

impl MigrationPass for V2ToV1 {
    fn name(&self) -> &'static str {
        "v2-to-v1"
    }
    fn from_version(&self) -> Version {
        Version::V2
    }
    fn to_version(&self) -> Version {
        Version::V1
    }

    fn collect_edits(
        &self,
        source: &str,
        source_id: SourceId,
        edits: &mut EditSet,
        diagnostics: &mut Vec<Diagnostic>,
    ) {
        let parsed = parse(source, source_id, Version::V2);
        let root = SyntaxNode::new_root(parsed.green);
        let Some(file) = SourceFile::cast(root.clone()) else {
            return;
        };

        for node in file.syntax().descendants() {
            if node.kind() != SyntaxKind::BinaryExpr {
                continue;
            }
            let Some(bin) = BinaryExpr::cast(node.clone()) else {
                continue;
            };
            let Some(op) = bin.op() else { continue };
            match op.kind() {
                SyntaxKind::StarStarEq => {
                    // `**=` is 3 chars at byte offset op.start(),
                    // we replace with `^=`.
                    let _ = edits.replace_token(&op, "^=".to_string());
                }
                SyntaxKind::CaretEq => {
                    // `^=` is xor-assign in v2; in v1 the SAME
                    // token would mean power-assign. Expand:
                    //   x ^= y    →    x = x ^ (y)
                    let Some(lhs) = bin.lhs() else { continue };
                    let Some(rhs) = bin.rhs() else { continue };
                    let lhs_text = lhs.syntax().text().to_string();
                    let rhs_text = rhs.syntax().text().to_string();

                    if lhs_has_call(lhs.syntax()) {
                        diagnostics.push(diag_call_in_lhs(leek_syntax::node_span(
                            bin.syntax(),
                            source_id,
                        )));
                        continue;
                    }

                    // Replace the entire BinaryExpr text.
                    let new = format!("{lhs_text} = {lhs_text} ^ ({rhs_text})");
                    let _ = edits.replace_node(bin.syntax(), new);
                }
                _ => {}
            }
        }
    }
}

/// True iff `node` contains any `CallExpr` descendant — duplicating
/// such an lhs would call the function twice.
fn lhs_has_call(node: &SyntaxNode) -> bool {
    node.descendants().any(|n| n.kind() == SyntaxKind::CallExpr)
}

fn diag_call_in_lhs(span: Span) -> Diagnostic {
    Diagnostic::warning(
        codes::DEPRECATED_FEATURE,
        span,
        "`^=` xor-assign with a call in the lhs cannot be safely \
         expanded for v1 downgrade — would evaluate the call twice"
            .to_string(),
    )
    .with_note(
        "rewrite this assignment by hand: `tmp = lhs; tmp = tmp ^ rhs; \
         lhs = tmp` or similar"
            .to_string(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::run_pass;

    fn migrate(src: &str) -> String {
        run_pass(&V2ToV1, src, SourceId::new(1).unwrap()).text
    }

    #[test]
    fn star_star_eq_becomes_caret_eq() {
        let out = migrate("var x = 5\nx **= 2\nreturn x\n");
        assert!(out.contains("x ^= 2"), "got: {out}");
        assert!(!out.contains("**="));
    }

    #[test]
    fn caret_eq_xor_assign_expands_to_long_form() {
        let out = migrate("var x = 5\nx ^= 3\nreturn x\n");
        assert!(out.contains("x = x ^ (3)"), "got: {out}");
        assert!(!out.contains("^="), "stale ^=: {out}");
    }

    #[test]
    fn both_rewrites_dont_interfere_in_one_file() {
        // `**=` should become `^=` (power-assign in v1);
        // pre-existing `^=` should become `x = x ^ (rhs)`.
        // Neither should accidentally re-rewrite the other.
        let out = migrate(
            "\
var a = 5\n\
a **= 2\n\
var b = 5\n\
b ^= 3\n\
return [a, b]\n",
        );
        assert!(
            out.contains("a ^= 2"),
            "power-assign rewrite missing: {out}"
        );
        assert!(out.contains("b = b ^ (3)"), "xor expansion missing: {out}");
    }

    #[test]
    fn caret_eq_on_indexed_lhs_expands() {
        let out = migrate("var arr = [1, 2, 3]\narr[1] ^= 5\nreturn arr\n");
        assert!(out.contains("arr[1] = arr[1] ^ (5)"), "got: {out}");
    }

    #[test]
    fn caret_eq_with_call_in_lhs_warns_and_keeps_source() {
        // Duplicating a side-effecting lhs is unsafe. Emit a
        // diagnostic and don't rewrite.
        let res = run_pass(
            &V2ToV1,
            "var arr = []\narr[size(arr)] ^= 1\n",
            SourceId::new(1).unwrap(),
        );
        assert!(
            res.text.contains("arr[size(arr)] ^= 1"),
            "got: {}",
            res.text
        );
        assert!(
            res.diagnostics
                .iter()
                .any(|d| d.message.contains("xor-assign")),
            "no warning: {:?}",
            res.diagnostics
        );
    }

    #[test]
    fn standalone_caret_xor_is_untouched() {
        // `^` standalone always means XOR; no rewrite needed.
        let out = migrate("var x = 5 ^ 3\nreturn x\n");
        assert!(out.contains("var x = 5 ^ 3"), "got: {out}");
    }

    #[test]
    fn bumps_version_pragma() {
        let out = migrate("// @version:2\nvar x = 1\nx **= 2\n");
        assert!(out.starts_with("// @version:1\n"), "got: {out}");
    }

    #[test]
    fn preserves_comments() {
        let src = "\
// @version:2\n\
// power up\n\
var x = 3\n\
x **= 2 // squared\n\
return x\n";
        let out = migrate(src);
        assert!(out.contains("// power up"));
        assert!(out.contains("// squared"));
        assert!(out.contains("x ^= 2"));
    }
}
