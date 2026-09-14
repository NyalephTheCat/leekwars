//! Structural traversal of the HIR, bound to the generic
//! [`leek_visit::tree`] framework.
//!
//! Four layers live here:
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
//! 2. **Lambda-crossing deep walkers** ([`walk_stmts_deep`],
//!    [`walk_lambda_stmts_in_expr`]). Same structure, opposite lambda
//!    rule: they report a lambda body's statements as part of the
//!    enclosing sequence. Only for analyses that hunt for bindings at
//!    any lambda depth; pick between the two layers deliberately.
//!
//! 3. **File-level enumerators** ([`walk_file_bodies`] and the
//!    statement / expression walkers over it). One variant-complete
//!    answer to *where does executable code live in a file* — main
//!    block, global and field initialisers, functions, constructors,
//!    methods, parameter defaults — handed out in a fixed order. Three
//!    walkers in the Java backend each grew their own partial answer;
//!    this is the shared one (#253).
//!
//! 4. **Framework wiring**: the [`Block`] / [`Stmt`] / [`Expr`] node types
//!    are bound to `leek_visit::tree` via [`Visitable`] / [`VisitableMut`] impls and
//!    the [`HirVisitor`] / [`HirVisitorMut`] umbrella traits, so a
//!    visitor reacts to only the node kinds it cares about and controls
//!    descent with [`Flow`]. Unlike the shallow primitives, the
//!    `Visitable` recursion descends into the **whole** lambda
//!    (parameter defaults + body); prune at the lambda boundary by
//!    returning [`Flow::Skip`] from `visit`.

use crate::ir::{
    Block, Callee, Class, Def, Expr, ExprKind, Field, Function, Global, HirFile, LambdaBody,
    MethodDef, Param, Stmt,
};

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
        ExprKind::Field(base, ..) => f(base),
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
        ExprKind::Array(items) => {
            for it in items {
                f(it);
            }
        }
        ExprKind::Set(items) => {
            for it in items {
                f(&it.start);
                if let Some(end) = &it.end {
                    f(end);
                }
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
// Lambda-crossing deep walkers
//
// The shallow primitives above stop at a lambda: `walk_expr_children` reports
// no children for [`ExprKind::Lambda`] and `walk_stmt_child_stmts` never
// reaches a lambda body, because a lambda has its own scope and most
// consumers must not confuse it with the enclosing one. A handful of analyses
// need the opposite contract — they look for *bindings* (`var aux =
// function…`, `@`-aliased locals) that can be declared at any lambda depth.
// Those two crates each hand-rolled the same pair of walkers; the pair lives
// here now.
//
// Pick deliberately: a consumer that wants scope-respecting descent and
// reaches for a `_deep` walker gets a lambda body's statements attributed to
// the enclosing scope, and one that wants the whole subtree and reaches for
// the shallow pair silently skips every lambda body. Getting that choice
// wrong is how the drift these primitives exist to prevent happened in the
// first place.
// ---------------------------------------------------------------------------

/// Invoke `f` on `s` and on **every** statement in its subtree, including
/// statements inside lambda bodies.
///
/// Unlike [`walk_stmt_child_stmts`], which is shallow and treats
/// [`ExprKind::Lambda`] as a leaf, this crosses lambda boundaries: the
/// callback sees a lambda body's statements as if they were part of the
/// enclosing sequence, with no scope bookkeeping. Use it only for analyses
/// that genuinely want that — finding every `var x = function…` binding in a
/// file, say — and use the shallow pair everywhere else.
pub fn walk_stmts_deep(s: &Stmt, f: &mut dyn FnMut(&Stmt)) {
    f(s);
    walk_stmt_child_stmts(s, &mut |c| walk_stmts_deep(c, f));
    walk_stmt_child_exprs(s, &mut |e| walk_lambda_stmts_in_expr(e, f));
}

/// Invoke `f` on every statement of every lambda body reachable from `e`, at
/// any nesting depth. The counterpart of [`walk_stmts_deep`] for the
/// expression side; `e` itself is never reported (it is an expression).
pub fn walk_lambda_stmts_in_expr(e: &Expr, f: &mut dyn FnMut(&Stmt)) {
    if let ExprKind::Lambda(l) = &e.kind {
        match &l.body {
            LambdaBody::Block(b) => {
                for s in &b.stmts {
                    walk_stmts_deep(s, f);
                }
            }
            LambdaBody::Expr(inner) => walk_lambda_stmts_in_expr(inner, f),
        }
    }
    walk_expr_children(e, &mut |c| walk_lambda_stmts_in_expr(c, f));
}

// ---------------------------------------------------------------------------
// File-level body enumerators
//
// Everything above walks *one* tree the caller already holds. These answer
// the prior question — where does executable code live in a file at all? —
// and that is why they live here rather than in a consumer: three walkers in
// the Java backend each answered it separately and each answered it
// differently (one reached 2 kinds of root, one 3, one 4), so a builtin
// reassigned inside a class method, a field initialiser or a global
// initialiser was invisible to some of them and not the others (#253).
//
// [`walk_file_bodies`] is the variant-complete answer; the three walkers
// under it are its statement- and expression-shaped conveniences, and they
// differ only in the lambda contract each carries over from the layers above.
// ---------------------------------------------------------------------------

/// One place executable code lives in a file.
///
/// Each root carries the *item* it belongs to, not only its body, so a
/// consumer can key what it finds by class or by definition.
///
/// A [`Self::Function`], [`Self::Method`] or [`Self::Constructor`] always has
/// a body, and a [`Self::FieldInit`] or [`Self::GlobalInit`] always has an
/// initialiser: [`walk_file_bodies`] does not hand out a root with no code in
/// it (the bodiless signatures of signature-file mode, or a `global` or field
/// declared without a value).
#[derive(Debug, Clone, Copy)]
pub enum BodyRoot<'a> {
    /// The file's top-level statements ([`HirFile::main`]).
    Main(&'a [Stmt]),
    /// A top-level `function`.
    Function(&'a Function),
    /// A method, with the class declaring it.
    Method(&'a Class, &'a MethodDef),
    /// A constructor, with the class declaring it.
    Constructor(&'a Class, &'a MethodDef),
    /// A field's initialiser, with the class declaring the field. Static and
    /// instance fields alike — [`Field::is_static`] tells them apart.
    FieldInit(&'a Class, &'a Field),
    /// A `global`'s initialiser.
    ///
    /// Lowering a file-scope `global x = e` puts the initialiser in the main
    /// block, as a `VarDecl` with `is_global` set, and leaves
    /// [`Global::init`] empty — so this root fires only for HIR that filled
    /// that slot some other way. A consumer after *every* global initialiser
    /// still has to walk [`Self::Main`].
    GlobalInit(&'a Global),
}

/// Invoke `f` on every executable root of `hir`, in this fixed order:
///
/// 1. the main block (always, even when empty);
/// 2. each `global` that has an initialiser, in [`HirFile::defs`] order;
/// 3. each top-level function that has a body, in `defs` order;
/// 4. per class in `defs` order: its field initialisers, then its
///    constructors, then its methods — each in declaration order.
///
/// The order is part of the contract. A set or `Vec` built from a walk is
/// then reproducible run to run, and a first-wins or last-wins analysis
/// cannot silently depend on which root it happened to reach first.
///
/// This enumerator is purely structural: it hands out roots and never looks
/// inside one, so it has no lambda contract of its own. Choosing one is what
/// [`walk_file_stmts`] (stops at a lambda) and [`walk_file_stmts_deep`] /
/// [`walk_file_exprs`] (cross into one) are for.
pub fn walk_file_bodies(hir: &HirFile, f: &mut impl FnMut(BodyRoot<'_>)) {
    f(BodyRoot::Main(&hir.main));
    for def in &hir.defs {
        if let Def::Global(g) = def
            && g.init.is_some()
        {
            f(BodyRoot::GlobalInit(g));
        }
    }
    for def in &hir.defs {
        if let Def::Function(fun) = def
            && fun.body.is_some()
        {
            f(BodyRoot::Function(fun));
        }
    }
    for def in &hir.defs {
        let Def::Class(class) = def else { continue };
        for field in class.fields.iter().filter(|f| f.init.is_some()) {
            f(BodyRoot::FieldInit(class, field));
        }
        for ctor in class.constructors.iter().filter(|m| m.body.is_some()) {
            f(BodyRoot::Constructor(class, ctor));
        }
        for method in class.methods.iter().filter(|m| m.body.is_some()) {
            f(BodyRoot::Method(class, method));
        }
    }
}

/// A piece of code a [`BodyRoot`] owns directly.
enum RootPart<'a> {
    /// A top-level statement of the root's body.
    Stmt(&'a Stmt),
    /// An expression the root owns with no statement around it: a parameter
    /// default, or a global / field initialiser.
    Expr(&'a Expr),
}

/// Invoke `f` on each part of `root`: for a callable root, its parameter
/// defaults first (they are evaluated before the body) and then its body's
/// statements in order; for an initialiser root, the single expression.
///
/// The one place a root is taken apart, so the three walkers below cannot
/// drift on *which* positions a root contributes — parameter defaults in
/// particular, which every hand-rolled walker in the Java backend forgot.
fn walk_root_parts(root: BodyRoot<'_>, f: &mut dyn FnMut(RootPart<'_>)) {
    fn callable(params: &[Param], body: Option<&Block>, f: &mut dyn FnMut(RootPart<'_>)) {
        for default in params.iter().filter_map(|p| p.default.as_ref()) {
            f(RootPart::Expr(default));
        }
        for s in body.into_iter().flat_map(|b| &b.stmts) {
            f(RootPart::Stmt(s));
        }
    }
    match root {
        BodyRoot::Main(stmts) => {
            for s in stmts {
                f(RootPart::Stmt(s));
            }
        }
        BodyRoot::Function(fun) => callable(&fun.params, fun.body.as_ref(), f),
        BodyRoot::Method(_, m) | BodyRoot::Constructor(_, m) => {
            callable(&m.params, m.body.as_ref(), f);
        }
        BodyRoot::FieldInit(_, field) => {
            if let Some(init) = &field.init {
                f(RootPart::Expr(init));
            }
        }
        BodyRoot::GlobalInit(global) => {
            if let Some(init) = &global.init {
                f(RootPart::Expr(init));
            }
        }
    }
}

/// Invoke `f` on every statement of every body in `hir`, at any depth within
/// a body.
///
/// **Stops at a lambda**, like the shallow [`walk_stmt_child_stmts`] it is
/// built on: a lambda body has its own scope, and a consumer that must not
/// confuse it with the enclosing one belongs here. [`walk_file_stmts_deep`]
/// is the opposite contract — pick deliberately, the way the module docs
/// describe for the two layers underneath.
///
/// A global or field initialiser is an expression, not a statement, so those
/// roots contribute nothing to this walk; neither does a parameter default.
pub fn walk_file_stmts(hir: &HirFile, f: &mut impl FnMut(&Stmt)) {
    fn subtree(s: &Stmt, f: &mut dyn FnMut(&Stmt)) {
        f(s);
        walk_stmt_child_stmts(s, &mut |c| subtree(c, f));
    }
    walk_file_bodies(hir, &mut |root: BodyRoot<'_>| {
        walk_root_parts(root, &mut |part| {
            if let RootPart::Stmt(s) = part {
                subtree(s, &mut *f);
            }
        });
    });
}

/// Invoke `f` on every statement of every body in `hir`, **lambda bodies
/// included**.
///
/// Carries [`walk_stmts_deep`]'s contract up to file scope: a lambda body's
/// statements are reported as part of the enclosing sequence, with no scope
/// bookkeeping. For analyses that hunt bindings at any lambda depth; every
/// other consumer wants [`walk_file_stmts`].
///
/// The expression-shaped positions — a global or field initialiser, and each
/// parameter default — contribute the statements of any lambda inside them,
/// via [`walk_lambda_stmts_in_expr`]. Parameter defaults are a real
/// expression position that each of the Java backend's hand-rolled walkers
/// missed.
///
/// One gap is inherited from that primitive: a *lambda's* own parameter
/// defaults are not descended into, so a lambda nested inside another
/// lambda's default contributes no statements here. [`walk_file_exprs`],
/// which takes its descent from [`Visitable`] instead, has no such gap.
pub fn walk_file_stmts_deep(hir: &HirFile, f: &mut impl FnMut(&Stmt)) {
    walk_file_bodies(hir, &mut |root: BodyRoot<'_>| {
        walk_root_parts(root, &mut |part| match part {
            RootPart::Stmt(s) => walk_stmts_deep(s, &mut *f),
            RootPart::Expr(e) => walk_lambda_stmts_in_expr(e, &mut *f),
        });
    });
}

/// Invoke `f` on every expression of every body in `hir`: each root's own
/// expressions, every sub-expression, and — **crossing every lambda
/// boundary** — lambda parameter defaults and lambda bodies too.
///
/// The widest of the four, for the name-keyed analyses that care about
/// expressions rather than statements: *does this file assign to the builtin
/// `count` anywhere at all* (#253). Because it takes its descent from the
/// [`Visitable`] recursion rather than from the shallow leaf rule, an
/// expression-bodied lambda (`x -> count = 1`) is covered as well as a block
/// one — the two shapes are indistinguishable to the question being asked.
///
/// A consumer that must respect lambda scope must not use this; drive
/// [`walk_file_stmts`] and the shallow expression primitives instead.
pub fn walk_file_exprs(hir: &HirFile, f: &mut impl FnMut(&Expr)) {
    let mut visitor = OnExpr(|e: &Expr| {
        f(e);
        Flow::Walk
    });
    walk_file_bodies(hir, &mut |root: BodyRoot<'_>| {
        walk_root_parts(root, &mut |part| {
            let _ = match part {
                RootPart::Stmt(s) => s.walk(&mut visitor),
                RootPart::Expr(e) => e.walk(&mut visitor),
            };
        });
    });
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
        ExprKind::Field(base, ..) => f(base),
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
        ExprKind::Array(items) => {
            for it in items {
                f(it);
            }
        }
        ExprKind::Set(items) => {
            for it in items {
                f(&mut it.start);
                if let Some(end) = &mut it.end {
                    f(end);
                }
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
