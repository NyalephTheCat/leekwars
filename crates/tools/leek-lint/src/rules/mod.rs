//! Individual lint implementations.
//!
//! Each lint lives in its own module as a unit struct implementing
//! [`crate::LintPass`], plus a `static META` describing it. The
//! driver in [`crate::pass`] walks the HIR once and fires every
//! pass's hooks, so modules here contain *only* the lint logic — no
//! traversal boilerplate.

pub mod approx_constant;
pub mod array_literal_membership;
pub mod assignment_in_condition;
pub mod chained_comparison;
pub mod collapsible_if;
pub mod constant_condition;
pub mod count_in_loop_condition;
pub mod deep_nesting;
pub mod deprecated_feature;
pub mod division_by_zero;
pub mod double_negation;
pub mod duplicate_branches;
pub mod duplicate_case;
pub mod duplicate_condition;
pub mod duplicate_include;
pub mod empty_block;
pub mod identical_operands;
pub mod interval_loop;
pub mod long_function;
pub mod manual_min_max;
pub mod manual_range_check;
pub mod map_as_set;
pub mod needless_index_loop;
pub mod negated_comparison;
pub mod redundant_boolean;
pub mod redundant_ternary;
pub mod self_assignment;
pub mod self_comparison;
pub mod shadowed_binding;
pub mod shadowed_builtin;
pub mod string_concat_in_loop;
pub(crate) mod structural;
pub mod switch_missing_default;
pub mod too_many_arguments;
pub mod unnecessary_else;
pub mod unreachable_code;
pub mod unused_expression;
pub mod unused_parameter;
pub mod unused_variable;
pub mod useless_foreach_write;

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
