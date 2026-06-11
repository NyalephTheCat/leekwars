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
                // Writes to a *boxed* captured local are now correct (the local
                // is a shared `Object[]`, passed to the factory as a `final
                // Object[]` param). Only a write to an *unboxed* captured local
                // — a parameter, foreach binding, or nested-lambda capture we
                // don't box — still can't be expressed, so that residual case
                // keeps the null-returning stub.
                let unboxed_captures: std::collections::HashSet<_> = {
                    let boxed = self.boxed_locals.borrow();
                    let ref_boxes = self.ref_boxes.borrow();
                    captures
                        .iter()
                        .copied()
                        // A captured `@`-ref param is a runtime `Box` (passed to
                        // the factory as `final Box`), so a write through it
                        // *is* expressible — `a += 2` routes to the Box mutator
                        // and propagates. Only genuinely unboxed captures force
                        // the null stub.
                        .filter(|d| !boxed.contains(d) && !ref_boxes.contains(d))
                        .collect()
                };
                if lambda_writes_to_outer(b, &unboxed_captures) {
                    self.write_lambda_inline_fallback(buf, l);
                    return;
                }
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

    /// Block-body lambda emit that always returns null. Used when
    /// outlining isn't viable (the body writes to a captured local).
    /// The surrounding code at least compiles; the call's return
    /// value is a value mismatch instead of a javac error.
    // Takes `&self` for symmetry with `write_lambda_inline`, though the
    // null-fallback shape it emits doesn't depend on emitter state.
    #[allow(clippy::unused_self)]
    pub(crate) fn write_lambda_inline_fallback(&self, buf: &mut String, l: &LambdaExpr) {
        let arity = l.params.len();
        buf.push_str("new FunctionLeekValue(");
        buf.push_str(&arity.to_string());
        buf.push_str(") {public Object run(AI ai, Object thiz, Object... values) throws LeekRunException {return null;}}");
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
                write!(
                    buf,
                    "Box {pname} = {src} instanceof Box ? (Box) ({src}) : new Box(ai, load({src}));"
                )
                .unwrap();
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
                write!(buf, "Box {pname} = new Box(ai, {src});").unwrap();
                self.ref_boxes.borrow_mut().insert(p.def);
            } else if captured_by_nested_lambda_body(&l.body, p.def) {
                // Param captured by an inner lambda (`x -> y -> x + 1`) →
                // bind through a runtime `Box`; the 2-arg ctor charges the
                // same 1 op as upstream's `new Box<>(AI.this, p)` wrap.
                write!(buf, "final Box {pname} = new Box(ai, {src});").unwrap();
                self.ref_boxes.borrow_mut().insert(p.def);
            } else {
                write!(buf, "var {pname} = {src};").unwrap();
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
                        write!(buf, "ops(1);ops({cost}); ").unwrap();
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
                write!(
                    factory_buf,
                    "Box {pname} = {src} instanceof Box ? (Box) ({src}) : new Box(ai, load({src}));"
                )
                .unwrap();
                self.ref_boxes.borrow_mut().insert(p.def);
            } else if matches!(self.opts.version, leek_syntax::Version::V1) {
                // Plain v1 lambda param: 2-arg Box ctor binding — see
                // `write_lambda_inline` for the clone/charge semantics.
                write!(factory_buf, "Box {pname} = new Box(ai, {src});").unwrap();
                self.ref_boxes.borrow_mut().insert(p.def);
            } else if !matches!(self.opts.version, leek_syntax::Version::V1)
                && captured_by_nested_lambda_stmts(&body.stmts, p.def)
            {
                // Param captured by an inner lambda → Box-bind (same shape
                // and 1-op ctor charge as upstream's `new Box<>(AI.this, p)`).
                write!(factory_buf, "final Box {pname} = new Box(ai, {src});").unwrap();
                self.ref_boxes.borrow_mut().insert(p.def);
            } else {
                write!(factory_buf, "var {pname} = {src};").unwrap();
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
            ExprKind::Name(NameRef::Local(id)) => note(*id, params, out, seen),
            ExprKind::Literal(_) | ExprKind::Name(_) => {}
            ExprKind::Binary(_, l, r) => {
                expr(l, params, out, seen);
                expr(r, params, out, seen);
            }
            ExprKind::Unary(_, x) | ExprKind::Postfix(_, x) => expr(x, params, out, seen),
            ExprKind::Call(c) => {
                match &c.callee {
                    leek_hir::Callee::Method { receiver, .. } => expr(receiver, params, out, seen),
                    leek_hir::Callee::Expr(e) => expr(e, params, out, seen),
                    leek_hir::Callee::Function(NameRef::Local(id)) => note(*id, params, out, seen),
                    leek_hir::Callee::Function(_) => {}
                }
                for a in &c.args {
                    expr(a, params, out, seen);
                }
            }
            ExprKind::Field(b, ..) => expr(b, params, out, seen),
            ExprKind::Index(b, i) => {
                expr(b, params, out, seen);
                expr(i, params, out, seen);
            }
            ExprKind::Slice(s) => {
                expr(&s.base, params, out, seen);
                if let Some(x) = &s.start {
                    expr(x, params, out, seen);
                }
                if let Some(x) = &s.end {
                    expr(x, params, out, seen);
                }
                if let Some(x) = &s.step {
                    expr(x, params, out, seen);
                }
            }
            ExprKind::Array(items) => {
                for i in items {
                    expr(i, params, out, seen);
                }
            }
            ExprKind::Set(items) => {
                for i in items {
                    expr(&i.start, params, out, seen);
                    if let Some(end) = &i.end {
                        expr(end, params, out, seen);
                    }
                }
            }
            ExprKind::Map(pairs) => {
                for (k, v) in pairs {
                    expr(k, params, out, seen);
                    expr(v, params, out, seen);
                }
            }
            ExprKind::Object(fields) => {
                for (_, v) in fields {
                    expr(v, params, out, seen);
                }
            }
            ExprKind::Ternary(c, t, e_) => {
                expr(c, params, out, seen);
                expr(t, params, out, seen);
                expr(e_, params, out, seen);
            }
            ExprKind::Interval(iv) => {
                if let Some(x) = &iv.start {
                    expr(x, params, out, seen);
                }
                if let Some(x) = &iv.end {
                    expr(x, params, out, seen);
                }
                if let Some(x) = &iv.step {
                    expr(x, params, out, seen);
                }
            }
            ExprKind::Cast(b, _) => expr(b, params, out, seen),
            ExprKind::New(n) => {
                for a in &n.args {
                    expr(a, params, out, seen);
                }
            }
            // Nested lambdas: don't descend — they have their own
            // scope and would produce false positives for nested
            // param shadowing. Their captures are recorded via the
            // outer expression flow above.
            ExprKind::Lambda(_) => {}
        }
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
            _ => {}
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
                // `for (k : v in iter)` — `k` and `v` are locals
                // bound by the loop header, not separate VarDecl
                // statements. Add them so they're not treated as
                // outer captures.
                inner.insert(fe.value.def);
                if let Some(k) = &fe.key {
                    inner.insert(k.def);
                }
                stmt(&fe.body, inner);
            }
            Stmt::Switch(sw) => {
                for a in &sw.arms {
                    for s in &a.body {
                        stmt(s, inner);
                    }
                }
            }
            _ => {}
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
            ExprKind::Name(NameRef::Local(id)) => *id == def,
            ExprKind::Call(c) => {
                let from_callee = match &c.callee {
                    leek_hir::Callee::Function(NameRef::Local(id)) => *id == def,
                    leek_hir::Callee::Method { receiver, .. } => expr(receiver, def),
                    leek_hir::Callee::Expr(e) => expr(e, def),
                    leek_hir::Callee::Function(_) => false,
                };
                from_callee || c.args.iter().any(|a| expr(a, def))
            }
            ExprKind::Binary(_, l, r) => expr(l, def) || expr(r, def),
            ExprKind::Unary(_, x) | ExprKind::Postfix(_, x) => expr(x, def),
            ExprKind::Field(b, ..) => expr(b, def),
            ExprKind::Index(b, i) => expr(b, def) || expr(i, def),
            ExprKind::Ternary(c, t, e_) => expr(c, def) || expr(t, def) || expr(e_, def),
            ExprKind::Array(items) => items.iter().any(|i| expr(i, def)),
            ExprKind::Set(items) => items
                .iter()
                .any(|i| expr(&i.start, def) || i.end.as_ref().is_some_and(|e2| expr(e2, def))),
            ExprKind::Map(pairs) => pairs.iter().any(|(k, v)| expr(k, def) || expr(v, def)),
            ExprKind::Object(fields) => fields.iter().any(|(_, v)| expr(v, def)),
            ExprKind::Cast(b, _) => expr(b, def),
            ExprKind::New(n) => n.args.iter().any(|a| expr(a, def)),
            _ => false,
        }
    }
    fn stmt(s: &Stmt, def: leek_hir::DefId) -> bool {
        match s {
            Stmt::Expr(e) | Stmt::Return(Some(e)) => expr(e, def),
            Stmt::VarDecl(d) => d.init.as_ref().is_some_and(|e| expr(e, def)),
            Stmt::Block(b) => b.stmts.iter().any(|s| stmt(s, def)),
            Stmt::If(i) => {
                expr(&i.cond, def)
                    || stmt(&i.then_branch, def)
                    || i.else_branch.as_deref().is_some_and(|s| stmt(s, def))
            }
            Stmt::While(w) => expr(&w.cond, def) || stmt(&w.body, def),
            Stmt::DoWhile(d) => stmt(&d.body, def) || expr(&d.cond, def),
            Stmt::For(f) => {
                f.init.as_deref().is_some_and(|s| stmt(s, def))
                    || f.cond.as_ref().is_some_and(|e| expr(e, def))
                    || f.step.as_ref().is_some_and(|e| expr(e, def))
                    || stmt(&f.body, def)
            }
            Stmt::Foreach(fe) => expr(&fe.iter, def) || stmt(&fe.body, def),
            Stmt::Switch(sw) => {
                expr(&sw.discriminant, def)
                    || sw.arms.iter().any(|a| {
                        a.case.as_ref().is_some_and(|e| expr(e, def))
                            || a.body.iter().any(|s| stmt(s, def))
                    })
            }
            _ => false,
        }
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
    pub(crate) fn is_captured_local(
        e: &Expr,
        captures: &std::collections::HashSet<leek_hir::DefId>,
    ) -> bool {
        matches!(&e.kind, ExprKind::Name(NameRef::Local(id)) if captures.contains(id))
    }
    fn expr(e: &Expr, captures: &std::collections::HashSet<leek_hir::DefId>) -> bool {
        match &e.kind {
            ExprKind::Binary(op, l, r) if op.is_assignment() => {
                is_captured_local(l, captures) || expr(l, captures) || expr(r, captures)
            }
            ExprKind::Unary(UnaryOp::PreInc | UnaryOp::PreDec, inner)
            | ExprKind::Postfix(PostfixOp::PostInc | PostfixOp::PostDec, inner) => {
                is_captured_local(inner, captures) || expr(inner, captures)
            }
            ExprKind::Binary(_, l, r) => expr(l, captures) || expr(r, captures),
            ExprKind::Unary(_, x) | ExprKind::Postfix(_, x) => expr(x, captures),
            ExprKind::Call(c) => {
                let from_callee = match &c.callee {
                    leek_hir::Callee::Method { receiver, .. } => expr(receiver, captures),
                    leek_hir::Callee::Expr(e) => expr(e, captures),
                    leek_hir::Callee::Function(_) => false,
                };
                from_callee || c.args.iter().any(|a| expr(a, captures))
            }
            ExprKind::Field(b, ..) => expr(b, captures),
            ExprKind::Index(b, i) => expr(b, captures) || expr(i, captures),
            ExprKind::Ternary(c, t, e_) => {
                expr(c, captures) || expr(t, captures) || expr(e_, captures)
            }
            ExprKind::Array(items) => items.iter().any(|i| expr(i, captures)),
            ExprKind::Set(items) => items.iter().any(|i| {
                expr(&i.start, captures) || i.end.as_ref().is_some_and(|e2| expr(e2, captures))
            }),
            ExprKind::Map(pairs) => pairs
                .iter()
                .any(|(k, v)| expr(k, captures) || expr(v, captures)),
            ExprKind::Object(fields) => fields.iter().any(|(_, v)| expr(v, captures)),
            ExprKind::Cast(b, _) => expr(b, captures),
            ExprKind::New(n) => n.args.iter().any(|a| expr(a, captures)),
            // Nested lambdas have their own scope; conservatively
            // ignore (matches the read-walker's policy).
            ExprKind::Lambda(_) => false,
            _ => false,
        }
    }
    fn stmt(s: &Stmt, captures: &std::collections::HashSet<leek_hir::DefId>) -> bool {
        match s {
            Stmt::Expr(e) | Stmt::Return(Some(e)) => expr(e, captures),
            Stmt::VarDecl(d) => d.init.as_ref().is_some_and(|e| expr(e, captures)),
            Stmt::Block(b) => block_walk(b, captures),
            Stmt::If(i) => {
                expr(&i.cond, captures)
                    || stmt(&i.then_branch, captures)
                    || i.else_branch.as_deref().is_some_and(|s| stmt(s, captures))
            }
            Stmt::While(w) => expr(&w.cond, captures) || stmt(&w.body, captures),
            Stmt::DoWhile(d) => stmt(&d.body, captures) || expr(&d.cond, captures),
            Stmt::For(f) => {
                f.init.as_deref().is_some_and(|s| stmt(s, captures))
                    || f.cond.as_ref().is_some_and(|e| expr(e, captures))
                    || f.step.as_ref().is_some_and(|e| expr(e, captures))
                    || stmt(&f.body, captures)
            }
            Stmt::Foreach(fe) => expr(&fe.iter, captures) || stmt(&fe.body, captures),
            Stmt::Switch(sw) => {
                expr(&sw.discriminant, captures)
                    || sw.arms.iter().any(|a| {
                        a.case.as_ref().is_some_and(|e| expr(e, captures))
                            || a.body.iter().any(|s| stmt(s, captures))
                    })
            }
            _ => false,
        }
    }
    fn block_walk(
        b: &leek_hir::Block,
        captures: &std::collections::HashSet<leek_hir::DefId>,
    ) -> bool {
        b.stmts.iter().any(|s| stmt(s, captures))
    }
    block_walk(block, captures)
}

/// True when `def` is referenced anywhere inside `e`, descending through
/// nested lambda bodies (unlike the capture walkers above, which treat a
/// lambda as a leaf).
fn refs_def_deep(e: &Expr, def: leek_hir::DefId) -> bool {
    match &e.kind {
        ExprKind::Name(NameRef::Local(id)) if *id == def => true,
        ExprKind::Lambda(l) => match &l.body {
            leek_hir::LambdaBody::Expr(b) => refs_def_deep(b, def),
            leek_hir::LambdaBody::Block(b) => b.stmts.iter().any(|s| stmt_refs_def_deep(s, def)),
        },
        _ => {
            // `walk_expr_children` doesn't surface a `Callee::Function`
            // name (it's a NameRef, not a child Expr) — check it here so
            // a captured first-class callable param (`a()`) is caught.
            if let ExprKind::Call(c) = &e.kind
                && matches!(&c.callee, leek_hir::Callee::Function(NameRef::Local(id)) if *id == def)
            {
                return true;
            }
            let mut found = false;
            leek_hir::walk_expr_children(e, &mut |c| found = found || refs_def_deep(c, def));
            found
        }
    }
}

fn stmt_refs_def_deep(s: &Stmt, def: leek_hir::DefId) -> bool {
    let mut found = false;
    leek_hir::walk_stmt_child_exprs(s, &mut |e| found = found || refs_def_deep(e, def));
    if !found {
        leek_hir::walk_stmt_child_stmts(s, &mut |c| found = found || stmt_refs_def_deep(c, def));
    }
    found
}

fn captured_in_expr(e: &Expr, def: leek_hir::DefId) -> bool {
    if let ExprKind::Lambda(l) = &e.kind {
        return match &l.body {
            leek_hir::LambdaBody::Expr(b) => refs_def_deep(b, def),
            leek_hir::LambdaBody::Block(b) => b.stmts.iter().any(|s| stmt_refs_def_deep(s, def)),
        };
    }
    let mut found = false;
    leek_hir::walk_expr_children(e, &mut |c| found = found || captured_in_expr(c, def));
    found
}

/// True when a lambda nested anywhere inside `stmts` references `def`
/// (at any lambda-nesting depth). Upstream boxes any function/lambda
/// parameter captured by a nested closure — read *or* write — at the
/// callee's entry (`final var u_a = new Box<>(AI.this, p_a)`); the
/// 2-arg Box ctor charges 1 op per call, and writes through the box
/// propagate into the closure. The emitter mirrors the binding for
/// both value semantics and ops parity.
pub(crate) fn captured_by_nested_lambda_stmts(stmts: &[Stmt], def: leek_hir::DefId) -> bool {
    fn walk(s: &Stmt, def: leek_hir::DefId) -> bool {
        let mut found = false;
        leek_hir::walk_stmt_child_exprs(s, &mut |e| found = found || captured_in_expr(e, def));
        if !found {
            leek_hir::walk_stmt_child_stmts(s, &mut |c| found = found || walk(c, def));
        }
        found
    }
    stmts.iter().any(|s| walk(s, def))
}

/// [`captured_by_nested_lambda_stmts`] for a lambda's own body — its
/// params get the same Box treatment when an inner lambda captures them
/// (`x -> y -> x + 1` boxes `x` at the outer lambda's entry).
pub(crate) fn captured_by_nested_lambda_body(body: &LambdaBody, def: leek_hir::DefId) -> bool {
    match body {
        LambdaBody::Expr(e) => captured_in_expr(e, def),
        LambdaBody::Block(b) => captured_by_nested_lambda_stmts(&b.stmts, def),
    }
}

/// Compute the file-wide set of VarDecl-declared locals that must be heap-boxed
/// because a *directly-nested* lambda captures **and writes** them. LeekScript
/// closures capture by reference, so a write inside the lambda must be visible
/// in the enclosing scope; Java's effectively-final rule forbids that for a
/// plain captured local, so we share a one-element `Object[]` instead.
///
/// Deliberately scoped to keep this safe:
/// - only **first-level** lambdas are inspected (we don't descend into nested
///   lambda bodies — threading a box through two factory levels is a separate
///   problem, so nested captured-writes stay on the null-stub fallback), and
/// - only **VarDecl** locals are boxable (a captured-written *parameter* or
///   foreach binding isn't a `var` declaration we can rewrite, so it also stays
///   on the fallback).
///
/// `DefId`s are unique across the whole HIR file, so one set serves every
/// function/method/main body.
pub(crate) fn collect_boxed_locals(
    hir: &leek_hir::HirFile,
    out: &mut std::collections::HashSet<leek_hir::DefId>,
) {
    use leek_hir::Def;
    let mut var_decls = std::collections::HashSet::new();
    let mut captured_written = std::collections::HashSet::new();
    for def in &hir.defs {
        match def {
            Def::Function(f) => {
                if let Some(b) = &f.body {
                    scan_stmts(&b.stmts, &mut var_decls, &mut captured_written);
                }
            }
            Def::Class(c) => {
                for m in c.methods.iter().chain(c.constructors.iter()) {
                    if let Some(b) = &m.body {
                        scan_stmts(&b.stmts, &mut var_decls, &mut captured_written);
                    }
                }
            }
            _ => {}
        }
    }
    scan_stmts(&hir.main, &mut var_decls, &mut captured_written);
    // A boxable local is one that is both a `var` declaration and is
    // captured-and-written by some first-level lambda.
    out.extend(
        captured_written
            .into_iter()
            .filter(|d| var_decls.contains(d)),
    );
}

fn scan_stmts(
    stmts: &[Stmt],
    var_decls: &mut std::collections::HashSet<leek_hir::DefId>,
    captured_written: &mut std::collections::HashSet<leek_hir::DefId>,
) {
    for s in stmts {
        scan_stmt(s, var_decls, captured_written);
    }
}

fn scan_stmt(
    s: &Stmt,
    var_decls: &mut std::collections::HashSet<leek_hir::DefId>,
    captured_written: &mut std::collections::HashSet<leek_hir::DefId>,
) {
    if let Stmt::VarDecl(v) = s {
        var_decls.insert(v.def);
    }
    // First-level lambdas in this statement's immediate expressions.
    leek_hir::walk_stmt_child_exprs(s, &mut |e| {
        find_first_level_lambda_writes(e, captured_written);
    });
    // Recurse into child statements (control-flow bodies). Lambdas are
    // expressions, not statements, so this never descends into a lambda body.
    leek_hir::walk_stmt_child_stmts(s, &mut |child| {
        scan_stmt(child, var_decls, captured_written);
    });
}

/// Find lambdas in `e` without descending into a lambda's own body, recording
/// each first-level lambda's captured-and-written outer locals.
fn find_first_level_lambda_writes(
    e: &Expr,
    captured_written: &mut std::collections::HashSet<leek_hir::DefId>,
) {
    if let ExprKind::Lambda(l) = &e.kind {
        if let LambdaBody::Block(b) = &l.body {
            let mut inner: std::collections::HashSet<_> = l.params.iter().map(|p| p.def).collect();
            collect_inner_decls(b, &mut inner);
            for c in lambda_outer_captures(b, &inner) {
                let one = std::iter::once(c).collect();
                if lambda_writes_to_outer(b, &one) {
                    captured_written.insert(c);
                }
            }
        }
        // Do not descend into the lambda body — nested captured-writes are
        // intentionally left on the null-stub path.
        return;
    }
    leek_hir::walk_expr_children(e, &mut |child| {
        find_first_level_lambda_writes(child, captured_written);
    });
}
