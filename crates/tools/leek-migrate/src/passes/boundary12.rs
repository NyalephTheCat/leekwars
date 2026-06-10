//! Semantic drift shared by both passes at the v1/v2 boundary.
//!
//! v1 is the odd one out on copy semantics and a couple of container
//! builtins. None of that is expressible as a faithful source
//! rewrite, so both adjacent passes surface the same
//! `MigrationBehaviorChange` warnings from the detection walks here.
//! The v1 quirks that CAN be rewritten (`/*/` comments, string
//! escapes, constant division by zero) stay in the passes themselves
//! because the rewrite differs per direction.
//!
//! The drift list is corpus-derived (see
//! `examples/corpus_verify.rs`):
//!
//! - **Copy semantics.** v1 deep-copies non-`@` arguments AND clones
//!   return values; v2+ passes containers by reference. Conversely a
//!   v1 `@` parameter aliases even scalars (`f(@a) { a++ }` mutates
//!   the caller's int), which v2 does not.
//! - **`sort` null placement.** Nulls sort last at v1, first at v2+.
//! - **`arrayFilter` keys.** v1 keeps the original keys in the
//!   result; v2+ reindexes from 0.
//! - **Division by zero.** `x / 0` (or `/ null`) yields `null` at
//!   v1 but `∞`/NaN at v2+.
//! - **String escapes.** `\<matching-delimiter>` keeps its backslash
//!   in the string content at v1 but drops it at v2+.
//! - **Aliasing extras.** The copy-semantics flip also shows up
//!   outside parameter lists: returned containers (cloned at v1,
//!   aliased at v2+), explicit `@` references (alias even scalars at
//!   v1), self-referential inserts (`push(a, [a])` deep-copies at v1
//!   but creates a cycle at v2+), and `==`/`!=` between an array
//!   literal and `null` (v1's unified container compares differently).

use leek_diagnostics::Diagnostic;
use leek_parser::ast::{AstNode, BinaryExpr, CallExpr, Expr, ReturnStmt, SourceFile};
use leek_span::{SourceId, Span};
use leek_syntax::language::NodeOrToken;
use leek_syntax::{SyntaxKind, Version};

use super::util::{
    behavior_change, ident_of_name_ref_expr, is_assign_op, is_null_literal, mentions_name,
    token_span,
};

/// Call `f` for every `lhs / rhs` whose rhs is a literal `0`, `0.0`,
/// or `null`. The bool reports whether the lhs is ALSO a literal —
/// constant-foldable, so the upgrade pass may rewrite the whole
/// expression; a non-literal lhs can only be flagged.
pub(crate) fn for_each_div_by_zero(file: &SourceFile, mut f: impl FnMut(&BinaryExpr, bool)) {
    for node in file.syntax().descendants() {
        if node.kind() != SyntaxKind::BinaryExpr {
            continue;
        }
        let Some(bin) = BinaryExpr::cast(node) else {
            continue;
        };
        let Some(op) = bin.op() else { continue };
        if op.kind() != SyntaxKind::Slash {
            continue;
        }
        let Some(rhs) = bin.rhs() else { continue };
        if !is_zero_or_null_literal(&rhs) {
            continue;
        }
        let lhs_is_literal = matches!(bin.lhs(), Some(Expr::Literal(_)));
        f(&bin, lhs_is_literal);
    }
}

fn is_zero_or_null_literal(expr: &Expr) -> bool {
    let Expr::Literal(lit) = expr else {
        return false;
    };
    let Some(tok) = lit.token() else {
        return false;
    };
    match tok.kind() {
        SyntaxKind::KwNull => true,
        SyntaxKind::IntLiteral => tok.text().parse::<i64>() == Ok(0),
        SyntaxKind::RealLiteral => tok.text().parse::<f64>() == Ok(0.0),
        _ => false,
    }
}

/// One warning per function (declaration or lambda) that takes
/// parameters: argument and return-value copy semantics flip at the
/// boundary.
pub(crate) fn flag_param_semantics(
    file: &SourceFile,
    source_id: SourceId,
    diagnostics: &mut Vec<Diagnostic>,
) {
    for node in file.syntax().descendants() {
        if !matches!(node.kind(), SyntaxKind::FnDecl | SyntaxKind::LambdaExpr) {
            continue;
        }
        let Some(params) = node.children().find(|n| n.kind() == SyntaxKind::ParamList) else {
            continue;
        };
        if !params.children().any(|n| n.kind() == SyntaxKind::Param) {
            continue;
        }
        diagnostics.push(behavior_change(
            leek_syntax::node_span(&params, source_id),
            "argument passing changes meaning across the v1/v2 boundary".to_string(),
            "v1 deep-copies non-`@` arguments and clones return values, while v2+ \
             passes containers by reference; a v1 `@` parameter aliases even \
             scalars, which v2+ does not — review call sites that mutate \
             parameters or returned containers",
        ));
    }
}

/// Builtins whose behavior differs between v1 and v2+ in ways no
/// rewrite can bridge.
const BUILTIN_DRIFT: &[(&str, &str)] = &[
    ("sort", "null elements sort last at v1 but first at v2+"),
    (
        "arrayFilter",
        "the result keeps the original keys at v1 but is reindexed from 0 at v2+",
    ),
];

/// One warning per call to a builtin from [`BUILTIN_DRIFT`].
pub(crate) fn flag_builtin_drift(
    file: &SourceFile,
    source_id: SourceId,
    diagnostics: &mut Vec<Diagnostic>,
) {
    for node in file.syntax().descendants() {
        if node.kind() != SyntaxKind::CallExpr {
            continue;
        }
        let Some(call) = CallExpr::cast(node.clone()) else {
            continue;
        };
        let Some(callee) = call.callee() else {
            continue;
        };
        let Some(ident) = ident_of_name_ref_expr(&callee) else {
            continue;
        };
        let Some((name, what)) = BUILTIN_DRIFT.iter().find(|(n, _)| *n == ident.text()) else {
            continue;
        };
        diagnostics.push(behavior_change(
            leek_syntax::node_span(call.syntax(), source_id),
            format!("`{name}`: {what}"),
            "verify the surrounding code doesn't depend on this difference",
        ));
    }
}

/// Builtins whose later arguments are inserted into the container
/// named by the first argument.
const INSERT_BUILTINS: &[&str] = &["push", "insert", "unshift", "pushAll"];

/// The v1 aliasing model surfaces outside parameter lists too. Flags:
///
/// - `return x` of a bare name inside a function — the container is
///   cloned at v1 but aliased at v2+;
/// - explicit `@` references (outside annotations) — alias even
///   scalars at v1, behave differently at v2+;
/// - self-referential inserts (`push(a, [a])`, `a[0] = a`) — v1
///   deep-copies the inserted value, v2+ aliases it into a cycle;
/// - `==`/`!=` between an array literal and `null` — v1's unified
///   container compares to null differently from v2+.
pub(crate) fn flag_aliasing_drift(
    file: &SourceFile,
    source_id: SourceId,
    diagnostics: &mut Vec<Diagnostic>,
) {
    for node in file.syntax().descendants() {
        match node.kind() {
            SyntaxKind::ReturnStmt => {
                let Some(ret) = ReturnStmt::cast(node.clone()) else {
                    continue;
                };
                if !matches!(ret.value(), Some(Expr::Name(_))) {
                    continue;
                }
                if !node
                    .ancestors()
                    .any(|a| matches!(a.kind(), SyntaxKind::FnDecl | SyntaxKind::LambdaExpr))
                {
                    // A top-level return is the script result; the
                    // clone-vs-alias difference is unobservable there.
                    continue;
                }
                diagnostics.push(behavior_change(
                    leek_syntax::node_span(&node, source_id),
                    "a returned container is cloned at v1 but aliased at v2+".to_string(),
                    "callers that mutate the returned value observe different sharing — \
                     review them",
                ));
            }
            SyntaxKind::CallExpr => {
                let Some(call) = CallExpr::cast(node.clone()) else {
                    continue;
                };
                let Some(callee) = call.callee() else {
                    continue;
                };
                let Some(ident) = ident_of_name_ref_expr(&callee) else {
                    continue;
                };
                if !INSERT_BUILTINS.contains(&ident.text()) {
                    continue;
                }
                let args: Vec<Expr> = call
                    .arg_list()
                    .map(|l| l.args().collect())
                    .unwrap_or_default();
                let Some(Expr::Name(first)) = args.first() else {
                    continue;
                };
                let Some(name_tok) = first.ident() else {
                    continue;
                };
                let name = name_tok.text();
                if args.iter().skip(1).any(|a| mentions_name(a.syntax(), name)) {
                    diagnostics.push(behavior_change(
                        leek_syntax::node_span(call.syntax(), source_id),
                        format!(
                            "`{}` inserts a value referencing the container itself: \
                             v1 deep-copies it, v2+ aliases it into a cycle",
                            ident.text()
                        ),
                        "the structures diverge after later mutations — restructure \
                         to avoid the self-reference",
                    ));
                }
            }
            SyntaxKind::BinaryExpr => {
                let Some(bin) = BinaryExpr::cast(node.clone()) else {
                    continue;
                };
                let Some(op) = bin.op() else { continue };
                if matches!(op.kind(), SyntaxKind::EqEq | SyntaxKind::NotEq) {
                    let (lhs, rhs) = (bin.lhs(), bin.rhs());
                    let null_vs_array = |a: &Option<Expr>, b: &Option<Expr>| {
                        matches!(a, Some(Expr::Array(_))) && b.as_ref().is_some_and(is_null_literal)
                    };
                    if null_vs_array(&lhs, &rhs) || null_vs_array(&rhs, &lhs) {
                        diagnostics.push(behavior_change(
                            leek_syntax::node_span(bin.syntax(), source_id),
                            "`==`/`!=` between an array literal and null evaluates \
                             differently at v1 than at v2+"
                                .to_string(),
                            "test emptiness with a count/size builtin instead of \
                             comparing the container to null",
                        ));
                    }
                } else if is_assign_op(op.kind()) {
                    // `a[…] = …a…` — the RHS references the container
                    // being written into.
                    let Some(Expr::Index(ix)) = bin.lhs() else {
                        continue;
                    };
                    let Some(Expr::Name(base)) = ix.base() else {
                        continue;
                    };
                    let Some(base_tok) = base.ident() else {
                        continue;
                    };
                    let name = base_tok.text();
                    if bin.rhs().is_some_and(|r| mentions_name(r.syntax(), name)) {
                        diagnostics.push(behavior_change(
                            leek_syntax::node_span(bin.syntax(), source_id),
                            format!(
                                "assigning a value referencing `{name}` into itself: \
                                 v1 deep-copies it, v2+ aliases it into a cycle"
                            ),
                            "the structures diverge after later mutations — restructure \
                             to avoid the self-reference",
                        ));
                    }
                }
            }
            _ => {}
        }
    }

    // Explicit `@` references (param or expression position). The
    // ones inside annotations (`@since` etc.) are metadata, not refs.
    for el in file.syntax().descendants_with_tokens() {
        let NodeOrToken::Token(tok) = el else {
            continue;
        };
        if tok.kind() != SyntaxKind::At {
            continue;
        }
        if tok
            .parent()
            .is_some_and(|p| p.ancestors().any(|a| a.kind() == SyntaxKind::Annotation))
        {
            continue;
        }
        diagnostics.push(behavior_change(
            token_span(&tok, source_id),
            "`@` reference semantics differ across the v1/v2 boundary".to_string(),
            "a v1 `@` aliases even scalars (`f(@a) { a++ }` mutates the caller's int); \
             v2+ does not — review every use of this reference",
        ));
    }
}

/// Call `f` with the span of every `\<matching-delimiter>` escape
/// inside a string literal, skipping `\\` pairs. At v1 the backslash
/// stays in the string's content; at v2+ it's consumed by the escape.
pub(crate) fn for_each_delim_escape(
    source: &str,
    source_id: SourceId,
    version: Version,
    mut f: impl FnMut(Span),
) {
    let lexed = leek_lexer::lex(source, source_id, version);
    for tok in &lexed.tokens {
        if tok.kind != SyntaxKind::StringLiteral {
            continue;
        }
        let bytes = source[tok.span.range()].as_bytes();
        if bytes.len() < 2 {
            continue;
        }
        let delim = bytes[0];
        // Exclude the closing delimiter (when present) from the scan.
        let body_end = if bytes[bytes.len() - 1] == delim {
            bytes.len() - 1
        } else {
            bytes.len()
        };
        let mut i = 1;
        while i < body_end {
            if bytes[i] == b'\\' && i + 1 < body_end {
                if bytes[i + 1] == delim {
                    // The token came from a u32-spanned source, so the
                    // in-token offset always fits.
                    let at = tok.span.start + u32::try_from(i).unwrap_or(u32::MAX);
                    f(Span::new(source_id, at, at + 1));
                }
                i += 2;
            } else {
                i += 1;
            }
        }
    }
}
