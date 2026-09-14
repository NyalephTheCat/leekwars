use leek_hir::{Expr, ExprKind, LambdaBody, LambdaExpr, NameRef, PostfixOp, Stmt, UnaryOp};
use std::fmt::Write as _;

use super::ends_with_return;
use crate::mangle;

impl super::Emitter<'_> {
    pub(crate) fn write_lambda(&self, buf: &mut String, l: &LambdaExpr) {
        // Block-bodied lambdas that capture outer locals get outlined
        // to a `__anon_<n>(final … captures)` factory method on the
        // AI class — Java's inner-class capture rules accept final
        // method parameters even when the original local was
        // reassignable. The call site becomes a call to the factory
        // passing the current values of the captures. See
        // `lambda_outer_captures` for the discovery walk and the
        // factory emit below.
        if let LambdaBody::Block(b) = &l.body {
            // "Inner scope" = lambda's params + every local declared
            // inside the lambda body itself. Outer captures are
            // references to *anything else*.
            let mut inner_scope: std::collections::HashSet<_> =
                l.params.iter().map(|p| p.def).collect();
            collect_inner_decls(b, &mut inner_scope);
            let mut captures = lambda_outer_captures(b, &inner_scope);
            // Drop the var currently being initialized — capturing
            // it produces a forward-reference (`__anon_N(u_self)`
            // where `u_self` is mid-declaration), which Java
            // rejects. Self-references go through the
            // `_self_box[0]` machinery below.
            let self_rec = self
                .initializing_def
                .get()
                .filter(|d| captures.contains(d) || lambda_references_initializing_def(b, *d));
            if let Some(self_def) = self_rec {
                captures.retain(|d| *d != self_def);
            }

            if !captures.is_empty() {
                // Every capture a lambda *writes* is shared through a box, so
                // outlining always expresses the write: a `var` declaration
                // becomes an `Object[]` (`boxed_locals`, threaded as a `final
                // Object[]` factory param), while a parameter or foreach
                // binding becomes a runtime `Box` (`ref_boxes`, threaded as a
                // `final Box`). Both survive Java's effectively-final rule.
                // This used to fall back to a lambda body that just returned
                // `null` — a silent wrong answer; the assertion below keeps a
                // future unhandled binding form from regressing to that (in
                // release the outlined emit is a javac "cannot assign to final
                // variable", i.e. loud, not silent).
                debug_assert!(
                    {
                        let unboxed: std::collections::HashSet<_> = {
                            let boxed = self.boxed_locals.borrow();
                            let ref_boxes = self.ref_boxes.borrow();
                            captures
                                .iter()
                                .copied()
                                .filter(|d| !boxed.contains(d) && !ref_boxes.contains(d))
                                .collect()
                        };
                        !lambda_writes_to_outer(b, &unboxed)
                    },
                    "lambda writes to a captured binding that was never boxed"
                );
                // Outlined factory. If the lambda is self-
                // recursive, route through the Supplier-box wrap
                // around the factory call and pass `_self_box` as
                // an extra final factory param so the body can
                // emit `_self_box[0]` for its self-references.
                if let Some(self_def) = self_rec {
                    buf.push_str(
                        "((java.util.function.Supplier<Object>) () -> { \
                        Object[] _self_box = new Object[1]; \
                        _self_box[0] = ",
                    );
                    let prev = self.self_rec_def.replace(Some(self_def));
                    self.emit_outlined_lambda(buf, l, b, &captures);
                    self.self_rec_def.set(prev);
                    buf.push_str("; return _self_box[0]; }).get()");
                } else {
                    self.emit_outlined_lambda(buf, l, b, &captures);
                }
                return;
            }
            // No outer captures but the body still references the
            // in-construction var — pure Supplier-box inline.
            if let Some(self_def) = self_rec {
                buf.push_str(
                    "((java.util.function.Supplier<Object>) () -> { \
                    Object[] _self_box = new Object[1]; \
                    _self_box[0] = ",
                );
                let prev = self.self_rec_def.replace(Some(self_def));
                self.write_lambda_inline(buf, l);
                self.self_rec_def.set(prev);
                buf.push_str("; return _self_box[0]; }).get()");
                return;
            }
        }
        self.write_lambda_inline(buf, l);
    }

    pub(crate) fn write_lambda_inline(&self, buf: &mut String, l: &LambdaExpr) {
        // Reference shape: anonymous subclass of (abstract)
        // `FunctionLeekValue` with arity, overriding `run(AI, Object,
        // Object...)`. Param destructure pulls each named param out
        // of `values[]` with a null fallback (so the lambda still
        // works when called with fewer args).
        let arity = l.params.len();
        buf.push_str("new FunctionLeekValue(");
        buf.push_str(&arity.to_string());
        buf.push_str(
            ") {public Object run(AI ai, Object thiz, Object... values) throws LeekRunException {",
        );
        for (i, p) in l.params.iter().enumerate() {
            let pname = mangle::local(self.opts, &p.name);
            let src = format!("(values.length > {i} ?  values[{i}] : null)");
            if self.is_v1_ref_param(p) {
                // `@x` at v1: bind to the runtime `Box` — alias the caller's box
                // if one was passed (the v1 runtime passes element boxes to `@`
                // callbacks), else box a copy. Reads/writes of `x` then route
                // through `Box` methods so mutations propagate.
                let _ = write!(
                    buf,
                    "Box {pname} = {src} instanceof Box ? (Box) ({src}) : new Box(ai, load({src}));"
                );
                self.ref_boxes.borrow_mut().insert(p.def);
            } else if matches!(self.opts.version, leek_syntax::Version::V1) {
                // Plain v1 lambda param: bind through the 2-arg Box ctor like
                // upstream (`var u_x = new Box(AI.this, values[0])`). At v1
                // the ctor deep-clones a *Box* argument (the runtime hands
                // callbacks element/arg boxes, and `execute` call sites pass
                // bare boxes for locals) — that clone is the value-semantics
                // copy AND its data-dependent op charge. A fresh value is
                // stored directly: no copy, no charge beyond the ctor's 1
                // (which replaces this param's share of `v1_param_box_ops`).
                let _ = write!(buf, "Box {pname} = new Box(ai, {src});");
                self.ref_boxes.borrow_mut().insert(p.def);
            } else if leek_hir::captured_by_nested_lambda_body(&l.body, p.def) {
                // Param captured by an inner lambda (`x -> y -> x + 1`) →
                // bind through a runtime `Box`; the 2-arg ctor charges the
                // same 1 op as upstream's `new Box<>(AI.this, p)` wrap.
                let _ = write!(buf, "final Box {pname} = new Box(ai, {src});");
                self.ref_boxes.borrow_mut().insert(p.def);
            } else {
                let _ = write!(buf, "var {pname} = {src};");
            }
        }
        // Bump lambda nesting so `ai_this()` returns `ai` inside the
        // body — bare `this` would otherwise resolve to the
        // anonymous FunctionLeekValue subclass.
        self.lambda_depth.set(self.lambda_depth.get() + 1);
        match &l.body {
            LambdaBody::Expr(e) => {
                let code = self.expr_to_string(e);
                if self.opts.emit_ops {
                    // (No `v1_param_box_ops` here: v1 lambda params bind
                    // through Box ctors above, which charge at runtime.)
                    let cost = self.emit_cost(e);
                    if cost > 0 {
                        let _ = write!(buf, "ops(1);ops({cost}); ");
                    } else {
                        buf.push_str("ops(1); ");
                    }
                }
                buf.push_str("return ");
                buf.push_str(&code);
                buf.push(';');
            }
            LambdaBody::Block(b) => {
                // Inline path — caller has already confirmed no
                // outer captures exist.
                if self.opts.emit_ops {
                    buf.push_str("ops(1);");
                }
                buf.push_str(&self.render_block_to_string(b));
                if !ends_with_return(&b.stmts, self.opts.emit_ops) {
                    buf.push_str("return null;");
                }
            }
        }
        buf.push_str("}}");
        self.lambda_depth.set(self.lambda_depth.get() - 1);
    }

    /// Emit a captured-block lambda by routing it through a
    /// synthesized `private FunctionLeekValue __anon_N(final Object
    /// u_x, …) { return new FunctionLeekValue(…) { … u_x … }; }`
    /// method on the AI class. The factory's final parameters act
    /// as the inner-class captures (Java accepts those even when the
    /// original outer locals are reassignable). The call site
    /// becomes `__anon_N(<current values of captures>)`.
    pub(crate) fn emit_outlined_lambda(
        &self,
        buf: &mut String,
        l: &LambdaExpr,
        body: &leek_hir::Block,
        captures: &[leek_hir::DefId],
    ) {
        let id = self.outline_counter.get();
        self.outline_counter.set(id + 1);
        let factory = format!("__anon_{id}");

        // Render the call site: `__anon_N(u_x, u_y, …[, _self_box])`.
        // `_self_box` is appended when this factory is being called
        // from inside a Supplier-box wrap for a self-recursive
        // lambda — see `write_lambda`.
        let pass_self_box = self.self_rec_def.get().is_some();
        buf.push_str(&factory);
        buf.push('(');
        for (i, def_id) in captures.iter().enumerate() {
            if i > 0 {
                buf.push_str(", ");
            }
            // A *nested* lambda may capture the var the enclosing lambda is
            // being assigned to (`var fact = function(n) { var h = function()
            // { return fact(n - 1) } … }`). The Java local is still
            // mid-initialization here, so read it out of the Supplier wrap's
            // `_self_box` like `write_name` does inside the body.
            if Some(*def_id) == self.self_rec_def.get() {
                buf.push_str("_self_box[0]");
                continue;
            }
            let name = self.def_name(*def_id).to_string();
            buf.push_str(&mangle::local(self.opts, &name));
        }
        if pass_self_box {
            if !captures.is_empty() {
                buf.push_str(", ");
            }
            buf.push_str("_self_box");
        }
        buf.push(')');

        // Build the factory body using a scratch emitter for the
        // lambda body itself. The body walks the lambda's inner
        // statements; outer-local refs there are picked up as the
        // factory's final params (same name) by Java's normal scope
        // rules.
        let arity = l.params.len();
        let mut factory_buf = String::new();
        factory_buf.push_str("private FunctionLeekValue ");
        factory_buf.push_str(&factory);
        factory_buf.push('(');
        for (i, def_id) in captures.iter().enumerate() {
            if i > 0 {
                factory_buf.push_str(", ");
            }
            let name = self.def_name(*def_id).to_string();
            // A boxed capture is passed as the shared array itself (`Object[]`),
            // so writes inside the lambda propagate to the enclosing scope; a
            // plain capture is passed by value as `final Object`. The call site
            // emits the raw mangled name for both — for a boxed local that name
            // *is* the array, so no `[0]` there.
            if self.boxed_locals.borrow().contains(def_id) {
                factory_buf.push_str("final Object[] ");
            } else if self.ref_boxes.borrow().contains(def_id) {
                // A captured `@`-ref-box param keeps its `Box` type so the body's
                // `.get()`/`Box` mutators resolve.
                factory_buf.push_str("final Box ");
            } else {
                factory_buf.push_str("final Object ");
            }
            factory_buf.push_str(&mangle::local(self.opts, &name));
        }
        if pass_self_box {
            if !captures.is_empty() {
                factory_buf.push_str(", ");
            }
            factory_buf.push_str("final Object[] _self_box");
        }
        // Factory body is just `return new FunctionLeekValue(...) {
        // ... };` — the construction itself doesn't throw
        // (`LeekRunException` lives on the inner `run` method, not
        // the constructor). When called from a Supplier wrap for
        // the self-rec pattern, declaring a `throws` here would
        // force a try/catch inside the non-throwing Supplier.
        factory_buf.push_str(") { return new FunctionLeekValue(");
        factory_buf.push_str(&arity.to_string());
        factory_buf.push_str(
            ") {public Object run(AI ai, Object thiz, Object... values) throws LeekRunException {",
        );
        for (i, p) in l.params.iter().enumerate() {
            let pname = mangle::local(self.opts, &p.name);
            let src = format!("(values.length > {i} ?  values[{i}] : null)");
            if self.is_v1_ref_param(p) {
                let _ = write!(
                    factory_buf,
                    "Box {pname} = {src} instanceof Box ? (Box) ({src}) : new Box(ai, load({src}));"
                );
                self.ref_boxes.borrow_mut().insert(p.def);
            } else if matches!(self.opts.version, leek_syntax::Version::V1) {
                // Plain v1 lambda param: 2-arg Box ctor binding — see
                // `write_lambda_inline` for the clone/charge semantics.
                let _ = write!(factory_buf, "Box {pname} = new Box(ai, {src});");
                self.ref_boxes.borrow_mut().insert(p.def);
            } else if !matches!(self.opts.version, leek_syntax::Version::V1)
                && leek_hir::captured_by_nested_lambda_stmts(&body.stmts, p.def)
            {
                // Param captured by an inner lambda → Box-bind (same shape
                // and 1-op ctor charge as upstream's `new Box<>(AI.this, p)`).
                let _ = write!(factory_buf, "final Box {pname} = new Box(ai, {src});");
                self.ref_boxes.borrow_mut().insert(p.def);
            } else {
                let _ = write!(factory_buf, "var {pname} = {src};");
            }
        }
        self.lambda_depth.set(self.lambda_depth.get() + 1);
        if self.opts.emit_ops {
            // (No `v1_param_box_ops`: v1 lambda params bind through Box
            // ctors above, which charge at runtime.)
            factory_buf.push_str("ops(1);");
        }
        // The factory is an AI-level method, so `<u_Class>.this` is out of scope
        // inside it — render the body with the outlined flag so an instance
        // `this` falls back to bare `this`.
        let prev_outlined = self.in_outlined.replace(true);
        factory_buf.push_str(&self.render_block_to_string(body));
        self.in_outlined.set(prev_outlined);
        if !ends_with_return(&body.stmts, self.opts.emit_ops) {
            factory_buf.push_str("return null;");
        }
        factory_buf.push_str("}}; }");
        self.lambda_depth.set(self.lambda_depth.get() - 1);
        self.outlined.borrow_mut().push(factory_buf);
    }
}

/// The l-values a `foreach` header stores into each iteration — the value
/// binding, and the key binding when the header has one.
///
/// These are **not** reported by `leek_hir::walk_stmt_child_exprs`, which
/// surfaces only the iterable. That is deliberate and stays that way: the
/// shared walk feeds leek-lint's rules and the LeekScript backend's
/// const-folder, none of which have ever seen an l-value come out of it, so
/// teaching it to emit bind targets would change lint and const-fold behaviour
/// well outside this backend. Every walk here that has to see a foreach write
/// names the targets explicitly through this helper instead — as does
/// `leek_hir::captures`, which answers the same question for the two
/// consumers that decide whether a *parameter* binding gets its runtime `Box`.
pub(crate) fn foreach_bind_targets(fe: &leek_hir::ForeachStmt) -> impl Iterator<Item = &Expr> {
    fe.key
        .iter()
        .chain([&fe.value])
        .map(|b: &leek_hir::ForeachBind| &b.target)
}

pub(crate) fn lambda_outer_captures(
    block: &leek_hir::Block,
    params: &std::collections::HashSet<leek_hir::DefId>,
) -> Vec<leek_hir::DefId> {
    let mut out: Vec<leek_hir::DefId> = Vec::new();
    let mut seen: std::collections::HashSet<leek_hir::DefId> = std::collections::HashSet::new();
    fn note(
        id: leek_hir::DefId,
        params: &std::collections::HashSet<leek_hir::DefId>,
        out: &mut Vec<leek_hir::DefId>,
        seen: &mut std::collections::HashSet<leek_hir::DefId>,
    ) {
        if !params.contains(&id) && seen.insert(id) {
            out.push(id);
        }
    }
    fn expr(
        e: &Expr,
        params: &std::collections::HashSet<leek_hir::DefId>,
        out: &mut Vec<leek_hir::DefId>,
        seen: &mut std::collections::HashSet<leek_hir::DefId>,
    ) {
        match &e.kind {
            // The two reads that *are* the answer: a bare local reference, and
            // a local used as a callee (`f(1)` where `f` holds a lambda). A
            // `Call`'s callee is not a sub-expression when it is a name, so
            // the shared child walk below never offers it.
            ExprKind::Name(NameRef::Local(id)) => note(*id, params, out, seen),
            ExprKind::Call(c) => {
                if let leek_hir::Callee::Function(NameRef::Local(id)) = &c.callee {
                    note(*id, params, out, seen);
                }
            }
            // Nested lambdas: descend with their own scope excluded. A
            // nested lambda's params and locals shadow the enclosing
            // scope, but anything *else* it references is still an outer
            // capture of **this** lambda — the outlined factory has to
            // receive it so the inner factory's call site (emitted inside
            // the outer factory body) can pass it on. Skipping the descent
            // used to emit `__anon_1(a)` inside an `__anon_0` that never
            // declared `a`, a javac "cannot find symbol".
            //
            // This arm is why the descent below is not the whole story:
            // the scope the children are read against changes here, and
            // `walk_expr_children` treats a lambda as a leaf precisely so a
            // consumer with a scope rule has to state it.
            ExprKind::Lambda(l) => {
                let mut nested: std::collections::HashSet<leek_hir::DefId> = params.clone();
                nested.extend(l.params.iter().map(|p| p.def));
                match &l.body {
                    LambdaBody::Expr(b) => expr(b, &nested, out, seen),
                    LambdaBody::Block(b) => {
                        collect_inner_decls(b, &mut nested);
                        block_walk(b, &nested, out, seen);
                    }
                }
                return;
            }
            _ => {}
        }
        // Every other variant is plain descent, so it goes through the shared
        // variant-complete child walk instead of a copy of it. The hand-rolled
        // match this replaced is the class of code that let this backend's
        // walkers drift apart (#253); `lambda_writes_to_outer`, which answers
        // the other half of the same question, was already written this way.
        leek_hir::walk_expr_children(e, &mut |c| expr(c, params, out, seen));
    }
    fn stmt(
        s: &Stmt,
        params: &std::collections::HashSet<leek_hir::DefId>,
        out: &mut Vec<leek_hir::DefId>,
        seen: &mut std::collections::HashSet<leek_hir::DefId>,
    ) {
        match s {
            Stmt::Expr(e) | Stmt::Return(Some(e)) => expr(e, params, out, seen),
            Stmt::Return(None) | Stmt::Break(_) | Stmt::Continue(_) => {}
            Stmt::VarDecl(d) => {
                if let Some(e) = &d.init {
                    expr(e, params, out, seen);
                }
            }
            Stmt::Block(b) => block_walk(b, params, out, seen),
            Stmt::If(i) => {
                expr(&i.cond, params, out, seen);
                stmt(&i.then_branch, params, out, seen);
                if let Some(s) = i.else_branch.as_deref() {
                    stmt(s, params, out, seen);
                }
            }
            Stmt::While(w) => {
                expr(&w.cond, params, out, seen);
                stmt(&w.body, params, out, seen);
            }
            Stmt::DoWhile(d) => {
                stmt(&d.body, params, out, seen);
                expr(&d.cond, params, out, seen);
            }
            Stmt::For(f) => {
                if let Some(s) = f.init.as_deref() {
                    stmt(s, params, out, seen);
                }
                if let Some(e) = &f.cond {
                    expr(e, params, out, seen);
                }
                if let Some(e) = &f.step {
                    expr(e, params, out, seen);
                }
                stmt(&f.body, params, out, seen);
            }
            Stmt::Foreach(fe) => {
                expr(&fe.iter, params, out, seen);
                // The binding targets are l-values, not declarations: a bare
                // `for (x in …)` over an outer local *writes* that local every
                // iteration, so the lambda captures it. `walk_stmt_child_exprs`
                // deliberately does not report them — see `foreach_bind_targets`.
                for t in foreach_bind_targets(fe) {
                    expr(t, params, out, seen);
                }
                stmt(&fe.body, params, out, seen);
            }
            Stmt::Switch(sw) => {
                expr(&sw.discriminant, params, out, seen);
                for a in &sw.arms {
                    if let Some(e) = &a.case {
                        expr(e, params, out, seen);
                    }
                    for s in &a.body {
                        stmt(s, params, out, seen);
                    }
                }
            }
            // Listed rather than caught by `_` so a new `Stmt` variant
            // that can hold a capture is a compile error here.
            Stmt::Include(_) | Stmt::Import(_) | Stmt::Charge(_) => {}
        }
    }
    fn block_walk(
        b: &leek_hir::Block,
        params: &std::collections::HashSet<leek_hir::DefId>,
        out: &mut Vec<leek_hir::DefId>,
        seen: &mut std::collections::HashSet<leek_hir::DefId>,
    ) {
        for s in &b.stmts {
            stmt(s, params, out, seen);
        }
    }
    block_walk(block, params, &mut out, &mut seen);
    out
}

/// Walk a lambda body and add every `VarDecl` inside it to `inner`.
/// Used by the lambda emitter to distinguish locals that the body
/// declares from outer-scope captures — `var f = function(x) { var
/// r = x ** 2 return r + 1 }` reads `r` but `r` is declared inside
/// the body and isn't captured.
pub(crate) fn collect_inner_decls(
    block: &leek_hir::Block,
    inner: &mut std::collections::HashSet<leek_hir::DefId>,
) {
    fn stmt(s: &Stmt, inner: &mut std::collections::HashSet<leek_hir::DefId>) {
        match s {
            Stmt::VarDecl(v) => {
                inner.insert(v.def);
            }
            Stmt::Block(b) => collect_inner_decls(b, inner),
            Stmt::If(i) => {
                stmt(&i.then_branch, inner);
                if let Some(s) = i.else_branch.as_deref() {
                    stmt(s, inner);
                }
            }
            Stmt::While(w) => stmt(&w.body, inner),
            Stmt::DoWhile(d) => stmt(&d.body, inner),
            Stmt::For(f) => {
                if let Some(s) = f.init.as_deref() {
                    stmt(s, inner);
                }
                stmt(&f.body, inner);
            }
            Stmt::Foreach(fe) => {
                // `for (var k : var v in iter)` — `k` and `v` are locals
                // bound by the loop header, not separate VarDecl
                // statements. Add them so they're not treated as
                // outer captures.
                //
                // Only the bindings the header *declares* (`is_new`). A bare
                // `for (x in …)` reuses a binding from an enclosing scope and
                // writes it; that is a capture of this lambda, not a local it
                // declares, and listing it here used to hide it from
                // `lambda_outer_captures` — so the outlined body assigned to
                // an outer local Java never boxed.
                inner.extend(
                    fe.key
                        .iter()
                        .chain([&fe.value])
                        .filter(|b| b.is_new)
                        .filter_map(leek_hir::ForeachBind::local_def),
                );
                stmt(&fe.body, inner);
            }
            Stmt::Switch(sw) => {
                for a in &sw.arms {
                    for s in &a.body {
                        stmt(s, inner);
                    }
                }
            }
            // Listed rather than caught by `_` so a new `Stmt` variant
            // that can declare a binding is a compile error here.
            Stmt::Expr(_)
            | Stmt::Return(_)
            | Stmt::Break(_)
            | Stmt::Continue(_)
            | Stmt::Include(_)
            | Stmt::Import(_)
            | Stmt::Charge(_) => {}
        }
    }
    for s in &block.stmts {
        stmt(s, inner);
    }
}

/// True when `block` references `def` (the var being initialized by
/// the enclosing `var X = …`). Used to detect self-recursive
/// lambdas after we've already filtered the def from the captures
/// list — if the body still tries to call it, outline + bare-null
/// is wrong; fall back to the no-op wrapper.
pub(crate) fn lambda_references_initializing_def(
    block: &leek_hir::Block,
    def: leek_hir::DefId,
) -> bool {
    fn expr(e: &Expr, def: leek_hir::DefId) -> bool {
        match &e.kind {
            ExprKind::Name(NameRef::Local(id)) => return *id == def,
            // `walk_expr_children` doesn't surface a `Callee::Function`
            // name (it's a `NameRef`, not a child `Expr`) — check it
            // here, since `f(…)` inside `var f = function…` is the
            // shape this walk exists for. Other callee forms are
            // ordinary children and fall through to the descent.
            ExprKind::Call(c)
                if matches!(
                    &c.callee,
                    leek_hir::Callee::Function(NameRef::Local(id)) if *id == def
                ) =>
            {
                return true;
            }
            // A nested lambda is a leaf to `walk_expr_children`, but a
            // self-reference from inside one is still a self-reference
            // (`var f = function(n) { var g = function() { return
            // f(n-1) } … }`), so descend explicitly.
            ExprKind::Lambda(l) => {
                return match &l.body {
                    LambdaBody::Expr(b) => expr(b, def),
                    LambdaBody::Block(b) => b.stmts.iter().any(|s| stmt(s, def)),
                };
            }
            _ => {}
        }
        let mut found = false;
        leek_hir::walk_expr_children(e, &mut |c| found = found || expr(c, def));
        found
    }
    fn stmt(s: &Stmt, def: leek_hir::DefId) -> bool {
        let mut found = false;
        leek_hir::walk_stmt_child_exprs(s, &mut |e| found = found || expr(e, def));
        if !found {
            leek_hir::walk_stmt_child_stmts(s, &mut |c| found = found || stmt(c, def));
        }
        found
    }
    block.stmts.iter().any(|s| stmt(s, def))
}

/// True when `block` contains an assignment whose l-value is a
/// captured outer local (i.e. an outer local also in `captures`).
/// The outlined-lambda factory passes captures as `final`
/// parameters; writes from inside the lambda body would fail Java's
/// "cannot assign to final variable" check.
//
pub(crate) fn lambda_writes_to_outer(
    block: &leek_hir::Block,
    captures: &std::collections::HashSet<leek_hir::DefId>,
) -> bool {
    fn is_captured_local(e: &Expr, captures: &std::collections::HashSet<leek_hir::DefId>) -> bool {
        matches!(&e.kind, ExprKind::Name(NameRef::Local(id)) if captures.contains(id))
    }
    fn expr(e: &Expr, captures: &std::collections::HashSet<leek_hir::DefId>) -> bool {
        // Only the l-value tests are special-cased; everything else is
        // plain descent, so the variant-complete `match` stays in
        // `leek_hir::visit` instead of being re-enumerated here. It used
        // to be re-enumerated, and `Slice`/`Interval` were missing: a
        // capture written only from a slice bound (`arr[acc++ : 3]`)
        // looked write-free and was handed to the factory as a `final`
        // parameter, which javac rejects.
        match &e.kind {
            ExprKind::Binary(op, l, _) if op.is_assignment() => {
                if is_captured_local(l, captures) {
                    return true;
                }
            }
            ExprKind::Unary(UnaryOp::PreInc | UnaryOp::PreDec, inner)
            | ExprKind::Postfix(PostfixOp::PostInc | PostfixOp::PostDec, inner) => {
                if is_captured_local(inner, captures) {
                    return true;
                }
            }
            // A write performed by a *nested* lambda still mutates the
            // enclosing binding, so it counts as a write to this lambda's
            // capture — the box has to be threaded through both factory
            // levels. `DefId`s are unique per binding, so a nested param or
            // local can never alias `captures`; no scope bookkeeping needed.
            // `walk_expr_children` treats a lambda as a leaf, so this arm
            // is load-bearing, not a leftover.
            ExprKind::Lambda(l) => {
                return match &l.body {
                    LambdaBody::Expr(b) => expr(b, captures),
                    LambdaBody::Block(b) => b.stmts.iter().any(|s| stmt(s, captures)),
                };
            }
            _ => {}
        }
        let mut found = false;
        leek_hir::walk_expr_children(e, &mut |c| found = found || expr(c, captures));
        found
    }
    fn stmt(s: &Stmt, captures: &std::collections::HashSet<leek_hir::DefId>) -> bool {
        // A bare `for (x in …)` stores into `x` every iteration — a write to
        // the capture, and the only one that is not an assignment expression.
        // `walk_stmt_child_exprs` reports only the iterable (see
        // `foreach_bind_targets`), so ask the targets here.
        if let Stmt::Foreach(fe) = s
            && foreach_bind_targets(fe).any(|t| is_captured_local(t, captures))
        {
            return true;
        }
        let mut found = false;
        leek_hir::walk_stmt_child_exprs(s, &mut |e| found = found || expr(e, captures));
        if !found {
            leek_hir::walk_stmt_child_stmts(s, &mut |c| found = found || stmt(c, captures));
        }
        found
    }
    block.stmts.iter().any(|s| stmt(s, captures))
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
pub(crate) fn collect_boxed_locals(
    hir: &leek_hir::HirFile,
    out: &mut std::collections::HashSet<leek_hir::DefId>,
) {
    let mut var_decls = std::collections::HashSet::new();
    leek_hir::walk_file_stmts_deep(hir, &mut |s| {
        if let Stmt::VarDecl(v) = s {
            var_decls.insert(v.def);
        }
    });

    let mut captured_written = std::collections::HashSet::new();
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
        let mut inner: std::collections::HashSet<_> = l.params.iter().map(|p| p.def).collect();
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
}
