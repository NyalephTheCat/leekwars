//! L0010 `SelfComparison` — flag a comparison whose two operands are
//! the same side-effect-free expression: `x == x`, `a < a`,
//! `this.n != this.n`. Such a comparison is constant (always true or
//! always false) — usually a typo for a different variable.

use leek_diagnostics::{Diagnostic, codes};
use leek_hir::{BinaryOp, ExprKind, HirFile};

use super::structural::{expr_key, has_side_effect};
use super::{for_each_block, for_each_expr_in_stmt};
use crate::LintRule;

pub struct SelfComparison;

impl LintRule for SelfComparison {
    fn name(&self) -> &'static str {
        "self-comparison"
    }

    fn code(&self) -> leek_diagnostics::Code {
        codes::SELF_COMPARISON
    }

    fn check(&self, file: &HirFile, out: &mut Vec<Diagnostic>) {
        for_each_block(file, &mut |block| {
            for stmt in &block.stmts {
                for_each_expr_in_stmt(stmt, &mut |e| {
                    if let ExprKind::Binary(op, a, b) = &e.kind
                        && is_comparison(*op)
                        && !has_side_effect(a)
                        && expr_key(a) == expr_key(b)
                    {
                        out.push(diagnostic(*op, e.span));
                    }
                });
            }
        });
    }
}

fn is_comparison(op: BinaryOp) -> bool {
    matches!(
        op,
        BinaryOp::Eq
            | BinaryOp::Ne
            | BinaryOp::IdentityEq
            | BinaryOp::IdentityNe
            | BinaryOp::Lt
            | BinaryOp::Le
            | BinaryOp::Gt
            | BinaryOp::Ge
    )
}

fn diagnostic(op: BinaryOp, span: leek_span::Span) -> Diagnostic {
    // Equality/`<=`/`>=` are always true; `!=`/`<`/`>` always false.
    let constant = match op {
        BinaryOp::Eq | BinaryOp::IdentityEq | BinaryOp::Le | BinaryOp::Ge => "always true",
        _ => "always false",
    };
    Diagnostic::warning(
        codes::SELF_COMPARISON,
        span,
        "both sides of this comparison are identical".to_string(),
    )
    .with_note(format!(
        "this comparison is {constant} — e.g. `x == x` is always true. \
         Did you mean to compare against a different variable, like `x == y`?"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use leek_parser::ast::{AstNode, SourceFile};
    use leek_parser::parse;
    use leek_span::SourceId;
    use leek_syntax::{SyntaxNode, Version};

    fn run(src: &str) -> Vec<Diagnostic> {
        let source = SourceId::new(1).unwrap();
        let parsed = parse(src, source, Version::V4);
        let ast = SourceFile::cast(SyntaxNode::new_root(parsed.green)).unwrap();
        let (hir, _) = leek_hir::lower_file(&ast, source);
        let mut out = Vec::new();
        SelfComparison.check(&hir, &mut out);
        out
    }

    #[test]
    fn flags_self_equality() {
        let d = run("function f(x) {\n  return x == x\n}\n");
        assert_eq!(d.len(), 1, "got {d:?}");
        assert!(d[0].notes.iter().any(|n| n.contains("always true")));
    }

    #[test]
    fn flags_self_less_than() {
        let d = run("function f(x) {\n  return x < x\n}\n");
        assert_eq!(d.len(), 1, "got {d:?}");
        assert!(d[0].notes.iter().any(|n| n.contains("always false")));
    }

    #[test]
    fn ignores_different_operands() {
        let d = run("function f(x, y) {\n  return x == y\n}\n");
        assert!(d.is_empty(), "got {d:?}");
    }

    #[test]
    fn ignores_calls() {
        // `rand() == rand()` may legitimately differ — not flagged.
        let d = run("var x = rand() == rand()\n");
        assert!(d.is_empty(), "got {d:?}");
    }
}
