//! L0015 `AssignmentInCondition` — flag an assignment used as a
//! condition: `if (x = 5)`, `while (n = next())`. This is almost always
//! a typo for `==`; the few intentional uses read more clearly written
//! out, so we always warn.

use leek_diagnostics::{Diagnostic, codes};
use leek_hir::{BinaryOp, Block, Def, Expr, ExprKind, HirFile, Stmt};

use super::for_each_stmt;
use crate::LintRule;

pub struct AssignmentInCondition;

impl LintRule for AssignmentInCondition {
    fn name(&self) -> &'static str {
        "assignment-in-condition"
    }

    fn code(&self) -> leek_diagnostics::Code {
        codes::ASSIGNMENT_IN_CONDITION
    }

    fn check(&self, file: &HirFile, out: &mut Vec<Diagnostic>) {
        check_stmts(&file.main, out);
        for def in &file.defs {
            match def {
                Def::Function(fun) => {
                    if let Some(body) = &fun.body {
                        check_stmts(&body.stmts, out);
                    }
                }
                Def::Class(cls) => {
                    for m in cls.methods.iter().chain(cls.constructors.iter()) {
                        if let Some(body) = &m.body {
                            check_stmts(&body.stmts, out);
                        }
                    }
                }
                Def::Global(_) | Def::Local(_) => {}
            }
        }
    }
}

fn check_stmts(stmts: &[Stmt], out: &mut Vec<Diagnostic>) {
    let wrapper = Block {
        stmts: stmts.to_vec(),
        span: leek_span::Span::synthetic(),
    };
    for_each_stmt(&wrapper, &mut |s| {
        let cond = match s {
            Stmt::If(i) => Some(&i.cond),
            Stmt::While(w) => Some(&w.cond),
            Stmt::DoWhile(d) => Some(&d.cond),
            Stmt::For(f) => f.cond.as_ref(),
            _ => None,
        };
        if let Some(cond) = cond
            && let Some(span) = assignment_span(cond)
        {
            out.push(diagnostic(span));
        }
    });
}

/// The span of `cond` when it's an assignment expression, else `None`.
fn assignment_span(cond: &Expr) -> Option<leek_span::Span> {
    match &cond.kind {
        ExprKind::Binary(op, ..) if is_assignment(*op) => Some(cond.span),
        _ => None,
    }
}

fn is_assignment(op: BinaryOp) -> bool {
    matches!(
        op,
        BinaryOp::Assign
            | BinaryOp::AddAssign
            | BinaryOp::SubAssign
            | BinaryOp::MulAssign
            | BinaryOp::DivAssign
            | BinaryOp::IntDivAssign
            | BinaryOp::ModAssign
            | BinaryOp::PowAssign
            | BinaryOp::BitAndAssign
            | BinaryOp::BitOrAssign
            | BinaryOp::BitXorAssign
            | BinaryOp::ShiftLAssign
            | BinaryOp::ShiftRAssign
            | BinaryOp::UShiftRAssign
            | BinaryOp::NullCoalesceAssign
    )
}

fn diagnostic(span: leek_span::Span) -> Diagnostic {
    Diagnostic::warning(
        codes::ASSIGNMENT_IN_CONDITION,
        span,
        "assignment used as a condition".to_string(),
    )
    .with_note(
        "this assigns and then tests the result — likely a typo for `==`. \
         If the assignment is intentional, move it out of the condition.",
    )
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
        AssignmentInCondition.check(&hir, &mut out);
        out
    }

    #[test]
    fn flags_if_assignment() {
        let d = run("function f() {\n  var x = 0\n  if (x = 5) { return 1 }\n  return 0\n}\n");
        assert_eq!(d.len(), 1, "got {d:?}");
    }

    #[test]
    fn flags_while_assignment() {
        let d = run("function f() {\n  var x = 0\n  while (x = 5) { return 1 }\n  return 0\n}\n");
        assert_eq!(d.len(), 1, "got {d:?}");
    }

    #[test]
    fn ignores_equality_condition() {
        let d = run("function f(x) {\n  if (x == 5) { return 1 }\n  return 0\n}\n");
        assert!(d.is_empty(), "got {d:?}");
    }
}
