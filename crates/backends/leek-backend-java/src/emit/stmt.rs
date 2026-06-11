use leek_hir::{
    Callee, DoWhileStmt, Expr, ExprKind, ForStmt, ForeachStmt, IfStmt, Literal, NameRef, Stmt,
    VarDecl, WhileStmt,
};

use super::lambda::captured_by_nested_lambda_stmts;
use super::traits::EmitStmt;
use super::{Emitter, JavaWriter, is_pure_value_expr, is_terminator, is_valid_statement_expr};
use crate::mangle;

impl EmitStmt for Emitter<'_> {
    fn emit_stmt(&mut self, s: &Stmt) {
        // Exact mode folds the per-statement op tick into the
        // value-producing expression via the `ops(value, n)`
        // overload (see `emit_var_decl` / `Stmt::Return`). The
        // standalone `ops(1);` form is reserved for control-flow
        // entry points the reference also instruments that way
        // (the `if` body prologue is the main one). Clean mode
        // skips emit_ops entirely and relies on `Stmt::Charge`.
        match s {
            Stmt::Expr(e) => {
                // `LeekExpressionInstruction.writeJavaCode`:
                //   - pure value reads in non-trailing position are
                //     dead code (`null;`, `a;`) — skipped entirely
                //   - call / assignment / ++ -- can stand alone as
                //     Java statements; if they have op cost we wrap
                //     in `ops(EXPR, n)`, otherwise emit bare
                //   - everything else (`12 && 5`, ternary, etc.)
                //     isn't a valid Java statement expression, so
                //     the reference wraps it in `nothing(...)` —
                //     a no-op AI method that turns the expression
                //     into a call-statement.
                if is_pure_value_expr(e) {
                    return;
                }
                let line = self.line_of(e.span);
                let code = self.expr_to_string(e);
                let cost = if self.opts.emit_ops {
                    self.emit_cost(e)
                } else {
                    0
                };
                let rendered = if cost > 0 {
                    format!("ops({code}, {cost})")
                } else if is_valid_statement_expr(e) {
                    code
                } else {
                    format!("nothing({code})")
                };
                self.writer.add_line_at(&format!("{rendered};"), line);
            }
            Stmt::VarDecl(v) => self.emit_var_decl(v),
            Stmt::Return(None) => self.writer.add_line("return null;"),
            Stmt::Return(Some(e)) => {
                // Mirror `LeekReturnInstruction.writeJavaCode`:
                // when the return expression has non-zero op cost,
                // prepend `ops(N); ` (with trailing space) before
                // the `return EXPR;`. The per-statement baseline
                // tick `ops(1);` is emitted separately by the
                // surrounding block-entry path (`emit_body`) — not
                // by the return itself.
                // Upstream NEVER copies on return (`LeekReturnInstruction`):
                // a v1 function returns its boxed local bare (`return u_a;`)
                // and the *caller* clones when storing the result into a new
                // Box (the 2-arg ctor's `instanceof Box` path). Cloning here
                // over-charged every `return <local array>` by a full
                // 1+2·size clone (OPS_DRIFT L2442/L2986/L3153/L3180). Our
                // plain locals return raw values; the callee's local dies at
                // return, so the caller aliasing it is unobservable unless
                // the callee retained another live reference (global /
                // capture) — a pattern the parity suite would surface.
                let code = self.expr_to_string(e);
                let line = self.line_of(e.span);
                if self.opts.emit_ops {
                    let cost = self.emit_cost(e);
                    if cost > 0 {
                        self.writer
                            .add_line_at(&format!("ops({cost}); return {code};"), line);
                    } else {
                        self.writer.add_line_at(&format!("return {code};"), line);
                    }
                } else {
                    self.writer.add_line_at(&format!("return {code};"), line);
                }
            }
            Stmt::If(i) => self.emit_if(i),
            Stmt::While(w) => self.emit_while(w),
            Stmt::DoWhile(d) => self.emit_do_while(d),
            Stmt::For(f) => self.emit_for(f),
            Stmt::Foreach(fe) => self.emit_foreach(fe),
            Stmt::Break(_) => {
                // Upstream `LeekBreakInstruction.writeJavaCode`
                // prepends `addCounter(1)`; we mirror so loops with
                // an early break match the expected op count.
                if self.opts.emit_ops {
                    self.writer.add_code("ops(1);");
                }
                self.writer.add_line("break;");
            }
            Stmt::Continue(_) => {
                if self.opts.emit_ops {
                    self.writer.add_code("ops(1);");
                }
                self.writer.add_line("continue;");
            }
            Stmt::Block(b) => {
                self.writer.add_line("{");
                self.writer.push_indent();
                self.emit_stmts(&b.stmts);
                self.writer.pop_indent();
                self.writer.add_line("}");
            }
            Stmt::Switch(sw) => self.emit_switch(sw),
            Stmt::Include(_) => {
                // Includes are inlined at the parser stage; the HIR
                // placeholder is purely for source-map round-trip.
            }
            Stmt::Import(_) => {
                // Imports are resolver-time metadata only.
            }
            Stmt::Charge(n) => {
                // Always emitted: backends choosing the pass want a
                // single `ops(n)` at the top of each block in lieu of
                // the per-stmt ticks.
                self.writer.add_line(&format!("ops({n});"));
            }
        }
    }
}

impl Emitter<'_> {
    pub(crate) fn emit_stmts(&mut self, stmts: &[Stmt]) {
        // Clean-mode dead-code-elim: drop everything past a definite
        // terminator at this nesting level — a `break`/`continue`/`return`, or
        // any statement that always returns / never falls through (e.g. an
        // infinite `while (true)`). Otherwise javac rejects the trailing code as
        // unreachable.
        let cutoff = if self.opts.dead_code_elim {
            stmts
                .iter()
                .position(|s| {
                    is_terminator(s) || super::stmt_definitely_returns(s, self.opts.emit_ops)
                })
                .map_or(stmts.len(), |p| p + 1)
        } else {
            stmts.len()
        };
        for s in &stmts[..cutoff] {
            self.emit_stmt(s);
        }
    }

    pub(crate) fn emit_var_decl(&mut self, v: &VarDecl) {
        let line = self.line_of(v.span);
        // Mark the var being initialized so a recursive lambda inside
        // `init` (e.g. `var fact = function(x) { … fact(x-1) … }`)
        // doesn't try to capture itself — that would emit
        // `__anon_N(u_fact)` where `u_fact` is not yet initialized
        // and javac rejects with "might not have been initialized".
        let prev = self.initializing_def.replace(Some(v.def));
        let init = if let Some(e) = &v.init {
            // A synthetic default-param binding (spliced by
            // `emit_default_overload`) charges only its default expression at
            // v2+ — upstream emits `Object u_x = 5l; ops(0);`, no declaration
            // tick. v1 keeps the +1 (the Box ctor's runtime charge).
            let base = if self.synthetic_default_decls.contains(&v.def)
                && !matches!(self.opts.version, leek_syntax::Version::V1)
            {
                0
            } else {
                1
            };
            let raw = self.v1_clone_with_ops(e, base);
            // A statically-typed scalar local coerces its initializer to the
            // declared type, mirroring upstream `compileConvert`: the runtime
            // is type-erased, so `integer b = a[1]` (real array) must store an
            // integer and `real b = 5` a double. `longint`/`real`/`bool` all
            // take `Object`, so the untyped HIR value drops straight in.
            Self::coerce_decl(v.ty.as_ref(), raw)
        } else {
            // Uninitialized decl still costs 1 op upstream — match
            // the `LeekVariableDeclarationInstruction.writeJavaCode`
            // baseline tick. A typed scalar defaults to its zero value
            // (`0l`/`0.0`/`false`) rather than `null` — upstream never
            // leaves a declared `integer` holding null.
            let base = Self::decl_default(v.ty.as_ref());
            if self.opts.emit_ops {
                format!("ops({base}, 1)")
            } else {
                base
            }
        };
        self.initializing_def.set(prev);
        if v.is_global {
            let name = mangle::global(self.opts, &v.name);
            self.writer.add_line_at(&format!("{name} = {init};"), line);
            self.writer.add_line(&format!("g_init_{} = true;", v.name));
        } else {
            let name = mangle::local(self.opts, &v.name);
            if self.ref_boxes.borrow().contains(&v.def) {
                // Passed to a `@` param somewhere → store in a runtime `Box` so
                // the callee can alias and mutate it. Reads emit `.get()`,
                // writes route through `Box` methods (see `write_name` /
                // `write_assignment`); the call site passes the box for `@` args.
                // The 2-arg Box ctor charges the decl's 1 op at runtime, so no
                // `ops(init, 1)` wrapper here — a double charge otherwise. A
                // non-zero init expression cost rides the 3-arg ctor (upstream:
                // `new Box<Object>(AI.this, <init>, n)` charges 1+n).
                let prev = self.initializing_def.replace(Some(v.def));
                let (inner, cost) = match &v.init {
                    Some(e) => (
                        Self::coerce_decl(v.ty.as_ref(), self.v1_store_clone(e)),
                        self.emit_cost(e),
                    ),
                    None => (Self::decl_default(v.ty.as_ref()), 0),
                };
                self.initializing_def.set(prev);
                let ai = self.ai_this();
                let ctor = if self.opts.emit_ops && cost > 0 {
                    format!("new Box({ai}, {inner}, {cost})")
                } else {
                    format!("new Box({ai}, {inner})")
                };
                self.writer
                    .add_line_at(&format!("Box {name} = {ctor};"), line);
            } else if self.boxed_locals.borrow().contains(&v.def) {
                // Heap-box: a nested lambda captures-and-writes this local, so
                // it's shared through a one-element `Object[]`. Reads/writes
                // elsewhere go via `[0]` (see `write_name`).
                self.writer
                    .add_line_at(&format!("Object[] {name} = new Object[]{{{init}}};"), line);
            } else {
                // `Object` is the safe declaration type for arbitrary
                // Leek locals. Narrowing to primitive `long`/`double`
                // is a clean-mode optimization we don't yet apply.
                self.writer
                    .add_line_at(&format!("Object {name} = {init};"), line);
            }
        }
    }
    /// Render an expression, deep-copying it at **v1** when it's a *load* of an
    /// existing value (a variable / field / index read). v1 has value
    /// semantics: `var b = a; mutate(b)` must not alias `a`, so the load is
    /// wrapped in `copy(...)` (mirrors upstream `JavaWriter.compileClone`). A
    /// fresh value — literal, arithmetic, call, `new`, collection literal — is
    /// already unaliased and isn't copied. No-op at v2+ (reference semantics).
    /// Coerce an emitted value to a declared **scalar** type
    /// (`integer`/`real`/`boolean`) via `longint`/`real`/`bool`. Composite
    /// declared types (`Array<…>`, `Map<…>`, `Object`, untyped) pass through —
    /// their element coercion happens at the store site, not here.
    pub(crate) fn coerce_decl(ty: Option<&leek_hir::Type>, inner: String) -> String {
        use leek_hir::Type;
        match ty {
            Some(Type::Integer | Type::Real | Type::Boolean) => {
                super::coerce_field_write(Some(super::java_type_for(ty)), &inner)
            }
            // `real? a = 12` stores `12.0`, but `real? a = null` must stay null —
            // so use the null-tolerant `longintOrNull`/`realOrNull` converters
            // (a plain `longint(null)` would throw).
            Some(Type::Nullable(inner_ty)) => match inner_ty.as_ref() {
                Type::Integer => format!("longintOrNull({inner})"),
                Type::Real => format!("realOrNull({inner})"),
                _ => inner,
            },
            _ => inner,
        }
    }

    /// Default Java value for an uninitialized declared local: the zero of a
    /// scalar type, else `null`.
    fn decl_default(ty: Option<&leek_hir::Type>) -> String {
        use leek_hir::Type;
        match ty {
            Some(Type::Integer) => "0l".into(),
            Some(Type::Real) => "0.0".into(),
            Some(Type::Boolean) => "false".into(),
            _ => "null".into(),
        }
    }

    pub(crate) fn v1_clone(&self, e: &Expr) -> String {
        let inner = self.expr_to_string(e);
        if matches!(self.opts.version, leek_syntax::Version::V1)
            && matches!(
                &e.kind,
                ExprKind::Name(NameRef::Local(_) | NameRef::Global(_))
                    | ExprKind::Field(..)
                    | ExprKind::Index(..)
            )
        {
            format!("copy({inner})")
        } else {
            inner
        }
    }

    /// [`Self::v1_clone`] plus the v1 *store-site* clone for calls. Upstream
    /// boxes every v1 local, so a callee whose body `return`s a plain variable
    /// hands its caller a `Box`; `var x = f()` compiles upstream to
    /// `new Box(ai, f())` whose 2-arg ctor clones Box inputs. We return raw
    /// values instead, so mirror that dynamic clone here: wrap the call in
    /// `copy(...)` when the callee is in the `v1_box_returners` set.
    /// Charge-identical (`copy` = upstream `LeekOperations.clone`, free for
    /// scalars) and breaks the alias the caller must not observe
    /// (OPS_DRIFT L2958/L2990/L3000/L3172/L3611).
    pub(crate) fn v1_store_clone(&self, e: &Expr) -> String {
        let inner = self.v1_clone(e);
        if matches!(self.opts.version, leek_syntax::Version::V1) && self.call_returns_box(e) {
            return format!("copy({inner})");
        }
        inner
    }

    /// Whether `e` is a call whose result is a `Box` upstream (callee returns
    /// a plain variable, directly or transitively). See [`super::v1_box_returners`].
    fn call_returns_box(&self, e: &Expr) -> bool {
        if let ExprKind::Call(c) = &e.kind {
            match &c.callee {
                Callee::Function(NameRef::Function(fid)) => self.returns_box_fns.contains(&fid.0),
                Callee::Function(NameRef::Local(id)) => self.returns_box_vars.contains(&id.0),
                _ => false,
            }
        } else {
            false
        }
    }

    /// [`Self::v1_store_clone`] wrapped in the `ops(value, n)` op-count
    /// overload (used for a `var x = <init>` initializer).
    fn v1_clone_with_ops(&self, e: &Expr, base_cost: u32) -> String {
        let inner = self.v1_store_clone(e);
        if !self.opts.emit_ops {
            return inner;
        }
        format!("ops({inner}, {})", base_cost + self.emit_cost(e))
    }

    /// Wrap an expression in the `ops(value, n)` overload when we're
    /// in op-counting mode; otherwise return the bare expression.
    /// `n` is the static cost contributed by this statement (the
    /// reference computes it precisely from the operator/literal
    /// graph — we approximate with a small additive scheme below).
    pub(crate) fn expr_with_ops(&self, e: &Expr, base_cost: u32) -> String {
        let inner = self.expr_to_string(e);
        if !self.opts.emit_ops {
            return inner;
        }
        let n = base_cost + self.emit_cost(e);
        format!("ops({inner}, {n})")
    }
    pub(crate) fn emit_if(&mut self, i: &IfStmt) {
        // `if (ops(cond, 1+cond_cost)) {` — the +1 inside the wrapper
        // is the if-statement's per-stmt baseline (added by
        // `ConditionalBloc.analyze`'s `mCondition.operations++`).
        // Body is flush-left; `else` lives on its own line.
        // A soft return (`return? x`) compiles its truthiness check without the
        // per-`if` op tick (the reference uses a bare `if (bool(r)) return r;`),
        // so the condition is emitted unwrapped.
        let cond = if i.soft {
            self.expr_to_bool(&i.cond)
        } else {
            self.if_cond_string(&i.cond)
        };
        self.writer.add_line(&format!("if ({cond}) {{"));
        if self.opts.is_clean() {
            self.writer.push_indent();
        }
        self.emit_stmt_or_block(&i.then_branch);
        if self.opts.is_clean() {
            self.writer.pop_indent();
        }
        if let Some(e) = &i.else_branch {
            // Special-case `else if` chains to keep the output flat.
            if let Stmt::If(_) = e.as_ref() {
                self.writer.add_line("}");
                self.writer.add_code("else ");
                self.emit_stmt(e);
                return;
            }
            if self.opts.is_clean() {
                self.writer.add_line("} else {");
            } else {
                self.writer.add_line("}");
                self.writer.add_line("else {");
            }
            if self.opts.is_clean() {
                self.writer.push_indent();
            }
            self.emit_stmt_or_block(e);
            if self.opts.is_clean() {
                self.writer.pop_indent();
            }
        }
        self.writer.add_line("}");
    }

    /// Render the condition of an `if` for emission inside `if (...)`.
    /// In exact mode, the condition is wrapped in the `ops(value, n)`
    /// overload to fold a static cost into the branch entry.
    pub(crate) fn if_cond_string(&self, cond: &Expr) -> String {
        let bool_form = self.expr_to_bool(cond);
        if !self.opts.emit_ops {
            return bool_form;
        }
        // Add 1 for the branch entry on top of the condition cost.
        let n = 1 + self.emit_cost(cond);
        format!("ops({bool_form}, {n})")
    }

    pub(crate) fn emit_while(&mut self, w: &WhileStmt) {
        let cond = self.loop_cond_string(&w.cond);
        self.writer.add_line(&format!("while ({cond}) {{"));
        if self.opts.is_clean() {
            self.writer.push_indent();
        }
        self.emit_body_with_entry_tick(&w.body);
        if self.opts.is_clean() {
            self.writer.pop_indent();
        }
        self.writer.add_line("}");
    }

    pub(crate) fn emit_do_while(&mut self, d: &DoWhileStmt) {
        self.writer.add_line("do {");
        if self.opts.is_clean() {
            self.writer.push_indent();
        }
        self.emit_body_with_entry_tick(&d.body);
        if self.opts.is_clean() {
            self.writer.pop_indent();
        }
        let cond = self.loop_cond_string(&d.cond);
        self.writer.add_line(&format!("}} while ({cond});"));
    }

    /// Render a while/do-while condition. Reference always wraps in
    /// `ops(cond, cost)`; cost = expr-cost only, no per-stmt baseline.
    pub(crate) fn loop_cond_string(&self, c: &Expr) -> String {
        let inner = self.expr_to_bool(c);
        if !self.opts.emit_ops {
            return inner;
        }
        // Match the reference `WhileBlock`: a boolean *literal* condition is
        // wrapped in `bool(...)` ("Prevent unreachable code error") so javac
        // can't fold `while (ops(true, 0))` to a provably-infinite loop and then
        // reject the method's trailing `return null;` as `missing return`.
        let inner = if matches!(&c.kind, ExprKind::Literal(Literal::Bool(_))) {
            format!("bool({inner})")
        } else {
            inner
        };
        let cost = self.emit_cost(c);
        format!("ops({inner}, {cost})")
    }

    pub(crate) fn emit_for(&mut self, f: &ForStmt) {
        // Render init/cond/step in the C-for header. The reference's
        // shape: `for (Object u_i = ops(0l, 1);\n ops(less(u_i, 10l), 1); ops(u_i = ..., 2)) {`
        // — note the newline after init, which we preserve via
        // `add_code` so we land on the same "Java line" the
        // reference does for `.lines` mapping purposes.
        let init = self.for_init_string(f.init.as_deref());
        let cond = match &f.cond {
            // For-cond gets no per-statement baseline — only the
            // expression cost, and only if it's non-zero. Matches
            // `LeekExpressionInstruction.writeJavaCode`'s for-header
            // emit path.
            Some(c) => self.for_cond_string(c),
            None => "true".into(),
        };
        let step = match &f.step {
            Some(s) => {
                let raw = self.expr_to_string(s);
                if self.opts.emit_ops {
                    // `ForBlock.writeJavaCode` always wraps the step
                    // in `ops(STEP, step.getOperations())` — same as
                    // the cond, no extra per-statement baseline.
                    let n = self.emit_cost(s);
                    format!("ops({raw}, {n})")
                } else {
                    raw
                }
            }
            None => String::new(),
        };
        self.writer.add_code(&format!("for ({init};\n"));
        // The reference newline-breaks before the cond; mirror that.
        self.writer.add_line(&format!("{cond}; {step}) {{"));
        if self.opts.is_clean() {
            self.writer.push_indent();
        }
        self.emit_body_with_entry_tick(&f.body);
        if self.opts.is_clean() {
            self.writer.pop_indent();
        }
        self.writer.add_line("}");
    }

    /// Render a for-loop condition. Reference always wraps it in the
    /// `ops(EXPR, cost)` overload — same shape as while-cond, with
    /// no per-statement baseline added on top.
    pub(crate) fn for_cond_string(&self, c: &Expr) -> String {
        self.loop_cond_string(c)
    }

    /// Render the init clause of a C-for header without the
    /// trailing semicolon (the caller writes it). Either a var
    /// declaration or a bare expression.
    pub(crate) fn for_init_string(&self, init: Option<&Stmt>) -> String {
        match init {
            Some(Stmt::VarDecl(v)) if !v.is_global => {
                let name = mangle::local(self.opts, &v.name);
                let init_str = match &v.init {
                    Some(e) => self.expr_with_ops(e, 1),
                    None => "null".into(),
                };
                // A loop variable passed to a `@` param binds through a runtime
                // `Box` like any other local (upstream declares it in the
                // for-header: `for (var u_i = new Box<Object>(AI.this, 0l); …)`);
                // the ctor charges the decl's 1 op, so no `ops(init, 1)` wrapper.
                if self.ref_boxes.borrow().contains(&v.def) {
                    let (inner, cost) = match &v.init {
                        Some(e) => (self.v1_store_clone(e), self.emit_cost(e)),
                        None => ("null".into(), 0),
                    };
                    let ai = self.ai_this();
                    if self.opts.emit_ops && cost > 0 {
                        format!("Box {name} = new Box({ai}, {inner}, {cost})")
                    } else {
                        format!("Box {name} = new Box({ai}, {inner})")
                    }
                } else if self.boxed_locals.borrow().contains(&v.def) {
                    // Box the loop variable too if a nested lambda captures-and-
                    // writes it, so its declaration matches the `[0]` accesses
                    // `write_name` emits elsewhere (consistency with `emit_var_decl`).
                    format!("Object[] {name} = new Object[]{{{init_str}}}")
                } else {
                    format!("Object {name} = {init_str}")
                }
            }
            // A reused-variable init (`for (i = 0; …)`) is a bare expression: the
            // reference's for-header charges only its `getOperations()` (the
            // assignment's own cost), with no per-statement baseline — unlike the
            // `var i = …` declaration init above, which keeps the +1 decl tick.
            Some(Stmt::Expr(e)) => self.expr_with_ops(e, 0),
            _ => String::new(),
        }
    }

    pub(crate) fn emit_foreach(&mut self, fe: &ForeachStmt) {
        // Reference shape (`ForeachBlock.writeJavaCode`):
        //   final var ar0 = ops(<iter>, 0);
        //   if (isIterable(ar0)) {
        //     Object u_x = null;
        //     ops(1);
        //     var i0 = iterator(ar0);
        //     while (i0.hasNext()) {
        //       var v0 = i0.next();
        //       u_x = (Object) v0.getValue();
        //       ops(1);
        //       <body>
        //     }
        //   }
        // Key/value form adds `Object u_k = null` and assigns
        // `v0.getKey()` before the value. We use stable `__i`/`__v`/
        // `__ar` suffixes since the reference's `ar0`/`i0`/`v0`
        // numbering tracks block-nesting state we don't reproduce.
        let iter_code = self.expr_to_string(&fe.iter);
        let iter_cost = self.emit_cost(&fe.iter);
        let value = mangle::local(self.opts, &fe.value.name);
        let key_decl = fe.key.as_ref().map(|k| mangle::local(self.opts, &k.name));
        let suffix = self.next_iter_id();
        let ar = format!("__ar{suffix}");
        let it = format!("__i{suffix}");
        let entry = format!("__v{suffix}");
        // Stash the iterable in a final temp so the `ops(...)` count
        // is paid once. Wrap with `ops(<iter>, iter_cost)` to mirror
        // upstream's `ForeachBlock.writeJavaCode` line 140.
        if self.opts.emit_ops {
            self.writer
                .add_line(&format!("final var {ar} = ops({iter_code}, {iter_cost});"));
        } else {
            self.writer
                .add_line(&format!("final var {ar} = {iter_code};"));
        }
        // The key:value form (`ForeachKeyBlock`) emits one tick BEFORE
        // the `if (isIterable…)` (upstream: `addCounter(1)` precedes the
        // `addLine(sb)` holding the whole construct); the value-only
        // form (`ForeachBlock`) ticks inside the `if` instead.
        if self.opts.emit_ops && fe.key.is_some() {
            self.writer.add_code("ops(1);");
        }
        self.writer.add_line(&format!("if (isIterable({ar})) {{"));
        // A binding captured by a nested lambda in the body binds to a
        // runtime `Box` (upstream: `final Wrapper<Object> u_x = new
        // Wrapper<Object>(new Box(ai, null));` — we use a bare `final Box`,
        // which has the same mutator surface, is charge-identical (the
        // Wrapper's 1-arg ctor is free; the inner 2-arg Box ctor charges
        // the slot's 1 op either way), and matches the `final Box` factory
        // params of outlined lambdas). The anonymous class captures the
        // final Box directly; reads go through `.get()` and writes through
        // the Box mutators (routed via `ref_boxes`). The runtime ctor
        // charge replaces the slot's share of the static setup charge
        // below.
        let body_slice = std::slice::from_ref(&*fe.body);
        let key_captured = fe
            .key
            .as_ref()
            .is_some_and(|k| k.is_new && captured_by_nested_lambda_stmts(body_slice, k.def));
        let value_captured =
            fe.value.is_new && captured_by_nested_lambda_stmts(body_slice, fe.value.def);
        // Only declare the binding when the foreach actually
        // introduces a new variable (`for (var x in …)` vs.
        // `for (x in …)` reusing an outer slot). Without this we'd
        // duplicate-declare any outer var the foreach reuses.
        if let Some(k) = &key_decl
            && fe.key.as_ref().is_none_or(|x| x.is_new)
        {
            if key_captured {
                let ai = self.ai_this();
                self.writer
                    .add_line(&format!("final Box {k} = new Box({ai}, null);"));
                if let Some(kb) = &fe.key {
                    self.ref_boxes.borrow_mut().insert(kb.def);
                }
            } else {
                self.writer.add_line(&format!("Object {k} = null;"));
            }
        }
        if fe.value.is_new {
            if value_captured {
                let ai = self.ai_this();
                self.writer
                    .add_line(&format!("final Box {value} = new Box({ai}, null);"));
                self.ref_boxes.borrow_mut().insert(fe.value.def);
            } else {
                self.writer.add_line(&format!("Object {value} = null;"));
            }
        }
        if self.opts.emit_ops {
            // Setup charge. Upstream totals are capture-independent
            // because every variant costs 1 per slot at runtime when
            // not statically ticked: v2+ declared → static `ops(1)`;
            // v1 declared and captured-at-any-version → `new
            // Box(ai, null)` / `Wrapper(new Box(ai, null))`, whose
            // 2-arg Box ctor charges 1. We emit neither Box nor
            // Wrapper, so charge statically instead.
            if fe.key.is_some() {
                // ForeachKeyBlock: 1 per *declared* slot; a reused
                // outer slot (`for (k : v in …)`) charges nothing, and
                // a captured slot pays its 1 op via the Box ctor above.
                let n = u32::from(fe.key.as_ref().is_some_and(|k| k.is_new) && !key_captured)
                    + u32::from(fe.value.is_new && !value_captured);
                if n > 0 {
                    self.writer.add_code(&format!("ops({n});"));
                }
            } else if !value_captured {
                // ForeachBlock: declared or reused both total 1
                // (captured → Box ctor charges instead).
                self.writer.add_code("ops(1);");
            }
        }
        self.writer.add_line(&format!("var {it} = iterator({ar});"));
        self.writer.add_line(&format!("while ({it}.hasNext()) {{"));
        self.writer.add_line(&format!("var {entry} = {it}.next();"));
        if let Some(k) = &key_decl {
            if key_captured {
                self.writer.add_line(&format!("{k}.set({entry}.getKey());"));
            } else {
                self.writer
                    .add_line(&format!("{k} = (Object) {entry}.getKey();"));
            }
        }
        if value_captured {
            self.writer
                .add_line(&format!("{value}.set({entry}.getValue());"));
        } else {
            self.writer
                .add_line(&format!("{value} = (Object) {entry}.getValue();"));
        }
        // Per-iteration tick. Value-only (`ForeachBlock`): one
        // unconditional `addCounter(1)` per iteration, plus at v1 a
        // second counter for a by-value iterator (the `set(...)` copy
        // path); a `@`-by-ref iterator is set through its `Box` and
        // pays only the single counter. Key:value (`ForeachKeyBlock`):
        // NO unconditional tick — v2+ charges nothing per iteration,
        // v1 charges 1 per non-`@ref` slot (the copy-on-set path).
        if self.opts.emit_ops {
            if fe.key.is_some() {
                if matches!(self.opts.version, leek_syntax::Version::V1) {
                    let n = u32::from(!fe.key.as_ref().is_some_and(|k| k.is_by_ref))
                        + u32::from(!fe.value.is_by_ref);
                    if n > 0 {
                        self.writer.add_code(&format!("ops({n});"));
                    }
                }
            } else if matches!(self.opts.version, leek_syntax::Version::V1) && !fe.value.is_by_ref {
                self.writer.add_code("ops(1);ops(1);");
            } else {
                self.writer.add_code("ops(1);");
            }
        }
        self.emit_stmt_or_block(&fe.body);
        self.writer.add_line("}");
        self.writer.add_line("}");
    }

    /// Monotonically increasing id for foreach-temp names so nested
    /// loops don't shadow each other. Resets per emit.
    pub(crate) fn next_iter_id(&mut self) -> u32 {
        self.iter_counter += 1;
        self.iter_counter
    }

    /// Render a block body to a String by spinning up a scratch
    /// `JavaWriter`, running the statement emitter on it, and
    /// pulling the text out. Used to inline block-bodied lambdas
    /// (which need full statement emission) inside an expression
    /// context that otherwise only sees `&self`. Inherits the
    /// `in_function`/`iter_counter` state from the parent.
    pub(crate) fn render_block_to_string(&self, b: &leek_hir::Block) -> String {
        let mut scratch = Emitter {
            opts: self.opts,
            hir: self.hir,
            writer: JavaWriter::new(),
            in_function: true,
            iter_counter: self.iter_counter,
            lambda_depth: std::cell::Cell::new(self.lambda_depth.get()),
            outlined: std::cell::RefCell::new(Vec::new()),
            fn_singletons: std::cell::RefCell::new(std::collections::BTreeMap::new()),
            in_outlined: std::cell::Cell::new(self.in_outlined.get()),
            ref_boxes: std::cell::RefCell::new(self.ref_boxes.borrow().clone()),
            outline_counter: std::cell::Cell::new(self.outline_counter.get()),
            initializing_def: std::cell::Cell::new(self.initializing_def.get()),
            self_rec_def: std::cell::Cell::new(self.self_rec_def.get()),
            shadowed_builtins: std::cell::RefCell::new(self.shadowed_builtins.borrow().clone()),
            boxed_locals: std::cell::RefCell::new(self.boxed_locals.borrow().clone()),
            current_class: std::cell::Cell::new(self.current_class.get()),
            var_ref_positions: self.var_ref_positions.clone(),
            returns_box_fns: self.returns_box_fns.clone(),
            returns_box_vars: self.returns_box_vars.clone(),
            synthetic_default_decls: self.synthetic_default_decls.clone(),
        };
        scratch.emit_stmts(&b.stmts);
        // Hand off any outlined-lambda helpers the scratch run
        // synthesized to the parent, and advance our counter so
        // subsequent outlines don't collide.
        self.outline_counter.set(scratch.outline_counter.get());
        self.outlined
            .borrow_mut()
            .append(&mut *scratch.outlined.borrow_mut());
        self.fn_singletons
            .borrow_mut()
            .append(&mut *scratch.fn_singletons.borrow_mut());
        let (java, _) = scratch.writer.into_parts();
        java
    }
    pub(crate) fn emit_stmt_or_block(&mut self, s: &Stmt) {
        if let Stmt::Block(b) = s {
            self.emit_stmts(&b.stmts);
        } else {
            self.emit_stmt(s);
        }
    }

    /// Emit a body whose first line begins with the `ops(1);` body-
    /// entry baseline tick. This mirrors `JavaWriter.addCounter(1)`
    /// in the reference: it's `addCode`, not `addLine`, so the tick
    /// is concatenated onto whatever the first statement emits next.
    /// Used for while/for/do-while/foreach/function bodies but NOT
    /// for if/else bodies or the main runIA block.
    pub(crate) fn emit_body_with_entry_tick(&mut self, s: &Stmt) {
        if self.opts.emit_ops {
            self.writer.add_code("ops(1);");
        }
        if let Stmt::Block(b) = s {
            self.emit_stmts(&b.stmts);
        } else {
            self.emit_stmt(s);
        }
    }
}
