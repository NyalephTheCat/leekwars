//! Intrinsic recognition: builtin/library functions the optimizer understands
//! well enough to treat as side-effect-free or rewrite via an identity.
//!
//! This is the O2+ companion to the conservative hardcoded constant-folding
//! whitelist in the parent module ([`super::fold_builtin_call`]). It knows a
//! broader set of *pure* builtins — so the purity analysis and dead-code
//! elimination can reason about calls to them — plus a few always-safe identity
//! rewrites on builtin calls.
//!
//! Everything here is intentionally conservative: a wrong "pure" classification
//! would let DCE drop a call with a real side effect, and a wrong identity would
//! miscompile, so only transformations that are exact regardless of language
//! version / dynamic typing are included.

use leek_span::Span;
use leek_types::Type;

use super::is_side_effect_free;
use crate::ir::{Callee, Expr, ExprKind, Literal, NameRef};

/// Whether `name` is a builtin with no observable side effect — safe for the
/// purity analysis to treat as pure and for DCE to drop when its result is
/// unused. Restricted to functions that compute a value from their arguments or
/// read-only state; nothing that mutates entity/game state, prints, or moves
/// the turn forward is included.
#[must_use]
pub(super) fn is_pure(name: &str) -> bool {
    matches!(
        name,
        // numeric / math
        "abs" | "min" | "max" | "floor" | "ceil" | "round" | "sqrt" | "cbrt"
            | "pow" | "exp" | "log" | "cos" | "sin" | "tan" | "acos" | "asin"
            | "atan" | "atan2" | "cosh" | "sinh" | "tanh" | "toDegrees"
            | "toRadians" | "hypot" | "signum"
            // read-only collection / string / conversion queries
            | "count" | "length" | "isEmpty" | "contains" | "indexOf"
            | "substring" | "charAt" | "toUpper" | "toLower" | "number"
            | "string" | "typeOf" | "isInteger" | "isReal"
    )
}

/// Apply an always-safe identity rewrite to a recognized builtin call, returning
/// the replacement expression. `None` when no identity applies.
///
/// Only rewrites that preserve both the value **and** the result type under
/// LeekScript's dynamic typing are included (e.g. nothing that could turn a
/// real-returning call into an integer).
pub(super) fn simplify_call(name: &str, args: &[Expr]) -> Option<Expr> {
    match (name, args) {
        // `count([literal array])` / `count([literal map])` → the element count.
        // Guarded on side-effect-free elements so we don't drop their evaluation.
        ("count", [arg]) => count_of_literal(arg),
        // `abs(abs(x))` → `abs(x)` — abs is idempotent and preserves the type of
        // its argument, so dropping the outer call is exact for any `x`.
        ("abs", [inner]) if is_call_to(inner, "abs") => Some(inner.clone()),
        _ => None,
    }
}

/// `count` of a literal array/map with side-effect-free elements → its length.
fn count_of_literal(arg: &Expr) -> Option<Expr> {
    let len = match &arg.kind {
        ExprKind::Array(xs) if xs.iter().all(is_side_effect_free) => xs.len(),
        ExprKind::Map(pairs)
            if pairs
                .iter()
                .all(|(k, v)| is_side_effect_free(k) && is_side_effect_free(v)) =>
        {
            pairs.len()
        }
        _ => return None,
    };
    Some(int_expr(i64::try_from(len).unwrap_or(i64::MAX), arg.span))
}

fn is_call_to(e: &Expr, name: &str) -> bool {
    matches!(
        &e.kind,
        ExprKind::Call(c)
            if matches!(&c.callee, Callee::Function(NameRef::Builtin(n)) if n == name)
    )
}

fn int_expr(n: i64, span: Span) -> Expr {
    Expr {
        kind: ExprKind::Literal(Literal::Int(n)),
        ty: Type::Integer,
        span,
    }
}
