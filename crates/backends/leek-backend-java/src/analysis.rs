//! Whole-file analyses the Java emitter runs *once*, before any output.
//!
//! Every set in [`Analysis`] is a pure function of the HIR file and the
//! language version: nothing here observes emission order, and nothing the
//! emitter does can change an answer. That is what lets one [`Analysis`] be
//! shared by reference across the emitter and every scratch emitter
//! `render_block_to_string` spins up for a block-bodied lambda — before, each
//! of those deep-cloned six sets per lambda, so the cost grew with
//! (lambdas x state size) (#307).
//!
//! Keeping them together also names the boundary the JIR lowering pass wants:
//! `lower(hir, &Analysis) -> Jir` takes exactly this, and nothing else the
//! emitter carries (#252).
//!
//! What is *not* here: `Emitter::ref_boxes`. It is seeded from
//! [`Analysis::caller_boxes`] but genuinely grows during emission (each
//! `@`-param rebind registers its def), so it is per-scope emitter state, not
//! analysis.

use leek_hir::{Callee, Def, DefId, Expr, ExprKind, HirFile, LambdaBody, NameRef, Stmt};
use leek_syntax::Version;
use std::collections::{HashMap, HashSet};

use crate::emit::lambda::{
    collect_inner_decls, foreach_bind_targets, lambda_outer_captures, lambda_writes_to_outer,
};

/// The emitter's read-only view of the whole file, computed once by
/// [`Analysis::compute`] and then shared by reference.
pub(crate) struct Analysis {
    /// Builtin names the source reassigns somewhere (`push = 1`,
    /// `cos = function(...) {...}`). At v1 upstream allows this and subsequent
    /// reads/calls of the name see the user's value instead of the builtin. We
    /// route those through a `__shadows` HashMap field on the AI class — see
    /// `Emitter::emit_file`. Read-only during emission: the fallback arm of a
    /// shadow ternary is emitted through `write_call_unshadowed` /
    /// `write_name_unshadowed` rather than by clearing this set and
    /// re-entering.
    pub(crate) shadowed_builtins: HashSet<String>,
    /// Function-local variables that must be heap-boxed (`Object[]`) because a
    /// directly-nested lambda captures *and writes* them. LeekScript closures
    /// capture by reference, so the write must be visible in the enclosing
    /// scope — Java's effectively-final rule forbids that for a plain captured
    /// local, so the variable is shared through a 1-element array and every
    /// read/write goes via `[0]` (the same trick as `_self_box`). `DefId`s are
    /// unique file-wide, so a single set serves every function.
    pub(crate) boxed_locals: HashSet<DefId>,
    /// v1 only. Locals passed at a `@`-ref parameter position, which therefore
    /// bind to a runtime `Box` at their declaration. Seeds `Emitter::ref_boxes`,
    /// which then grows as `@` params are rebound during emission.
    pub(crate) caller_boxes: HashSet<DefId>,
    /// v1 only. `var f = function(@a){…}` bindings → which call positions are
    /// written-`@` (so an `execute(f, …)` arm passes the box for a ref-box arg
    /// there, aliasing the caller's variable). Keyed by the var's `DefId.0`.
    /// Empty at v2+ (no by-ref propagation).
    pub(crate) var_ref_positions: HashMap<u32, Vec<bool>>,
    /// v1 only. Callees whose call *result* is a `Box` upstream: their body
    /// `return`s a plain variable (every v1 local is a Box there) — directly
    /// or transitively through another such callee. A v1 `var x = f()` then
    /// mirrors upstream's `new Box(ai, f())` clone-if-box with a `copy(...)`
    /// wrapper (see `v1_store_clone`). Two id spaces: named-function defs
    /// (index into `hir.defs`) and lambda-holding local vars (`DefId.0`).
    /// Empty at v2+.
    pub(crate) returns_box_fns: HashSet<u32>,
    pub(crate) returns_box_vars: HashSet<u32>,
    /// Param defs spliced as synthetic body-leading locals into a
    /// default-param overload (see `Emitter::emit_default_overload`). At v2+
    /// upstream binds an omitted param with only the default expression's own
    /// cost (`ops(0);` for a literal) — no +1 declaration tick — so
    /// `emit_var_decl` drops its base cost for these. v1 keeps the +1 (it
    /// matches the Box ctor's runtime charge).
    pub(crate) synthetic_default_decls: HashSet<DefId>,
}

impl Analysis {
    /// Run every analysis over `hir`.
    ///
    /// The two by-ref analyses are v1-only — v2+ `@` params are plain, with no
    /// propagation — so at v2+ their three sets stay empty and the walks are
    /// skipped entirely, exactly as `Emitter::new` used to gate them.
    pub(crate) fn compute(hir: &HirFile, version: Version) -> Analysis {
        let v1 = matches!(version, Version::V1);
        let (caller_boxes, var_ref_positions) = if v1 {
            caller_box_locals(hir)
        } else {
            (HashSet::new(), HashMap::new())
        };
        let (returns_box_fns, returns_box_vars) = if v1 {
            v1_box_returners(hir)
        } else {
            (HashSet::new(), HashSet::new())
        };
        Analysis {
            shadowed_builtins: collect_shadowed_builtins(hir),
            boxed_locals: collect_boxed_locals(hir),
            caller_boxes,
            var_ref_positions,
            returns_box_fns,
            returns_box_vars,
            synthetic_default_decls: synthetic_default_decls(hir),
        }
    }
}

/// The param defs a default-arity overload splices in as body-leading locals.
///
/// `Emitter::emit_function` emits one overload per arity in
/// `min_arity..full_arity` and each marks `params[arity..]`, so the union over
/// the whole loop is just `params[min_arity..]` — computable here without
/// watching emission happen.
///
/// Only `Def::Function` contributes. A class method, a constructor and a
/// static method emit their default-arity forwarders through
/// `Emitter::default_overload_body`, which writes the omitted bindings as Java
/// text rather than splicing HIR `VarDecl`s, so no `emit_var_decl` ever sees
/// one of their param defs and marking them would put defs in this set that
/// nothing reads. A bodiless function (an external signature) emits no body at
/// all and so no overloads either.
fn synthetic_default_decls(hir: &HirFile) -> HashSet<DefId> {
    let mut out = HashSet::new();
    for def in &hir.defs {
        let Def::Function(f) = def else { continue };
        if f.body.is_none() {
            continue;
        }
        if let Some(min) = f.params.iter().position(|p| p.default.is_some()) {
            out.extend(f.params[min..].iter().map(|p| p.def));
        }
    }
    out
}

/// Collect every builtin name this file *writes*.
///
/// A write to a name-keyed reference (`NameRef::Builtin` / `Unresolved`) has
/// no Java variable behind it, so the emitter routes it through the AI class's
/// `__shadows` map and makes every later read or call of that name test the
/// map first. Two positions write a name that way:
///
/// - an **assignment** whose left-hand side is such a reference. Every form in
///   the family counts, not just plain `=`: `cos += 1` stores to `cos` as
///   surely as `cos = 1` does, and the store site (`write_place_store`) needs
///   the name in this set either way.
/// - a bare **`foreach` binding** (`for (push in […])`), which stores one slot
///   per iteration into whatever the name already denotes with no assignment
///   anywhere in the file (#371). The bind targets are l-values that
///   `walk_stmt_child_exprs` deliberately does not report, so they are asked
///   for by name through [`lambda::foreach_bind_targets`], the same helper
///   every other walk in this backend uses for the question.
///
/// Both walks are rooted at [`leek_hir::walk_file_bodies`] (through the
/// expression- and statement-shaped conveniences over it), so a class method,
/// a constructor, a field initialiser, a global initialiser and a parameter
/// default all count. The hand-rolled walker this replaced re-derived the
/// whole `ExprKind` descent and rooted it at the main block plus
/// `Def::Function` bodies, so a builtin reassigned anywhere inside a class
/// body was invisible (#253).
///
/// One gap is inherited from [`leek_hir::walk_file_stmts_deep`] and documented
/// there: a `foreach` inside a lambda that is itself nested in *another
/// lambda's* parameter default contributes no statements. The assignment walk
/// has no such gap.
fn collect_shadowed_builtins(hir: &HirFile) -> HashSet<String> {
    let mut out: HashSet<String> = HashSet::new();
    /// The builtin name a write to `target` shadows, when `target` is one of
    /// the two name-keyed references — the exact pair `write_place_store` and
    /// `write_name` test the set with.
    fn shadowed_name(target: &Expr) -> Option<&str> {
        match &target.kind {
            ExprKind::Name(NameRef::Builtin(name) | NameRef::Unresolved(name)) => Some(name),
            _ => None,
        }
    }
    leek_hir::walk_file_exprs(hir, &mut |e| {
        if let ExprKind::Binary(op, lhs, _) = &e.kind
            && op.is_assignment()
            && let Some(name) = shadowed_name(lhs)
        {
            out.insert(name.to_owned());
        }
    });
    leek_hir::walk_file_stmts_deep(hir, &mut |s| {
        if let Stmt::Foreach(fe) = s {
            out.extend(
                foreach_bind_targets(fe)
                    .filter_map(shadowed_name)
                    .map(str::to_owned),
            );
        }
    });
    out
}

/// `(locals to box, var-binding → written-`@` positions)` — see
/// [`caller_box_locals`].
type CallerBoxInfo = (HashSet<DefId>, HashMap<u32, Vec<bool>>);

/// v1 by-ref propagation analysis. A local passed as an argument at ANY `@`-ref
/// parameter position binds to a runtime `Box` at its declaration, so the
/// callee can alias it (upstream boxes *every* v1 local; we only need the ones
/// a `@` param might alias — the 2-arg Box ctor's runtime `ops(1)` replaces
/// the plain decl's static `ops(init, 1)`, so plain decls stay
/// charge-equivalent without boxing). Returns the set of such locals (seeded
/// into `ref_boxes`) plus, for `var f = function(@a){…}` bindings, the `@`
/// positions keyed by the var's def (so an `execute(f, …)` call can pass the
/// bare box at the right argument).
///
/// Foreach bindings are exempt: `emit_foreach` declares them as plain
/// `Object` slots with a static setup charge, so boxing them here would
/// emit `.get()` reads against a non-Box declaration.
fn caller_box_locals(hir: &HirFile) -> CallerBoxInfo {
    use leek_hir::Param;

    fn ref_positions(params: &[Param]) -> Vec<bool> {
        params.iter().map(|p| p.is_by_ref).collect()
    }

    // `var f = function(@a){…}` bindings → the lambda's `@` positions, so a
    // `f(b)` call resolves. Also the assign form (`var aux; aux =
    // function(@a){…}` — the usual v1 recursion idiom). Store the owned
    // `Vec<bool>` (not a reference) so it outlives the walk.
    let mut var_lambda_pos: HashMap<u32, Vec<bool>> = HashMap::new();
    fn collect_var_lambdas(s: &Stmt, m: &mut HashMap<u32, Vec<bool>>) {
        match s {
            Stmt::VarDecl(v) => {
                if let Some(init) = &v.init
                    && let ExprKind::Lambda(l) = &init.kind
                {
                    m.insert(v.def.0, ref_positions(&l.params));
                }
            }
            Stmt::Expr(e) => {
                if let ExprKind::Binary(leek_hir::BinaryOp::Assign, lhs, rhs) = &e.kind
                    && let ExprKind::Name(NameRef::Local(id)) = &lhs.kind
                    && let ExprKind::Lambda(l) = &rhs.kind
                {
                    m.insert(id.0, ref_positions(&l.params));
                }
            }
            _ => {}
        }
    }

    // Foreach bindings can't take the Box declaration shape (see doc above) —
    // collect their defs so the marking pass skips them.
    fn collect_foreach_binds(s: &Stmt, out: &mut HashSet<DefId>) {
        if let Stmt::Foreach(fe) = s {
            out.extend(
                fe.key
                    .iter()
                    .chain([&fe.value])
                    .filter_map(leek_hir::ForeachBind::local_def),
            );
        }
    }

    // `walk_file_stmts_deep`, not `walk_file_stmts`: by-ref propagation has to
    // see recursive `aux(copy, …)` calls *inside* the lambda that defines
    // `aux` — that's where the `@`-aliased locals are declared. What changed
    // is only where the walk starts. This used to hand-list main + top-level
    // functions + class methods and constructors, which left a `@`-ref call in
    // a field initialiser, a global initialiser or a parameter default
    // unanalysed; `walk_file_bodies` underneath answers that once, for every
    // walk in this crate (#253).
    leek_hir::walk_file_stmts_deep(hir, &mut |s| collect_var_lambdas(s, &mut var_lambda_pos));

    let mut foreach_binds: HashSet<DefId> = HashSet::new();
    leek_hir::walk_file_stmts_deep(hir, &mut |s| collect_foreach_binds(s, &mut foreach_binds));

    // Resolve each call's `@` positions, then mark the local args sitting in
    // them. `walk_file_exprs` carries the same lambda-crossing contract the
    // hand-rolled descent here had, and closes the two holes it left: a
    // lambda's own parameter defaults, and the roots above.
    let mut out: HashSet<DefId> = HashSet::new();
    leek_hir::walk_file_exprs(hir, &mut |e| {
        let ExprKind::Call(c) = &e.kind else { return };
        let positions = match &c.callee {
            Callee::Function(NameRef::Function(fid)) => match hir.defs.get(fid.0 as usize) {
                Some(Def::Function(f)) => Some(ref_positions(&f.params)),
                _ => None,
            },
            Callee::Function(NameRef::Local(fid)) => var_lambda_pos.get(&fid.0).cloned(),
            _ => None,
        };
        if let Some(positions) = positions {
            for (i, arg) in c.args.iter().enumerate() {
                if positions.get(i).copied().unwrap_or(false)
                    && let ExprKind::Name(NameRef::Local(id)) = &arg.kind
                {
                    out.insert(*id);
                }
            }
        }
        // `t[i](args)` — dynamically dispatched through
        // `executeArrayAccess`, so the callee (and its `@` positions)
        // is unknowable statically. Upstream passes every variable arg
        // as its Box and the callee's `instanceof Box` binding decides
        // aliasing (an `@` param aliases for free; a by-value param
        // copies). Mark every local arg so the call site can hand over
        // the box (charge-neutral for by-value callees — they copy the
        // content either way).
        if let Callee::Expr(inner) = &c.callee
            && matches!(inner.kind, ExprKind::Index(..))
        {
            for arg in &c.args {
                if let ExprKind::Name(NameRef::Local(id)) = &arg.kind {
                    out.insert(*id);
                }
            }
        }
    });

    for d in &foreach_binds {
        out.remove(d);
    }
    (out, var_lambda_pos)
}

/// v1 box-return analysis. Upstream boxes *every* v1 local, so a function or
/// lambda whose `return` hands back a plain variable (or an array/object
/// element — legacy array `get` returns the element's `Box`) returns a `Box`
/// to its caller; `var x = f()` then compiles upstream to `new Box(ai, f())`
/// whose 2-arg ctor clones Box inputs. We return raw (unboxed) values, so the
/// store sites consult this set to add the equivalent `copy(...)` (see
/// `v1_store_clone`). A return of a *call* propagates the callee's verdict
/// (`return cellsInRange(10)` forwards the inner Box untouched), hence the
/// fixpoint. Returns `(named-function defs, lambda-holding var defs)`, both
/// keyed by `DefId.0` in their respective id spaces.
fn v1_box_returners(hir: &HirFile) -> (HashSet<u32>, HashSet<u32>) {
    enum Dep {
        Fn(u32),
        Var(u32),
    }
    #[derive(Default)]
    struct Ev {
        direct: bool,
        deps: Vec<Dep>,
    }

    fn expr_evidence(e: &Expr, ev: &mut Ev) {
        match &e.kind {
            // A variable (Box upstream) or an element read (legacy array
            // `get` returns the element Box).
            ExprKind::Name(NameRef::Local(_) | NameRef::Global(_))
            | ExprKind::Field(..)
            | ExprKind::Index(..) => ev.direct = true,
            ExprKind::Call(c) => match &c.callee {
                Callee::Function(NameRef::Function(fid)) => ev.deps.push(Dep::Fn(fid.0)),
                Callee::Function(NameRef::Local(id)) => ev.deps.push(Dep::Var(id.0)),
                _ => {}
            },
            _ => {}
        }
    }

    // Top-level returns of a body — recurse through control flow but NOT into
    // nested lambdas (their returns belong to the lambda, not this callee).
    fn stmt_evidence(s: &Stmt, ev: &mut Ev) {
        if let Stmt::Return(Some(e)) = s {
            expr_evidence(e, ev);
        }
        leek_hir::visit::walk_stmt_child_stmts(s, &mut |c| stmt_evidence(c, ev));
    }
    fn body_evidence(stmts: &[Stmt]) -> Ev {
        let mut ev = Ev::default();
        for s in stmts {
            stmt_evidence(s, &mut ev);
        }
        ev
    }
    fn lambda_evidence(l: &leek_hir::LambdaExpr) -> Ev {
        match &l.body {
            leek_hir::LambdaBody::Block(b) => body_evidence(&b.stmts),
            leek_hir::LambdaBody::Expr(e) => {
                let mut ev = Ev::default();
                expr_evidence(e, &mut ev);
                ev
            }
        }
    }

    let mut fn_ev: HashMap<u32, Ev> = HashMap::new();
    let mut var_ev: HashMap<u32, Ev> = HashMap::new();

    // The per-function evidence table is keyed by index into `hir.defs` —
    // the id space `returns_box_fns` is consulted in — so it stays a `defs`
    // scan: it is a table, not a walk over the file's code.
    for (i, d) in (0u32..).zip(hir.defs.iter()) {
        if let Def::Function(f) = d
            && let Some(b) = &f.body
        {
            fn_ev.insert(i, body_evidence(&b.stmts));
        }
    }
    let mut collect_var_lambda = |s: &Stmt| match s {
        Stmt::VarDecl(v) => {
            if let Some(init) = &v.init
                && let ExprKind::Lambda(l) = &init.kind
            {
                var_ev.insert(v.def.0, lambda_evidence(l));
            }
        }
        Stmt::Expr(e) => {
            if let ExprKind::Binary(leek_hir::BinaryOp::Assign, lhs, rhs) = &e.kind
                && let ExprKind::Name(NameRef::Local(id)) = &lhs.kind
                && let ExprKind::Lambda(l) = &rhs.kind
            {
                var_ev.insert(id.0, lambda_evidence(l));
            }
        }
        _ => {}
    };
    // Deep walk (crosses lambda boundaries) for *finding* the candidates —
    // `var aux = function(){…}` bindings can live inside other lambdas — now
    // rooted at every body in the file through `walk_file_bodies`. The
    // hand-listed main + top-level functions this replaced reached neither a
    // class body nor a parameter default (#253).
    leek_hir::walk_file_stmts_deep(hir, &mut collect_var_lambda);

    // Fixpoint over the call-forwarding deps (cycles settle at "no").
    let mut fns: HashSet<u32> = fn_ev
        .iter()
        .filter(|(_, e)| e.direct)
        .map(|(k, _)| *k)
        .collect();
    let mut vars: HashSet<u32> = var_ev
        .iter()
        .filter(|(_, e)| e.direct)
        .map(|(k, _)| *k)
        .collect();
    loop {
        let mut changed = false;
        let resolved = |d: &Dep, fns: &HashSet<u32>, vars: &HashSet<u32>| match d {
            Dep::Fn(id) => fns.contains(id),
            Dep::Var(id) => vars.contains(id),
        };
        for (k, e) in &fn_ev {
            if !fns.contains(k) && e.deps.iter().any(|d| resolved(d, &fns, &vars)) {
                fns.insert(*k);
                changed = true;
            }
        }
        for (k, e) in &var_ev {
            if !vars.contains(k) && e.deps.iter().any(|d| resolved(d, &fns, &vars)) {
                vars.insert(*k);
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    (fns, vars)
}

/// Compute the file-wide set of VarDecl-declared locals that must be heap-boxed
/// because a lambda captures **and writes** them. LeekScript closures capture
/// by reference, so a write inside the lambda must be visible in the enclosing
/// scope; Java's effectively-final rule forbids that for a plain captured
/// local, so we share a one-element `Object[]` instead.
///
/// Lambdas are inspected at **every** nesting depth, on both sides:
/// - a `var` declared *inside* a lambda body is collected too, so a lambda
///   nested in that body can box it, and
/// - a write performed by a deeper lambda is attributed to the capture of
///   every lambda between it and the declaration, so each factory level
///   threads the same `Object[]` through.
///
/// The other binding forms carry their own box: a captured-written
/// function/method/constructor/lambda **parameter** binds to a runtime `Box`
/// at entry (see `emit_function` / `emit_class_method` / `write_lambda_inline`)
/// and so does a captured foreach binding (see `emit_foreach`). Between them
/// every binding form a lambda can write is shared, which is what lets
/// `write_lambda` outline unconditionally.
///
/// `DefId`s are unique across the whole HIR file, so one set serves every
/// function/method/main body.
///
/// Both halves are file-level walks over [`leek_hir::walk_file_bodies`]: the
/// declarations through the statement walk that crosses lambda boundaries, the
/// lambdas through the expression walk that crosses them *and* a lambda's own
/// parameter defaults. The hand-rolled `defs` match this replaced enumerated
/// top-level functions, class methods and constructors and the main block, so
/// a lambda living in a global initialiser, a field initialiser or a parameter
/// default was never analysed at all (#253).
fn collect_boxed_locals(hir: &HirFile) -> HashSet<DefId> {
    let mut out: HashSet<DefId> = HashSet::new();
    let mut var_decls = HashSet::new();
    leek_hir::walk_file_stmts_deep(hir, &mut |s| {
        if let Stmt::VarDecl(v) = s {
            var_decls.insert(v.def);
        }
    });

    let mut captured_written = HashSet::new();
    leek_hir::walk_file_exprs(hir, &mut |e| {
        let ExprKind::Lambda(l) = &e.kind else {
            return;
        };
        // An expression-bodied lambda is always emitted inline, so it needs no
        // box of its own; the walk reaches any block-bodied lambda inside it
        // on its own.
        let LambdaBody::Block(b) = &l.body else {
            return;
        };
        let mut inner: HashSet<_> = l.params.iter().map(|p| p.def).collect();
        collect_inner_decls(b, &mut inner);
        // `lambda_outer_captures` / `lambda_writes_to_outer` both see through
        // nested lambdas, so a write a deeper lambda performs is attributed to
        // this lambda's capture as well — every factory level then declares
        // the box as a parameter.
        for c in lambda_outer_captures(b, &inner) {
            let one = std::iter::once(c).collect();
            if lambda_writes_to_outer(b, &one) {
                captured_written.insert(c);
            }
        }
    });

    // A boxable local is one that is both a `var` declaration and is
    // captured-and-written by some lambda.
    out.extend(
        captured_written
            .into_iter()
            .filter(|d| var_decls.contains(d)),
    );
    out
}
