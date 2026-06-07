//! Standalone type-checking helpers.

use leek_parser::ast::Expr;
use leek_syntax::SyntaxKind;

use crate::ty::{Type, promote_numeric};

/// Walk a function declaration's body and return `true` if any
/// `ReturnStmt` is reached. Doesn't analyze reachability — even
/// `if (false) return 1;` counts. Used by [`collect_fn_signature`]
/// to distinguish "no return → null" from "may return → any".
pub(crate) fn has_return_stmt(node: &leek_syntax::SyntaxNode) -> bool {
    if node.kind() == SyntaxKind::ReturnStmt {
        return true;
    }
    node.children().any(|c| has_return_stmt(&c))
}

/// Compute the result type of a binary expression given the operator
/// kind and operand types. `+` with a string operand short-circuits
/// to string concatenation.
pub(crate) fn binary_result_type(
    op_kind: Option<SyntaxKind>,
    lhs_ty: &Type,
    rhs_ty: &Type,
) -> Type {
    match op_kind {
        Some(SyntaxKind::Plus) => {
            if matches!(lhs_ty, Type::String) || matches!(rhs_ty, Type::String) {
                Type::String
            } else {
                promote_numeric(lhs_ty, rhs_ty)
            }
        }
        Some(
            SyntaxKind::Minus
            | SyntaxKind::Star
            | SyntaxKind::Slash
            | SyntaxKind::Percent
            | SyntaxKind::Backslash
            | SyntaxKind::StarStar,
        ) => promote_numeric(lhs_ty, rhs_ty),
        Some(
            SyntaxKind::EqEq
            | SyntaxKind::NotEq
            | SyntaxKind::EqEqEq
            | SyntaxKind::NotEqEq
            | SyntaxKind::Lt
            | SyntaxKind::Le
            | SyntaxKind::Gt
            | SyntaxKind::Ge
            | SyntaxKind::AmpAmp
            | SyntaxKind::PipePipe
            | SyntaxKind::KwAnd
            | SyntaxKind::KwOr
            | SyntaxKind::KwIs
            | SyntaxKind::KwInstanceof
            | SyntaxKind::KwIn,
        ) => Type::Boolean,
        Some(SyntaxKind::Eq) => rhs_ty.clone(),
        _ => Type::Any,
    }
}

/// True for `=` (assignment). Compound assignments are excluded from
/// the strict type-mismatch check because they implicitly cast.
pub(crate) fn is_plain_assignment(kind: Option<SyntaxKind>) -> bool {
    matches!(kind, Some(SyntaxKind::Eq))
}

/// True for any `op=` compound assignment.
pub(crate) fn is_compound_assignment(kind: SyntaxKind) -> bool {
    matches!(
        kind,
        SyntaxKind::PlusEq
            | SyntaxKind::MinusEq
            | SyntaxKind::StarEq
            | SyntaxKind::SlashEq
            | SyntaxKind::PercentEq
            | SyntaxKind::BackslashEq
            | SyntaxKind::StarStarEq
            | SyntaxKind::AmpEq
            | SyntaxKind::PipeEq
            | SyntaxKind::CaretEq
            | SyntaxKind::ShiftLeftEq
            | SyntaxKind::ShiftRightEq
            | SyntaxKind::UShiftRightEq
            | SyntaxKind::QuestionQuestionEq
    )
}

/// Canonical text of a literal map key — used for duplicate-key
/// detection. Returns `None` for non-literal keys (variables,
/// exprs) where we can't decide statically.
pub(crate) fn literal_key_canonical(e: &Expr) -> Option<String> {
    let Expr::Literal(lit) = e else { return None };
    let tok = lit.token()?;
    match tok.kind() {
        SyntaxKind::IntLiteral | SyntaxKind::RealLiteral => Some(tok.text().to_string()),
        SyntaxKind::StringLiteral => {
            let s = tok.text();
            if s.len() >= 2
                && let Some(first) = s.chars().next()
                && let Some(last) = s.chars().next_back()
                && first == last
                && (first == '\'' || first == '"')
            {
                Some(format!("str:{}", &s[1..s.len() - 1]))
            } else {
                Some(format!("str:{s}"))
            }
        }
        SyntaxKind::KwTrue => Some("true".into()),
        SyntaxKind::KwFalse => Some("false".into()),
        SyntaxKind::KwNull => Some("null".into()),
        _ => None,
    }
}
