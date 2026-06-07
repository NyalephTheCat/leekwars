//! L0013 `DoubleNegation` — flag `!!x` / `not not x`, which is just
//! `x` (booleanized). Ships a machine-applicable autofix that strips
//! the leading `!!`.

use leek_diagnostics::{Applicability, Diagnostic, Suggestion, TextEdit, codes};
use leek_hir::{ExprKind, HirFile, UnaryOp};
use leek_span::Span;

use super::{for_each_block, for_each_expr_in_stmt};
use crate::LintRule;

pub struct DoubleNegation;

impl LintRule for DoubleNegation {
    fn name(&self) -> &'static str {
        "double-negation"
    }

    fn code(&self) -> leek_diagnostics::Code {
        codes::DOUBLE_NEGATION
    }

    fn check(&self, file: &HirFile, out: &mut Vec<Diagnostic>) {
        for_each_block(file, &mut |block| {
            for stmt in &block.stmts {
                for_each_expr_in_stmt(stmt, &mut |e| {
                    if let ExprKind::Unary(UnaryOp::Not, inner) = &e.kind
                        && let ExprKind::Unary(UnaryOp::Not, innermost) = &inner.kind
                    {
                        out.push(diagnostic(e.span, innermost.span));
                    }
                });
            }
        });
    }
}

fn diagnostic(outer: Span, operand: Span) -> Diagnostic {
    // Delete the `!!` (everything from the outer `!` up to the operand).
    let fix = Suggestion {
        message: "remove the double negation".to_string(),
        edits: vec![TextEdit {
            span: Span::new(outer.source, outer.start, operand.start),
            replacement: String::new(),
        }],
        applicability: Applicability::MachineApplicable,
    };
    Diagnostic::new(
        codes::DOUBLE_NEGATION,
        leek_diagnostics::Severity::Hint,
        outer,
        "double negation is redundant".to_string(),
    )
    .with_note("`!!x` is just `x` — remove the extra `!`")
    .with_suggestion(fix)
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
        DoubleNegation.check(&hir, &mut out);
        out
    }

    #[test]
    fn flags_double_bang() {
        let d = run("function f(x) {\n  return !!x\n}\n");
        assert_eq!(d.len(), 1, "got {d:?}");
        assert_eq!(
            d[0].suggestions[0].applicability,
            Applicability::MachineApplicable
        );
    }

    #[test]
    fn ignores_single_negation() {
        let d = run("function f(x) {\n  return !x\n}\n");
        assert!(d.is_empty(), "got {d:?}");
    }
}
