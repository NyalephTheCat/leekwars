//! Helpers shared by the migration passes: CST navigation around
//! `NameRef` tokens and span/range conversions. Every pass that
//! renames builtins or inspects call sites uses these.

use leek_diagnostics::{Diagnostic, codes};
use leek_parser::ast::{AstNode, Expr};
use leek_span::{SourceId, Span};
use leek_syntax::language::NodeOrToken;
use leek_syntax::{SyntaxKind, SyntaxNode, SyntaxToken};

/// Extract the `Ident` token from an `Expr` that's a bare
/// `NameRef`. Returns `None` for any other expression shape (e.g.
/// `a.b`, `(x)`, etc.) — those aren't direct references to a
/// top-level builtin.
pub(crate) fn ident_of_name_ref_expr(expr: &Expr) -> Option<SyntaxToken> {
    let Expr::Name(name_ref) = expr else {
        return None;
    };
    name_ref_ident(name_ref.syntax())
}

/// The `Ident` token inside a `NameRef` node.
pub(crate) fn name_ref_ident(node: &SyntaxNode) -> Option<SyntaxToken> {
    node.children_with_tokens()
        .filter_map(NodeOrToken::into_token)
        .find(|t| t.kind() == SyntaxKind::Ident)
}

/// True iff `node` (a NameRef) sits in the field-name slot of a
/// `FieldExpr` (`receiver.field`) — `obj.randFloat` names a user
/// member, not the builtin.
pub(crate) fn is_field_name_position(node: &SyntaxNode) -> bool {
    let Some(parent) = node.parent() else {
        return false;
    };
    if parent.kind() != SyntaxKind::FieldExpr {
        return false;
    }
    let mut seen_dot = false;
    for el in parent.children_with_tokens() {
        match el {
            NodeOrToken::Token(t) if t.kind() == SyntaxKind::Dot => seen_dot = true,
            NodeOrToken::Node(n) if n == *node => return seen_dot,
            _ => {}
        }
    }
    false
}

/// A token's byte range as `(start, end)`.
pub(crate) fn token_range(tok: &SyntaxToken) -> (u32, u32) {
    let r = tok.text_range();
    (u32::from(r.start()), u32::from(r.end()))
}

/// A token's byte range as a [`Span`].
pub(crate) fn token_span(tok: &SyntaxToken, source_id: SourceId) -> Span {
    let (start, end) = token_range(tok);
    Span::new(source_id, start, end)
}

/// A `MigrationBehaviorChange` (W0511) warning: the construct still
/// compiles at the target version, but evaluates differently there.
pub(crate) fn behavior_change(span: Span, message: String, note: &str) -> Diagnostic {
    Diagnostic::warning(codes::MIGRATION_BEHAVIOR_CHANGE, span, message).with_note(note.to_string())
}

/// True iff `expr` is the `null` literal.
pub(crate) fn is_null_literal(expr: &Expr) -> bool {
    matches!(
        expr,
        Expr::Literal(lit) if lit.token().is_some_and(|t| t.kind() == SyntaxKind::KwNull)
    )
}

/// True iff `kind` is any of the assignment operators (`=` and the
/// compound forms).
pub(crate) fn is_assign_op(kind: SyntaxKind) -> bool {
    matches!(
        kind,
        SyntaxKind::Eq
            | SyntaxKind::PlusEq
            | SyntaxKind::MinusEq
            | SyntaxKind::StarEq
            | SyntaxKind::SlashEq
            | SyntaxKind::BackslashEq
            | SyntaxKind::PercentEq
            | SyntaxKind::StarStarEq
            | SyntaxKind::ShiftLeftEq
            | SyntaxKind::ShiftRightEq
            | SyntaxKind::UShiftRightEq
            | SyntaxKind::AmpEq
            | SyntaxKind::PipeEq
            | SyntaxKind::CaretEq
            | SyntaxKind::QuestionQuestionEq
    )
}

/// True iff `node` or any of its descendants is a `NameRef` to
/// `name`. Used by the self-reference detectors.
pub(crate) fn mentions_name(node: &SyntaxNode, name: &str) -> bool {
    node.descendants()
        .filter(|n| n.kind() == SyntaxKind::NameRef)
        .filter_map(|n| name_ref_ident(&n))
        .any(|t| t.text() == name)
}
