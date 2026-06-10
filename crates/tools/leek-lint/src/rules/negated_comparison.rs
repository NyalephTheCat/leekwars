//! L0021 `NegatedComparison` — flag `!(a == b)` and friends, which read
//! more clearly as the negated comparison (`a != b`). A readability hint.

use leek_diagnostics::{Diagnostic, codes};
use leek_hir::{BinaryOp, ExprKind, HirFile, UnaryOp};

use super::{for_each_block, for_each_expr_in_stmt};
use crate::LintRule;

pub struct NegatedComparison;

impl LintRule for NegatedComparison {
    fn name(&self) -> &'static str {
        "negated-comparison"
    }

    fn code(&self) -> leek_diagnostics::Code {
        codes::NEGATED_COMPARISON
    }

    fn check(&self, file: &HirFile, out: &mut Vec<Diagnostic>) {
        for_each_block(file, &mut |block| {
            for stmt in &block.stmts {
                for_each_expr_in_stmt(stmt, &mut |e| {
                    if let ExprKind::Unary(UnaryOp::Not, inner) = &e.kind
                        && let ExprKind::Binary(op, ..) = &inner.kind
                        && let Some((had, want)) = negation(*op)
                    {
                        out.push(diagnostic(had, want, e.span));
                    }
                });
            }
        });
    }
}

/// For a comparison op, the `(spelling, negated-spelling)` pair; `None`
/// for ops without a clean De-Morgan-free negation.
fn negation(op: BinaryOp) -> Option<(&'static str, &'static str)> {
    Some(match op {
        BinaryOp::Eq => ("==", "!="),
        BinaryOp::Ne => ("!=", "=="),
        BinaryOp::Lt => ("<", ">="),
        BinaryOp::Le => ("<=", ">"),
        BinaryOp::Gt => (">", "<="),
        BinaryOp::Ge => (">=", "<"),
        _ => return None,
    })
}

fn diagnostic(had: &str, want: &str, span: leek_span::Span) -> Diagnostic {
    Diagnostic::new(
        codes::NEGATED_COMPARISON,
        leek_diagnostics::Severity::Hint,
        span,
        format!("`!(a {had} b)` is clearer as `a {want} b`"),
    )
    .with_note(format!(
        "rewrite the negated comparison using `{want}` directly"
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
        NegatedComparison.check(&hir, &mut out);
        out
    }

    #[test]
    fn flags_negated_equality() {
        let d = run("function f(a, b) {\n  return !(a == b)\n}\n");
        assert_eq!(d.len(), 1, "got {d:?}");
        assert!(d[0].message.contains("!="), "{d:?}");
    }

    #[test]
    fn flags_negated_less_than() {
        let d = run("function f(a, b) {\n  return !(a < b)\n}\n");
        assert_eq!(d.len(), 1, "got {d:?}");
        assert!(d[0].message.contains(">="), "{d:?}");
    }

    #[test]
    fn ignores_plain_not() {
        let d = run("function f(x) {\n  return !x\n}\n");
        assert!(d.is_empty(), "got {d:?}");
    }
}
