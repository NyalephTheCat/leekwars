//! L0012 `RedundantBoolean` — flag comparisons against a boolean
//! literal: `x == true`, `x != false`, `flag == false`. These simplify
//! to `x` or `!x`, and the lint ships a machine-applicable autofix.

use leek_diagnostics::{Applicability, Diagnostic, Suggestion, TextEdit, codes};
use leek_hir::{BinaryOp, Expr, ExprKind, Literal};
use leek_span::Span;

use crate::LintGroup;
use crate::pass::{LintCx, LintMeta, LintPass};

pub struct RedundantBoolean;

static META: LintMeta = LintMeta {
    name: "redundant-boolean",
    code: codes::REDUNDANT_BOOLEAN,
    group: LintGroup::Complexity,
    description: "comparison against a boolean literal — `x == true` is just `x`",
};

impl LintPass for RedundantBoolean {
    fn meta(&self) -> &'static LintMeta {
        &META
    }

    fn check_expr(&mut self, cx: &mut LintCx<'_, '_>, e: &Expr) {
        if let ExprKind::Binary(op, a, b) = &e.kind
            && matches!(op, BinaryOp::Eq | BinaryOp::Ne)
        {
            // Whichever side is the boolean literal; the other side is
            // the operand we keep.
            if let Some(v) = bool_lit(b) {
                cx.emit(diagnostic(*op, v, e, a, /* literal_on_right = */ true));
            } else if let Some(v) = bool_lit(a) {
                cx.emit(diagnostic(
                    *op, v, e, b, /* literal_on_right = */ false,
                ));
            }
        }
    }
}

fn bool_lit(e: &Expr) -> Option<bool> {
    match &e.kind {
        ExprKind::Literal(Literal::Bool(b)) => Some(*b),
        _ => None,
    }
}

fn diagnostic(
    op: BinaryOp,
    literal: bool,
    expr: &Expr,
    operand: &Expr,
    literal_on_right: bool,
) -> Diagnostic {
    // `== true` / `!= false` keep the value; `== false` / `!= true`
    // negate it.
    let negate = matches!((op, literal), (BinaryOp::Eq, false) | (BinaryOp::Ne, true));
    let example = if negate {
        "`x == false` is just `!x`"
    } else {
        "`x == true` is just `x`"
    };
    Diagnostic::new(
        codes::REDUNDANT_BOOLEAN,
        leek_diagnostics::Severity::Hint,
        expr.span,
        "comparison against a boolean literal is redundant".to_string(),
    )
    .with_note(format!(
        "a boolean is already truthy on its own — {example}"
    ))
    .with_suggestion(simplify(expr.span, operand.span, literal_on_right, negate))
}

/// Build the span-based edits that turn `x == true` into `x` (or
/// `x == false` into `!(x)`). No source text needed — we delete the
/// comparison-and-literal part and, when negating, wrap the operand.
fn simplify(es: Span, os: Span, literal_on_right: bool, negate: bool) -> Suggestion {
    let src = es.source;
    let edits: Vec<TextEdit> = if literal_on_right {
        // operand is on the left; the ` <op> <lit>` tail is [os.end, es.end).
        if negate {
            vec![
                TextEdit {
                    span: Span::new(src, os.start, os.start),
                    replacement: "!(".to_string(),
                },
                TextEdit {
                    span: Span::new(src, os.end, es.end),
                    replacement: ")".to_string(),
                },
            ]
        } else {
            vec![TextEdit {
                span: Span::new(src, os.end, es.end),
                replacement: String::new(),
            }]
        }
    } else {
        // operand is on the right; the `<lit> <op> ` head is [es.start, os.start).
        if negate {
            vec![
                TextEdit {
                    span: Span::new(src, es.start, os.start),
                    replacement: "!(".to_string(),
                },
                TextEdit {
                    span: Span::new(src, os.end, os.end),
                    replacement: ")".to_string(),
                },
            ]
        } else {
            vec![TextEdit {
                span: Span::new(src, es.start, os.start),
                replacement: String::new(),
            }]
        }
    };
    Suggestion {
        message: if negate {
            "negate the operand instead".to_string()
        } else {
            "use the operand directly".to_string()
        },
        edits,
        applicability: Applicability::MachineApplicable,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::lint_one;

    fn run(src: &str) -> Vec<Diagnostic> {
        lint_one(RedundantBoolean, src)
    }

    #[test]
    fn flags_eq_true_with_autofix() {
        let d = run("function f(x) {\n  return x == true\n}\n");
        assert_eq!(d.len(), 1, "got {d:?}");
        let sug = &d[0].suggestions[0];
        assert_eq!(sug.applicability, Applicability::MachineApplicable);
        // The fix deletes the ` == true` tail (one edit, empty replacement).
        assert_eq!(sug.edits.len(), 1);
        assert_eq!(sug.edits[0].replacement, "");
    }

    #[test]
    fn flags_eq_false_negates() {
        let d = run("function f(x) {\n  return x == false\n}\n");
        assert_eq!(d.len(), 1, "got {d:?}");
        let sug = &d[0].suggestions[0];
        // Negation wraps the operand: insert `!(` and `)`.
        assert_eq!(sug.edits.len(), 2);
        assert!(sug.edits.iter().any(|e| e.replacement == "!("));
        assert!(sug.edits.iter().any(|e| e.replacement == ")"));
    }

    #[test]
    fn ignores_normal_comparison() {
        let d = run("function f(x, y) {\n  return x == y\n}\n");
        assert!(d.is_empty(), "got {d:?}");
    }
}
