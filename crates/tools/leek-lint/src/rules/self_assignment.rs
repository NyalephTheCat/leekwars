//! L0011 `SelfAssignment` — flag `x = x`, `this.n = this.n`, etc.: an
//! assignment whose target and value are the same side-effect-free
//! place. It does nothing — usually a leftover or a typo.

use leek_diagnostics::{Applicability, Diagnostic, Suggestion, TextEdit, codes};
use leek_hir::{BinaryOp, ExprKind, HirFile};

use super::structural::{expr_key, has_side_effect};
use super::{for_each_block, for_each_expr_in_stmt};
use crate::LintRule;

pub struct SelfAssignment;

impl LintRule for SelfAssignment {
    fn name(&self) -> &'static str {
        "self-assignment"
    }

    fn code(&self) -> leek_diagnostics::Code {
        codes::SELF_ASSIGNMENT
    }

    fn check(&self, file: &HirFile, out: &mut Vec<Diagnostic>) {
        for_each_block(file, &mut |block| {
            for stmt in &block.stmts {
                for_each_expr_in_stmt(stmt, &mut |e| {
                    // Only plain `=`; compound forms (`x += x`) are not
                    // no-ops.
                    if let ExprKind::Binary(BinaryOp::Assign, lhs, rhs) = &e.kind
                        && !has_side_effect(lhs)
                        && expr_key(lhs) == expr_key(rhs)
                    {
                        out.push(diagnostic(e.span));
                    }
                });
            }
        });
    }
}

fn diagnostic(span: leek_span::Span) -> Diagnostic {
    Diagnostic::warning(
        codes::SELF_ASSIGNMENT,
        span,
        "this assignment has no effect (assigns a value to itself)".to_string(),
    )
    .with_note("`x = x` does nothing — remove it, or assign the value you intended")
    .with_suggestion(Suggestion {
        message: "remove the assignment".to_string(),
        edits: vec![TextEdit {
            span,
            replacement: String::new(),
        }],
        // Deleting the expression leaves the statement's `;` behind
        // (a harmless empty statement), so flag for a human glance.
        applicability: Applicability::MaybeIncorrect,
    })
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
        SelfAssignment.check(&hir, &mut out);
        out
    }

    #[test]
    fn flags_variable_self_assignment() {
        let d = run("function f() {\n  var x = 1\n  x = x\n}\n");
        assert_eq!(d.len(), 1, "got {d:?}");
    }

    #[test]
    fn ignores_real_assignment() {
        let d = run("function f(y) {\n  var x = 1\n  x = y\n}\n");
        assert!(d.is_empty(), "got {d:?}");
    }

    #[test]
    fn ignores_compound_assignment() {
        let d = run("function f() {\n  var x = 1\n  x += x\n}\n");
        assert!(d.is_empty(), "got {d:?}");
    }
}
