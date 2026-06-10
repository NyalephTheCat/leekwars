//! Structural traversal of the HIR, bound to the generic
//! [`leek_visit::tree`] framework.
//!
//! Two layers live here:
//!
//! 1. **Shallow child enumerators** (`walk_expr_children`,
//!    `walk_stmt_child_exprs`, `walk_stmt_child_stmts`, and their `_mut`
//!    mirrors). Each visits a node's *immediate* children exactly once
//!    and does not recurse — the variant-complete `match` over every
//!    [`ExprKind`] / [`Stmt`] lives here, in one place. Several crates
//!    previously hand-rolled this descent and drifted apart (one
//!    silently dropped `Ternary`, `Map`, `Set`, `Object`, `Slice`,
//!    `Interval`, `Cast`, and `New`). [`ExprKind::Lambda`] is a **leaf**
//!    in these primitives: a lambda body has its own scope, so consumers
//!    that descend into it must opt in explicitly.
//!
//! 2. **Framework wiring**: the [`Hir`] node types are bound to
//!    `leek_visit::tree` via [`Visitable`] / [`VisitableMut`] impls and
//!    the [`HirVisitor`] / [`HirVisitorMut`] umbrella traits, so a
//!    visitor reacts to only the node kinds it cares about and controls
//!    descent with [`Flow`]. Unlike the shallow primitives, the
//!    `Visitable` recursion descends into the **whole** lambda
//!    (parameter defaults + body); prune at the lambda boundary by
//!    returning [`Flow::Skip`] from `visit`.

use crate::ir::{Block, Callee, Expr, ExprKind, LambdaBody, Stmt};

/// Invoke `f` on each immediate sub-expression of `e`.
///
/// Does **not** call `f` on `e` itself and does **not** recurse.
/// [`ExprKind::Lambda`] is a leaf (see the module docs); callers
/// that need lambda internals match on it explicitly.
pub fn walk_expr_children(e: &Expr, f: &mut impl FnMut(&Expr)) {
    match &e.kind {
        ExprKind::Literal(_) | ExprKind::Name(_) => {}
        ExprKind::Binary(_, l, r) => {
            f(l);
            f(r);
        }
        ExprKind::Unary(_, x) | ExprKind::Postfix(_, x) | ExprKind::Cast(x, _) => f(x),
        ExprKind::Call(c) => {
            match &c.callee {
                Callee::Function(_) => {}
                Callee::Method { receiver, .. } => f(receiver),
                Callee::Expr(callee) => f(callee),
            }
            for arg in &c.args {
                f(arg);
            }
        }
        ExprKind::Field(base, _) => f(base),
        ExprKind::Index(base, idx) => {
            f(base);
            f(idx);
        }
        ExprKind::Slice(s) => {
            f(&s.base);
            for x in [&s.start, &s.end, &s.step].into_iter().flatten() {
                f(x);
            }
        }
        ExprKind::Array(items) | ExprKind::Set(items) => {
            for it in items {
                f(it);
            }
        }
        ExprKind::Map(pairs) => {
            for (k, v) in pairs {
                f(k);
                f(v);
            }
        }
        ExprKind::Object(fields) => {
            for (_, v) in fields {
                f(v);
            }
        }
        ExprKind::Ternary(c, t, e) => {
            f(c);
            f(t);
            f(e);
        }
        ExprKind::Interval(i) => {
            for x in [&i.start, &i.end, &i.step].into_iter().flatten() {
                f(x);
            }
        }
        ExprKind::New(n) => {
            for arg in &n.args {
                f(arg);
            }
        }
        // Lambda is a leaf — see module docs.
        ExprKind::Lambda(_) => {}
    }
}

/// Invoke `f` on each expression that appears *directly* in `s` —
/// i.e. expressions belonging to `s` itself, not to any nested
/// statement (a loop body, an `if` branch, …). Pair with
/// [`walk_stmt_child_stmts`] to recurse over the whole tree.
pub fn walk_stmt_child_exprs(s: &Stmt, f: &mut impl FnMut(&Expr)) {
    match s {
        Stmt::Expr(e) => f(e),
        Stmt::VarDecl(v) => {
            if let Some(init) = &v.init {
                f(init);
            }
        }
        Stmt::Return(opt) => {
            if let Some(e) = opt {
                f(e);
            }
        }
        Stmt::If(i) => f(&i.cond),
        Stmt::While(w) => f(&w.cond),
        Stmt::DoWhile(d) => f(&d.cond),
        Stmt::For(fr) => {
            if let Some(c) = &fr.cond {
                f(c);
            }
            if let Some(st) = &fr.step {
                f(st);
            }
        }
        Stmt::Foreach(fe) => f(&fe.iter),
        Stmt::Switch(sw) => {
            f(&sw.discriminant);
            for arm in &sw.arms {
                if let Some(case) = &arm.case {
                    f(case);
                }
            }
        }
        Stmt::Block(_)
        | Stmt::Break(_)
        | Stmt::Continue(_)
        | Stmt::Include(_)
        | Stmt::Import(_)
        | Stmt::Charge(_) => {}
    }
}

/// Invoke `f` on each statement nested *directly* inside `s` (loop
/// bodies, `if`/`else` branches, block contents, switch-arm bodies).
/// Does not recurse.
pub fn walk_stmt_child_stmts(s: &Stmt, f: &mut impl FnMut(&Stmt)) {
    match s {
        Stmt::If(i) => {
            f(&i.then_branch);
            if let Some(e) = &i.else_branch {
                f(e);
            }
        }
        Stmt::While(w) => f(&w.body),
        Stmt::DoWhile(d) => f(&d.body),
        Stmt::For(fr) => {
            if let Some(init) = &fr.init {
                f(init);
            }
            f(&fr.body);
        }
        Stmt::Foreach(fe) => f(&fe.body),
        Stmt::Block(b) => {
            for st in &b.stmts {
                f(st);
            }
        }
        Stmt::Switch(sw) => {
            for arm in &sw.arms {
                for st in &arm.body {
                    f(st);
                }
            }
        }
        Stmt::Expr(_)
        | Stmt::VarDecl(_)
        | Stmt::Return(_)
        | Stmt::Break(_)
        | Stmt::Continue(_)
        | Stmt::Include(_)
        | Stmt::Import(_)
        | Stmt::Charge(_) => {}
    }
}

// ---------------------------------------------------------------------------
// Mutable primitives
//
// Exact `&mut` mirrors of the read-only `walk_*_children` helpers above. Same
// shallow contract, same lambda-as-leaf rule. They back the `VisitableMut`
// recursion.
// ---------------------------------------------------------------------------

/// Invoke `f` on each immediate sub-expression of `e`, mutably.
/// Mirrors [`walk_expr_children`]; [`ExprKind::Lambda`] is a leaf.
pub fn walk_expr_children_mut(e: &mut Expr, f: &mut impl FnMut(&mut Expr)) {
    match &mut e.kind {
        ExprKind::Literal(_) | ExprKind::Name(_) => {}
        ExprKind::Binary(_, l, r) => {
            f(l);
            f(r);
        }
        ExprKind::Unary(_, x) | ExprKind::Postfix(_, x) | ExprKind::Cast(x, _) => f(x),
        ExprKind::Call(c) => {
            match &mut c.callee {
                Callee::Function(_) => {}
                Callee::Method { receiver, .. } => f(receiver),
                Callee::Expr(callee) => f(callee),
            }
            for arg in &mut c.args {
                f(arg);
            }
        }
        ExprKind::Field(base, _) => f(base),
        ExprKind::Index(base, idx) => {
            f(base);
            f(idx);
        }
        ExprKind::Slice(s) => {
            f(&mut s.base);
            for x in [&mut s.start, &mut s.end, &mut s.step]
                .into_iter()
                .flatten()
            {
                f(x);
            }
        }
        ExprKind::Array(items) | ExprKind::Set(items) => {
            for it in items {
                f(it);
            }
        }
        ExprKind::Map(pairs) => {
            for (k, v) in pairs {
                f(k);
                f(v);
            }
        }
        ExprKind::Object(fields) => {
            for (_, v) in fields {
                f(v);
            }
        }
        ExprKind::Ternary(c, t, e) => {
            f(c);
            f(t);
            f(e);
        }
        ExprKind::Interval(i) => {
            for x in [&mut i.start, &mut i.end, &mut i.step]
                .into_iter()
                .flatten()
            {
                f(x);
            }
        }
        ExprKind::New(n) => {
            for arg in &mut n.args {
                f(arg);
            }
        }
        ExprKind::Lambda(_) => {}
    }
}

/// Mutable mirror of [`walk_stmt_child_exprs`].
pub fn walk_stmt_child_exprs_mut(s: &mut Stmt, f: &mut impl FnMut(&mut Expr)) {
    match s {
        Stmt::Expr(e) => f(e),
        Stmt::VarDecl(v) => {
            if let Some(init) = &mut v.init {
                f(init);
            }
        }
        Stmt::Return(opt) => {
            if let Some(e) = opt {
                f(e);
            }
        }
        Stmt::If(i) => f(&mut i.cond),
        Stmt::While(w) => f(&mut w.cond),
        Stmt::DoWhile(d) => f(&mut d.cond),
        Stmt::For(fr) => {
            if let Some(c) = &mut fr.cond {
                f(c);
            }
            if let Some(st) = &mut fr.step {
                f(st);
            }
        }
        Stmt::Foreach(fe) => f(&mut fe.iter),
        Stmt::Switch(sw) => {
            f(&mut sw.discriminant);
            for arm in &mut sw.arms {
                if let Some(case) = &mut arm.case {
                    f(case);
                }
            }
        }
        Stmt::Block(_)
        | Stmt::Break(_)
        | Stmt::Continue(_)
        | Stmt::Include(_)
        | Stmt::Import(_)
        | Stmt::Charge(_) => {}
    }
}

/// Mutable mirror of [`walk_stmt_child_stmts`].
pub fn walk_stmt_child_stmts_mut(s: &mut Stmt, f: &mut impl FnMut(&mut Stmt)) {
    match s {
        Stmt::If(i) => {
            f(&mut i.then_branch);
            if let Some(e) = &mut i.else_branch {
                f(e);
            }
        }
        Stmt::While(w) => f(&mut w.body),
        Stmt::DoWhile(d) => f(&mut d.body),
        Stmt::For(fr) => {
            if let Some(init) = &mut fr.init {
                f(init);
            }
            f(&mut fr.body);
        }
        Stmt::Foreach(fe) => f(&mut fe.body),
        Stmt::Block(b) => {
            for st in &mut b.stmts {
                f(st);
            }
        }
        Stmt::Switch(sw) => {
            for arm in &mut sw.arms {
                for st in &mut arm.body {
                    f(st);
                }
            }
        }
        Stmt::Expr(_)
        | Stmt::VarDecl(_)
        | Stmt::Return(_)
        | Stmt::Break(_)
        | Stmt::Continue(_)
        | Stmt::Include(_)
        | Stmt::Import(_)
        | Stmt::Charge(_) => {}
    }
}

// ---------------------------------------------------------------------------
// Framework wiring
// ---------------------------------------------------------------------------

use std::ops::ControlFlow;

pub use leek_visit::tree::{Flow, Visit, VisitMut, Visitable, VisitableMut};
use leek_visit::{descend, enter, enter_mut, umbrella, umbrella_mut};

umbrella!(HirVisitor: Block, Stmt, Expr);
umbrella_mut!(HirVisitorMut: Block, Stmt, Expr);

impl<V: HirVisitor> Visitable<V> for Block {
    fn walk(&self, v: &mut V) -> ControlFlow<()> {
        enter!(v, self);
        for s in &self.stmts {
            descend!(s.walk(v));
        }
        ControlFlow::Continue(())
    }
}

impl<V: HirVisitor> Visitable<V> for Stmt {
    fn walk(&self, v: &mut V) -> ControlFlow<()> {
        enter!(v, self);
        // A block statement routes through `Block` so `Visit<Block>` fires.
        if let Stmt::Block(b) = self {
            return b.walk(v);
        }
        let mut flow = ControlFlow::Continue(());
        walk_stmt_child_exprs(self, &mut |e| {
            if flow.is_continue() {
                flow = e.walk(v);
            }
        });
        descend!(flow);
        walk_stmt_child_stmts(self, &mut |s| {
            if flow.is_continue() {
                flow = s.walk(v);
            }
        });
        flow
    }
}

impl<V: HirVisitor> Visitable<V> for Expr {
    fn walk(&self, v: &mut V) -> ControlFlow<()> {
        enter!(v, self);
        // Descend into the whole lambda (parameter defaults + body), unlike
        // the shallow `walk_expr_children` leaf rule. Prune by returning
        // `Flow::Skip` from `visit`.
        if let ExprKind::Lambda(lam) = &self.kind {
            for p in &lam.params {
                if let Some(d) = &p.default {
                    descend!(d.walk(v));
                }
            }
            return match &lam.body {
                LambdaBody::Block(b) => b.walk(v),
                LambdaBody::Expr(e) => e.walk(v),
            };
        }
        let mut flow = ControlFlow::Continue(());
        walk_expr_children(self, &mut |c| {
            if flow.is_continue() {
                flow = c.walk(v);
            }
        });
        flow
    }
}

// Mutable mirrors.

impl<V: HirVisitorMut> VisitableMut<V> for Block {
    fn walk_mut(&mut self, v: &mut V) -> ControlFlow<()> {
        enter_mut!(v, self);
        for s in &mut self.stmts {
            descend!(s.walk_mut(v));
        }
        ControlFlow::Continue(())
    }
}

impl<V: HirVisitorMut> VisitableMut<V> for Stmt {
    fn walk_mut(&mut self, v: &mut V) -> ControlFlow<()> {
        enter_mut!(v, self);
        if let Stmt::Block(b) = self {
            return b.walk_mut(v);
        }
        let mut flow = ControlFlow::Continue(());
        walk_stmt_child_exprs_mut(self, &mut |e| {
            if flow.is_continue() {
                flow = e.walk_mut(v);
            }
        });
        descend!(flow);
        walk_stmt_child_stmts_mut(self, &mut |s| {
            if flow.is_continue() {
                flow = s.walk_mut(v);
            }
        });
        flow
    }
}

impl<V: HirVisitorMut> VisitableMut<V> for Expr {
    fn walk_mut(&mut self, v: &mut V) -> ControlFlow<()> {
        enter_mut!(v, self);
        if let ExprKind::Lambda(lam) = &mut self.kind {
            for p in &mut lam.params {
                if let Some(d) = &mut p.default {
                    descend!(d.walk_mut(v));
                }
            }
            return match &mut lam.body {
                LambdaBody::Block(b) => b.walk_mut(v),
                LambdaBody::Expr(e) => e.walk_mut(v),
            };
        }
        let mut flow = ControlFlow::Continue(());
        walk_expr_children_mut(self, &mut |c| {
            if flow.is_continue() {
                flow = c.walk_mut(v);
            }
        });
        flow
    }
}

// ---------------------------------------------------------------------------
// Closure adapters
//
// Each reacts to one HIR node kind through a closure and no-ops the other
// two (by the `Visit` default), so it satisfies the [`HirVisitor`]
// umbrella. This is the ergonomic entry point for one-off walks:
// `block.walk(&mut OnExpr(|e| { …; Flow::Walk }))`.
// ---------------------------------------------------------------------------

/// React to expressions only.
pub struct OnExpr<F>(pub F);
impl<F: FnMut(&Expr) -> Flow> Visit<Expr> for OnExpr<F> {
    fn visit(&mut self, e: &Expr) -> Flow {
        (self.0)(e)
    }
}
impl<F> Visit<Block> for OnExpr<F> {}
impl<F> Visit<Stmt> for OnExpr<F> {}

/// React to statements only.
pub struct OnStmt<F>(pub F);
impl<F: FnMut(&Stmt) -> Flow> Visit<Stmt> for OnStmt<F> {
    fn visit(&mut self, s: &Stmt) -> Flow {
        (self.0)(s)
    }
}
impl<F> Visit<Block> for OnStmt<F> {}
impl<F> Visit<Expr> for OnStmt<F> {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lower_file;
    use leek_parser::ast::{AstNode, SourceFile};
    use leek_span::SourceId;
    use leek_syntax::{SyntaxNode, Version};

    fn lower(src: &str) -> crate::HirFile {
        let source = SourceId::new(1).unwrap();
        let parsed = leek_parser::parse(src, source, Version::V4);
        let file = SourceFile::cast(SyntaxNode::new_root(parsed.green)).expect("parses");
        lower_file(&file, source).0
    }

    /// Count every expression reachable from a function body by
    /// recursing through the shallow walkers. Exercises the
    /// previously-dropped variants (ternary, map, cast, …).
    fn count_all_exprs(stmts: &[Stmt]) -> usize {
        fn expr(e: &Expr, n: &mut usize) {
            *n += 1;
            walk_expr_children(e, &mut |c| expr(c, n));
        }
        fn stmt(s: &Stmt, n: &mut usize) {
            walk_stmt_child_exprs(s, &mut |e| expr(e, n));
            walk_stmt_child_stmts(s, &mut |c| stmt(c, n));
        }
        let mut n = 0;
        for s in stmts {
            stmt(s, &mut n);
        }
        n
    }

    #[test]
    fn descends_into_previously_dropped_variants() {
        let hir = lower("var x = true ? [1: 2] : (3 as integer)\n");
        let total = count_all_exprs(&hir.main);
        // ternary, cond `true`, map, key `1`, value `2`, cast, inner `3` = 7.
        assert_eq!(total, 7, "walker must reach map/cast subexpressions");
    }

    #[test]
    fn shallow_lambda_is_a_leaf() {
        let hir = lower("var f = x -> x + 1\n");
        assert_eq!(count_all_exprs(&hir.main), 1, "lambda body not visited");
    }

    /// A `Visit`-based visitor that tallies blocks/stmts/exprs, exercising
    /// the umbrella cross-recursion through `Visitable::walk`.
    #[derive(Default)]
    struct Counter {
        stmts: usize,
        exprs: usize,
        blocks: usize,
    }
    impl Visit<Block> for Counter {
        fn visit(&mut self, _b: &Block) -> Flow {
            self.blocks += 1;
            Flow::Walk
        }
    }
    impl Visit<Stmt> for Counter {
        fn visit(&mut self, _s: &Stmt) -> Flow {
            self.stmts += 1;
            Flow::Walk
        }
    }
    impl Visit<Expr> for Counter {
        fn visit(&mut self, _e: &Expr) -> Flow {
            self.exprs += 1;
            Flow::Walk
        }
    }

    #[test]
    fn umbrella_recurses_across_node_kinds() {
        let hir = lower("if (a) { var b = c } else { d() }\n");
        let mut c = Counter::default();
        for s in &hir.main {
            let _ = s.walk(&mut c);
        }
        // the `if`, its two branch blocks, the `var b` decl, the `d()`
        // expr-stmt = 5.
        assert_eq!(c.stmts, 5, "stmts");
        assert_eq!(c.blocks, 2, "blocks");
        // cond `a`, init `c`, call `d()` = 3.
        assert_eq!(c.exprs, 3, "exprs");
    }

    #[test]
    fn visit_descends_into_lambda_body_and_defaults() {
        let hir = lower("var f = x -> x + 1\n");
        let mut c = Counter::default();
        for s in &hir.main {
            let _ = s.walk(&mut c);
        }
        // var decl stmt (1); exprs: lambda, body `x + 1`, `x`, `1` = 4.
        assert_eq!(c.stmts, 1, "stmts");
        assert_eq!(c.exprs, 4, "lambda body must be visited via the umbrella");
    }

    #[test]
    fn skip_prunes_lambda_body() {
        // A visitor that returns Skip at a lambda sees the lambda expr but
        // not its body.
        let hir = lower("var f = x -> x + 1\n");
        let mut seen = 0usize;
        for s in &hir.main {
            let _ = s.walk(&mut OnExpr(|e: &Expr| {
                seen += 1;
                if matches!(e.kind, ExprKind::Lambda(_)) {
                    Flow::Skip
                } else {
                    Flow::Walk
                }
            }));
        }
        assert_eq!(seen, 1, "only the lambda expr itself, body pruned");
    }

    /// A `VisitMut` that rewrites every integer literal to `0`.
    struct Zeroer {
        count: usize,
    }
    impl VisitMut<Expr> for Zeroer {
        fn visit_mut(&mut self, e: &mut Expr) -> Flow {
            if let ExprKind::Literal(crate::ir::Literal::Int(n)) = &mut e.kind
                && *n != 0
            {
                *n = 0;
                self.count += 1;
            }
            Flow::Walk
        }
    }
    impl VisitMut<Block> for Zeroer {}
    impl VisitMut<Stmt> for Zeroer {}

    #[test]
    fn visit_mut_rewrites_in_place() {
        let mut hir = lower("var x = 1 + (2 * 3)\n");
        let mut z = Zeroer { count: 0 };
        for s in &mut hir.main {
            let _ = s.walk_mut(&mut z);
        }
        assert_eq!(z.count, 3, "1, 2, 3 all rewritten");
        let mut remaining = 0usize;
        fn check(e: &Expr, r: &mut usize) {
            if let ExprKind::Literal(crate::ir::Literal::Int(n)) = &e.kind
                && *n != 0
            {
                *r += 1;
            }
            walk_expr_children(e, &mut |c| check(c, r));
        }
        for s in &hir.main {
            walk_stmt_child_exprs(s, &mut |e| check(e, &mut remaining));
        }
        assert_eq!(remaining, 0, "all literals zeroed in place");
    }
}
