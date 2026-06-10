//! Semantic drift shared by both passes at the v3/v4 boundary.
//!
//! v4 split the unified v1-v3 container into real arrays and maps,
//! made `==` boolean-strict, and changed several builtin contracts
//! along the way. The faithful rewrites (callback parameter order,
//! bool-literal equality strictification) are driven from here so the
//! two adjacent passes stay exact mirrors of each other; everything
//! with no faithful rewrite gets a `MigrationBehaviorChange` warning.
//!
//! The drift list is corpus-derived (see
//! `examples/corpus_verify.rs`):
//!
//! - **Callback parameter order.** Two-parameter callbacks of
//!   `arrayMap`/`arrayFilter`/`arrayPartition` receive `(key, value)`
//!   at v3 but `(value, key)` at v4. Swapping the two parameter NAMES
//!   of an inline closure preserves the body's meaning, and the swap
//!   is its own inverse, so both directions share it.
//! - **Equality juggling.** v3 `==` type-juggles (`1 == true` is
//!   true, `0 == '0'` is true); v4 `==` between operands of
//!   different types is strict. Two literal shapes are detectable:
//!   either operand a bool literal, or BOTH operands literals of
//!   different classes (number/string/bool/null/array/map). v4 → v3
//!   is rewritable (`==` → `===` matches v4's strictness); v3 → v4 is
//!   not, because the juggling outcome depends on runtime values.
//! - **Index assignment.** `a[i] = x` grows the container at v3 but
//!   out-of-range writes on a v4 array are dropped.
//! - **`jsonDecode`** returns a map at v3 but an object at v4.
//! - **`search`** returns null at v3 but -1 at v4 when not found.
//! - **`removeElement`** leaves a hole at v3 but reindexes at v4.
//! - **`count`** of a string is 0 at v3 but its length at v4.

use std::collections::HashSet;

use leek_diagnostics::Diagnostic;
use leek_parser::ast::{AstNode, BinaryExpr, CallExpr, Expr, SourceFile, VarDeclStmt};
use leek_rewrite::EditSet;
use leek_span::SourceId;
use leek_syntax::language::NodeOrToken;
use leek_syntax::{SyntaxKind, SyntaxNode, SyntaxToken};

use super::util::{
    behavior_change, ident_of_name_ref_expr, is_assign_op, mentions_name, token_span,
};

/// Builtins whose two-parameter callback swapped order at the 3/4
/// boundary: `(key, value)` at v3, `(value, key)` at v4.
const CALLBACK_BUILTINS: &[&str] = &["arrayMap", "arrayFilter", "arrayPartition"];

/// Swap the two parameter names of inline two-param callbacks passed
/// to [`CALLBACK_BUILTINS`], so each name stays bound to the same
/// data after the positional order flips. Non-inline callbacks can't
/// be inspected — those are flagged instead.
pub(crate) fn swap_callback_params(
    file: &SourceFile,
    source_id: SourceId,
    edits: &mut EditSet,
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
        if !CALLBACK_BUILTINS.contains(&ident.text()) {
            continue;
        }
        let Some(arg_list) = call.arg_list() else {
            continue;
        };
        let args: Vec<Expr> = arg_list.args().collect();
        let Some(callback) = args.get(1) else {
            continue;
        };
        if let Expr::Lambda(lambda) = callback {
            let params = lambda_param_idents(lambda.syntax());
            if let [a, b] = params.as_slice() {
                let (ta, tb) = (a.text().to_string(), b.text().to_string());
                if ta != tb {
                    let _ = edits.replace_token(a, tb);
                    let _ = edits.replace_token(b, ta);
                }
            }
        } else {
            diagnostics.push(behavior_change(
                leek_syntax::node_span(callback.syntax(), source_id),
                format!(
                    "`{}` passes (key, value) to two-parameter callbacks at v3 \
                     but (value, key) at v4",
                    ident.text()
                ),
                "this callback isn't an inline closure, so its parameters can't \
                 be swapped automatically — check its arity and parameter order \
                 by hand",
            ));
        }
    }
}

/// The `Ident` tokens of a lambda's parameters, in source order.
fn lambda_param_idents(lambda: &SyntaxNode) -> Vec<SyntaxToken> {
    let Some(params) = lambda
        .children()
        .find(|n| n.kind() == SyntaxKind::ParamList)
    else {
        return Vec::new();
    };
    params
        .children()
        .filter(|n| n.kind() == SyntaxKind::Param)
        .filter_map(|p| {
            p.children_with_tokens()
                .filter_map(NodeOrToken::into_token)
                .find(|t| t.kind() == SyntaxKind::Ident)
        })
        .collect()
}

fn is_bool_literal(expr: &Expr) -> bool {
    matches!(
        expr,
        Expr::Literal(lit) if lit
            .token()
            .is_some_and(|t| matches!(t.kind(), SyntaxKind::KwTrue | SyntaxKind::KwFalse))
    )
}

fn is_string_literal(expr: &Expr) -> bool {
    matches!(
        expr,
        Expr::Literal(lit) if lit.token().is_some_and(|t| t.kind() == SyntaxKind::StringLiteral)
    )
}

fn is_real_literal(expr: &Expr) -> bool {
    matches!(
        expr,
        Expr::Literal(lit) if lit.token().is_some_and(|t| t.kind() == SyntaxKind::RealLiteral)
    )
}

/// A literal operand's type class for the juggling detector. Int and
/// real share a class (`1 == 1.0` is true in every version).
#[derive(Clone, Copy, PartialEq, Eq)]
enum LitClass {
    Num,
    Str,
    Bool,
    Null,
    Array,
    Map,
}

fn lit_class(expr: &Expr) -> Option<LitClass> {
    match expr {
        Expr::Array(_) => Some(LitClass::Array),
        Expr::Map(_) => Some(LitClass::Map),
        Expr::Literal(lit) => match lit.token()?.kind() {
            SyntaxKind::IntLiteral | SyntaxKind::RealLiteral => Some(LitClass::Num),
            SyntaxKind::StringLiteral => Some(LitClass::Str),
            SyntaxKind::KwTrue | SyntaxKind::KwFalse => Some(LitClass::Bool),
            SyntaxKind::KwNull => Some(LitClass::Null),
            _ => None,
        },
        _ => None,
    }
}

/// Which literal pattern triggered the juggling detector.
enum JuggleKind {
    /// Either operand is a `true`/`false` literal.
    BoolLiteral,
    /// Both operands are literals of different type classes
    /// (`0 == '0'`, `0 == []`, `'1' == 1`, …).
    MixedLiterals,
}

/// Call `f` with the operator token of every `==`/`!=` whose literal
/// operands guarantee v3 type-juggling vs v4 strictness.
fn for_each_juggling_equality(file: &SourceFile, mut f: impl FnMut(SyntaxToken, JuggleKind)) {
    for node in file.syntax().descendants() {
        if node.kind() != SyntaxKind::BinaryExpr {
            continue;
        }
        let Some(bin) = BinaryExpr::cast(node) else {
            continue;
        };
        let Some(op) = bin.op() else { continue };
        if !matches!(op.kind(), SyntaxKind::EqEq | SyntaxKind::NotEq) {
            continue;
        }
        let (lhs, rhs) = (bin.lhs(), bin.rhs());
        if lhs.as_ref().is_some_and(is_bool_literal) || rhs.as_ref().is_some_and(is_bool_literal) {
            f(op, JuggleKind::BoolLiteral);
        } else if let (Some(a), Some(b)) = (
            lhs.as_ref().and_then(lit_class),
            rhs.as_ref().and_then(lit_class),
        ) && a != b
        {
            f(op, JuggleKind::MixedLiterals);
        }
    }
}

/// v4 → v3: rewrite `x == true` to `x === true` (and `!=` to `!==`),
/// and likewise for mixed-class literal comparisons (`0 == '0'`).
/// v4's `==` between operands of different types is plain false;
/// v3's strict equality does the same, so the rewrite is faithful.
pub(crate) fn strictify_juggling_equality(file: &SourceFile, edits: &mut EditSet) {
    for_each_juggling_equality(file, |op, _| {
        let strict = if op.kind() == SyntaxKind::EqEq {
            "==="
        } else {
            "!=="
        };
        let _ = edits.replace_token(&op, strict.to_string());
    });
}

/// v3 → v4: no faithful rewrite exists — v3's juggling outcome
/// depends on runtime values (`false == 'false'` is true at v3 but
/// `'false' == false` is plain false at v4) — so flag each site.
pub(crate) fn flag_juggling_equality(
    file: &SourceFile,
    source_id: SourceId,
    diagnostics: &mut Vec<Diagnostic>,
) {
    for_each_juggling_equality(file, |op, kind| {
        let message = match kind {
            JuggleKind::BoolLiteral => {
                "`==`/`!=` against a boolean literal type-juggles at v3 \
                 (`1 == true` is true) but is strict at v4 (false unless both \
                 sides are booleans)"
            }
            JuggleKind::MixedLiterals => {
                "`==`/`!=` between literals of different types type-juggles \
                 at v3 (`0 == '0'` is true) but is strict at v4"
            }
        };
        diagnostics.push(behavior_change(
            token_span(&op, source_id),
            message.to_string(),
            "compare the underlying condition directly (e.g. a truthiness \
             test) so both versions agree",
        ));
    });
}

/// Builtins whose return contract changed at the 3/4 boundary.
const CALL_DRIFT: &[(&str, &str, &str)] = &[
    (
        "jsonDecode",
        "returns a map at v3 but an object at v4",
        "downstream key access (`m['k']` vs `o.k`) and iteration order differ — \
         review every use of the decoded value",
    ),
    (
        "search",
        "returns null at v3 but -1 at v4 when nothing matches",
        "null-checks on the result silently change meaning — compare against \
         the version-appropriate sentinel",
    ),
    (
        "removeElement",
        "leaves a hole at v3 but shifts the following elements down at v4",
        "indices past the removed element differ afterwards — review code that \
         indexes the array after the removal",
    ),
    (
        "arrayFilter",
        "keeps the original keys at v3 but reindexes the result from 0 at v4",
        "code that indexes the filtered result (or iterates its keys) sees \
         different keys — review the downstream uses",
    ),
    (
        "arrayPartition",
        "keeps the original keys at v3 but reindexes both halves from 0 at v4",
        "code that indexes the partitions (or iterates their keys) sees \
         different keys — review the downstream uses",
    ),
];

/// Flag the container/builtin contract changes that have no faithful
/// rewrite: [`CALL_DRIFT`] builtins, `count` on a string literal, and
/// index assignments that may rely on v3's grow-on-write arrays.
pub(crate) fn flag_container_drift(
    file: &SourceFile,
    source_id: SourceId,
    diagnostics: &mut Vec<Diagnostic>,
) {
    // Names declared with a map literal (`[:]` / `[k: v]`) — writes
    // through those are map operations and don't drift.
    let map_names: HashSet<String> = file
        .syntax()
        .descendants()
        .filter(|n| n.kind() == SyntaxKind::VarDeclStmt)
        .filter_map(VarDeclStmt::cast)
        .filter(|decl| matches!(decl.init(), Some(Expr::Map(_))))
        .filter_map(|decl| decl.name().map(|t| t.text().to_string()))
        .collect();

    for node in file.syntax().descendants() {
        match node.kind() {
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
                let name = ident.text();
                let span = leek_syntax::node_span(call.syntax(), source_id);
                let args: Vec<Expr> = call
                    .arg_list()
                    .map(|l| l.args().collect())
                    .unwrap_or_default();
                if let Some((_, what, note)) = CALL_DRIFT.iter().find(|(n, _, _)| *n == name) {
                    diagnostics.push(behavior_change(span, format!("`{name}` {what}"), note));
                } else if name == "count" {
                    if args.len() == 1 && is_string_literal(&args[0]) {
                        diagnostics.push(behavior_change(
                            span,
                            "`count` of a string is 0 at v3 but the string's length at v4"
                                .to_string(),
                            "if the length is wanted, compute it with a string builtin that \
                             behaves the same in both versions",
                        ));
                    }
                } else if matches!(name, "removeKey" | "mapRemove") {
                    if args.len() == 2 && is_real_literal(&args[1]) {
                        diagnostics.push(behavior_change(
                            span,
                            format!(
                                "`{name}` with a real-number key matches the integer key \
                                 loosely at v3 but not at v4"
                            ),
                            "v3's unified container compares `12.12` equal to key `12`; \
                             v4 maps do not — pass the exact key",
                        ));
                    }
                } else if name == "string"
                    && args.len() == 1
                    && matches!(args[0], Expr::Array(_) | Expr::Map(_))
                {
                    diagnostics.push(behavior_change(
                        span,
                        "`string` of a container renders nested strings without quotes \
                         at v3 but with quotes at v4"
                            .to_string(),
                        "the rendered text differs whenever the container holds strings — \
                         format the elements explicitly if the output matters",
                    ));
                }
            }
            SyntaxKind::BinaryExpr => {
                let Some(bin) = BinaryExpr::cast(node.clone()) else {
                    continue;
                };
                let Some(op) = bin.op() else { continue };
                if !is_assign_op(op.kind()) {
                    continue;
                }
                let Some(Expr::Index(index_expr)) = bin.lhs() else {
                    continue;
                };
                // A write whose index or RHS references the base
                // container itself drifts regardless of map-ness
                // (`a[a] = 1`, `a[0] = [a]`) — containers used as
                // their own keys/values compare and render
                // differently across the boundary.
                let base_name = match index_expr.base() {
                    Some(Expr::Name(base)) => base.ident().map(|t| t.text().to_string()),
                    _ => None,
                };
                let self_ref = base_name.as_deref().is_some_and(|n| {
                    index_expr
                        .index()
                        .is_some_and(|ix| mentions_name(ix.syntax(), n))
                        || bin.rhs().is_some_and(|r| mentions_name(r.syntax(), n))
                });
                if !self_ref {
                    // String-literal keys are map-style usage; bases
                    // declared with a map literal are maps outright.
                    if index_expr.index().as_ref().is_some_and(is_string_literal) {
                        continue;
                    }
                    if base_name.as_deref().is_some_and(|n| map_names.contains(n)) {
                        continue;
                    }
                }
                diagnostics.push(behavior_change(
                    leek_syntax::node_span(bin.syntax(), source_id),
                    "assigning through an out-of-range index grows the container at v3 \
                     but is dropped by v4 arrays"
                        .to_string(),
                    "if every write targets an existing index (or the base is a map), \
                     this is fine; otherwise build the array with push()",
                ));
            }
            _ => {}
        }
    }
}
