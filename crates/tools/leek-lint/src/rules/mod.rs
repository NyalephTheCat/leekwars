//! Individual lint implementations.
//!
//! Each lint lives in its own module as a unit struct implementing
//! [`crate::LintPass`], plus a `declare_lint!` call describing it. The driver
//! in [`crate::pass`] walks the HIR once and fires every pass's hooks, so
//! modules here contain *only* the lint logic — no traversal boilerplate.
//!
//! The `lint_rules!` list below is the **single source of truth** for which
//! lints exist: it generates both the `pub mod` declarations and [`REGISTRY`],
//! which [`crate::all_passes`] and [`crate::allow`]'s name lookup are derived
//! from. Adding a lint is one line here plus the `declare_lint!` in the
//! module; `tests/registry.rs` fails if the catalog and this list disagree.

/// Declare the rule modules once and derive [`REGISTRY`] from the same list.
macro_rules! lint_rules {
    ($($m:ident),+ $(,)?) => {
        $(pub mod $m;)+

        /// Every lint this crate knows about, in module order. Output order
        /// does not depend on it — [`crate::lint_with`] sorts by code and
        /// span — so keep the list alphabetical.
        pub(crate) const REGISTRY: &[crate::registry::LintRegistration] =
            &[$($m::REGISTRATION),+];
    };
}

lint_rules! {
    approx_constant,
    array_literal_membership,
    assignment_in_condition,
    chained_comparison,
    collapsible_if,
    constant_condition,
    count_in_loop_condition,
    deep_nesting,
    deprecated_feature,
    division_by_zero,
    double_negation,
    duplicate_branches,
    duplicate_case,
    duplicate_condition,
    duplicate_include,
    empty_block,
    identical_operands,
    interval_loop,
    long_function,
    manual_min_max,
    manual_range_check,
    map_as_set,
    needless_index_loop,
    negated_comparison,
    redundant_boolean,
    redundant_ternary,
    self_assignment,
    self_comparison,
    shadowed_binding,
    shadowed_builtin,
    string_concat_in_loop,
    switch_missing_default,
    too_many_arguments,
    unnecessary_else,
    unreachable_code,
    unused_expression,
    unused_parameter,
    unused_variable,
    useless_foreach_write,
}

/// Shared structural comparison helpers — not a lint, so outside
/// `lint_rules!`.
pub(crate) mod structural;
/// Shared one-shape predicates — not a lint; see [`structural`].
pub(crate) mod util;

// ---- Recursive walk helpers ----
//
// Thin recursive closures over `leek-hir`'s canonical shallow walkers,
// for passes that need their own sub-walk (collecting references in a
// body, scanning a condition). They borrow statement slices directly —
// no synthesized `Block` wrappers, no cloning.

use leek_hir::{Expr, ExprKind, LambdaBody, Stmt};

/// Visit every statement in `stmts` and, recursively, every statement
/// nested inside them. Source order. Statements only nest inside
/// statements, so lambda bodies (expressions) are never entered.
pub(crate) fn for_each_stmt(stmts: &[Stmt], f: &mut impl FnMut(&Stmt)) {
    fn visit(s: &Stmt, f: &mut impl FnMut(&Stmt)) {
        f(s);
        leek_hir::walk_stmt_child_stmts(s, &mut |c| visit(c, f));
    }
    for s in stmts {
        visit(s, f);
    }
}

/// Visit `e` and every sub-expression. Lambdas are leaves — their
/// bodies are separate scopes; use [`for_each_expr_deep`] to enter
/// them.
pub(crate) fn for_each_expr(e: &Expr, f: &mut impl FnMut(&Expr)) {
    f(e);
    leek_hir::walk_expr_children(e, &mut |c| for_each_expr(c, f));
}

/// Like [`for_each_expr`], but descends through lambda parameter
/// defaults and bodies. For lints where a reference inside a nested
/// lambda still counts (e.g. "is this variable used?").
pub(crate) fn for_each_expr_deep(e: &Expr, f: &mut impl FnMut(&Expr)) {
    f(e);
    if let ExprKind::Lambda(lam) = &e.kind {
        for p in &lam.params {
            if let Some(d) = &p.default {
                for_each_expr_deep(d, f);
            }
        }
        match &lam.body {
            LambdaBody::Block(b) => for_each_expr_deep_in_stmts(&b.stmts, f),
            LambdaBody::Expr(x) => for_each_expr_deep(x, f),
        }
        return;
    }
    leek_hir::walk_expr_children(e, &mut |c| for_each_expr_deep(c, f));
}

/// [`for_each_expr_in_stmts`], descending into lambdas.
pub(crate) fn for_each_expr_deep_in_stmts(stmts: &[Stmt], f: &mut impl FnMut(&Expr)) {
    for_each_stmt(stmts, &mut |s| {
        leek_hir::walk_stmt_child_exprs(s, &mut |e| for_each_expr_deep(e, f));
    });
}
