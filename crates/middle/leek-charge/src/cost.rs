//! Static per-statement and per-expression cost model.

use leek_hir::{Expr, ExprKind, Stmt};

use crate::opts::ChargeOpts;

pub(crate) fn stmts_cost(stmts: &[Stmt], opts: ChargeOpts) -> u64 {
    stmts.iter().map(|s| stmt_cost(s, opts)).sum()
}

/// Per-statement static cost. We recurse into expressions; nested
/// blocks (`if`, `while`, etc.) are not counted here — they receive
/// their own block-entry charge during the recursive walk.
pub(crate) fn stmt_cost(s: &Stmt, opts: ChargeOpts) -> u64 {
    let own = opts.per_stmt;
    let exprs = match s {
        Stmt::Expr(e) => expr_cost(e, opts),
        Stmt::VarDecl(v) => v.init.as_ref().map_or(0, |e| expr_cost(e, opts)),
        Stmt::Return(e) => e.as_ref().map_or(0, |e| expr_cost(e, opts)),
        Stmt::If(i) => expr_cost(&i.cond, opts),
        Stmt::While(w) => expr_cost(&w.cond, opts),
        Stmt::DoWhile(dw) => expr_cost(&dw.cond, opts),
        Stmt::For(f) => {
            f.init.as_ref().map_or(0, |s| stmt_cost(s, opts))
                + f.cond.as_ref().map_or(0, |e| expr_cost(e, opts))
                + f.step.as_ref().map_or(0, |e| expr_cost(e, opts))
        }
        Stmt::Foreach(fe) => expr_cost(&fe.iter, opts),
        Stmt::Switch(s) => expr_cost(&s.discriminant, opts),
        Stmt::Block(_)
        | Stmt::Break(_)
        | Stmt::Continue(_)
        | Stmt::Include(_)
        | Stmt::Import(_)
        | Stmt::Charge(_) => 0,
    };
    own + exprs
}

/// Per-expression static cost. Dynamic input-scaled cost (for
/// builtins like `replace`) is *not* added here — those self-charge
/// inside the runtime/interpreter.
///
/// Every expression costs `per_expr` plus the cost of its
/// sub-expressions. The only non-uniform case is a ternary, where
/// just one branch runs, so the branches are `max`'d rather than
/// summed; everything else (calls included — a call's cost is its
/// receiver + arguments) is the sum of its immediate children, which
/// [`leek_hir::walk_expr_children`] enumerates. A lambda is a leaf:
/// its body is deferred and not charged here.
pub(crate) fn expr_cost(e: &Expr, opts: ChargeOpts) -> u64 {
    let own = opts.per_expr;
    let children = if let ExprKind::Ternary(c, t, f) = &e.kind {
        // Only one branch runs, so `max` the branches.
        expr_cost(c, opts) + expr_cost(t, opts).max(expr_cost(f, opts))
    } else {
        // Everything else costs the sum of its sub-expressions.
        let mut sum = 0u64;
        leek_hir::walk_expr_children(e, &mut |child| sum += expr_cost(child, opts));
        sum
    };
    own + children
}

pub(crate) fn charge_stmt_for_block_start(_stmts: &[Stmt], total: u64) -> Stmt {
    Stmt::Charge(total)
}
