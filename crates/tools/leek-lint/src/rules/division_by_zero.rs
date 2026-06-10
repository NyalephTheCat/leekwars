//! L0016 `DivisionByZero` — flag a division or modulo by a literal
//! zero: `x / 0`, `n % 0`, `a \ 0`. The result is never useful (a fault
//! or a non-finite value), so it's almost always a mistake.

use leek_diagnostics::{Diagnostic, codes};
use leek_hir::{BinaryOp, Expr, ExprKind, HirFile, Literal};

use super::{for_each_block, for_each_expr_in_stmt};
use crate::LintRule;

pub struct DivisionByZero;

impl LintRule for DivisionByZero {
    fn name(&self) -> &'static str {
        "division-by-zero"
    }

    fn code(&self) -> leek_diagnostics::Code {
        codes::DIVISION_BY_ZERO
    }

    fn check(&self, file: &HirFile, out: &mut Vec<Diagnostic>) {
        for_each_block(file, &mut |block| {
            for stmt in &block.stmts {
                for_each_expr_in_stmt(stmt, &mut |e| {
                    if let ExprKind::Binary(op, _, rhs) = &e.kind
                        && is_division(*op)
                        && is_zero(rhs)
                    {
                        out.push(diagnostic(*op, e.span));
                    }
                });
            }
        });
    }
}

fn is_division(op: BinaryOp) -> bool {
    matches!(op, BinaryOp::Div | BinaryOp::IntDiv | BinaryOp::Mod)
}

fn is_zero(e: &Expr) -> bool {
    match &e.kind {
        ExprKind::Literal(Literal::Int(0)) => true,
        ExprKind::Literal(Literal::Real(r)) => *r == 0.0,
        _ => false,
    }
}

fn diagnostic(op: BinaryOp, span: leek_span::Span) -> Diagnostic {
    let (verb, what) = match op {
        BinaryOp::Mod => ("take", "modulo by zero"),
        _ => ("divide", "division by zero"),
    };
    Diagnostic::warning(codes::DIVISION_BY_ZERO, span, what.to_string()).with_note(format!(
        "you can't {verb} by zero — this faults or yields a non-finite value at runtime"
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
        DivisionByZero.check(&hir, &mut out);
        out
    }

    #[test]
    fn flags_div_by_zero() {
        let d = run("function f(x) {\n  return x / 0\n}\n");
        assert_eq!(d.len(), 1, "got {d:?}");
    }

    #[test]
    fn flags_mod_by_zero() {
        let d = run("function f(x) {\n  return x % 0\n}\n");
        assert_eq!(d.len(), 1, "got {d:?}");
    }

    #[test]
    fn ignores_nonzero_divisor() {
        let d = run("function f(x) {\n  return x / 2\n}\n");
        assert!(d.is_empty(), "got {d:?}");
    }
}
