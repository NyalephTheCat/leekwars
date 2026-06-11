//! Call emission: user calls, builtin dispatch, default arguments, and the by-ref cell-argument plumbing.

use super::{
    Callee, Const, DefId, DefaultArg, FloatCC, InstBuilder, IntCC, LocalId, MathSig, MirFunction,
    NativeError, Operand, Place, Tx, ValTy, Value, byref_cell_params,
    byref_param_escape_threadable, const_default, const_eval_default, fillable_default,
    is_dispatchable_builtin, is_generic_builtin, program_writes_global, types, unsupported,
};

impl Tx<'_, '_> {
    /// Lower a `Statement::Call`. Supports scalar math builtins: the
    /// shared runtime functions (`sqrt`/`floor`/`pow`/`atan2`/…, called
    /// via the resolved import) plus the type-polymorphic ones handled
    /// inline (`abs`/`signum`/`min`/`max`). Everything else skips.
    pub(super) fn call(
        &mut self,
        dest: Option<&Place>,
        call: &leek_mir::ir::CallExpr,
    ) -> Result<(), NativeError> {
        // A class reference called directly (`Class clazz = A; clazz()`) is
        // constructor sugar — and the class is statically known (a
        // `classref_local`), so construct it at compile time via `new_instance`
        // (`clazz(args)` == `new A(args)`). (Static-method calls use the class
        // ref as the method *receiver*, handled in the `Method` arm.)
        if let Callee::Indirect(local) = &call.callee
            && let Some(cls) = self.classref_locals.get(local).cloned()
        {
            let (res, res_ty) = self.new_instance(&cls, &call.args)?;
            if let Some(Place::Local(id)) = dest {
                let target = self.var_tys[id.0 as usize];
                let v = self.coerce(res, res_ty, target)?;
                self.b.def_var(self.vars[id.0 as usize], v);
            }
            return Ok(());
        }
        // A class reference passed as an argument flows as a value — fine when
        // the class has a constructor thunk (it constructs through
        // `dispatch_call_value` if the callee, e.g. `arrayMap`, invokes it).
        // Without a thunk (un-constructible class / v1) the runtime couldn't
        // build it, so keep skipping.
        if call.args.iter().any(|a| {
            matches!(a, Operand::Local(id)
                if self.classref_locals.contains_key(id) && !self.classref_has_thunk(*id))
        }) {
            return Err(unsupported("class reference passed as a call argument"));
        }
        let (res, res_ty) = match &call.callee {
            Callee::Builtin(name) => {
                // A global with the same name shadows the builtin (`cos =
                // function(...){…}; cos(1,2,3)`). Whether the global is set at
                // the call site is a runtime decision, so defer to the
                // `leek_call_ref_or_builtin` shim: it calls the global's value
                // if assigned, else dispatches the builtin — matching the
                // interpreter's resolution order. (Such globals are referenced
                // via `Global`/`GlobalRef` places, not always in
                // `program.globals`.)
                if program_writes_global(self.program, name) {
                    let name_h = self.const_string(name)?;
                    let (ptr, n) = self.build_ref_array(&call.args)?;
                    let nc = self.b.ins().iconst(types::I64, n as i64);
                    let ver = self
                        .b
                        .ins()
                        .iconst(types::I64, i64::from(self.lang.version));
                    let f = self.imports.rt("leek_call_ref_or_builtin")?;
                    let inst = self.b.ins().call(f, &[name_h, ptr, nc, ver]);
                    (self.b.inst_results(inst)[0], ValTy::Ref)
                } else {
                    self.dispatch_builtin(name, &call.args)?
                }
            }
            // `recv.method(args)`: first try static dispatch to a user-class
            // method; otherwise it's builtin sugar for `method(recv, args)`.
            Callee::Method { receiver, method } => {
                if let Some(res) = self.try_super_method(*receiver, method, &call.args)? {
                    res
                } else if let Some(res) = self.try_static_method(*receiver, method, &call.args)? {
                    res
                } else if let Some(res) = self.try_user_method(*receiver, method, &call.args)? {
                    res
                } else if let Some(res) = self.try_object_method(*receiver, method, &call.args)? {
                    res
                } else {
                    let mut combined = Vec::with_capacity(call.args.len() + 1);
                    combined.push(Operand::Local(*receiver));
                    combined.extend(call.args.iter().cloned());
                    // A method that's builtin-sugar (`arr.push(x)` → `push(arr,
                    // x)`) dispatches as that builtin. An *unknown* method
                    // (`null.toto()`) isn't a builtin — LeekScript resolves it
                    // to null (with a warning), so route it through the generic
                    // runtime dispatch, whose `call_builtin` returns null for an
                    // unknown name, instead of refusing to compile. Scoped to
                    // the method path (a bare unknown call still skips), and the
                    // generic-builtin gate skips it cleanly with no IR emitted.
                    if is_dispatchable_builtin(method, self.link_game) {
                        self.dispatch_builtin(method, &combined)?
                    } else {
                        self.generic_builtin(method, &combined)?
                    }
                }
            }
            Callee::Function(def_id) => {
                // A signature-file function carrying a `@native-backend:`
                // directive has no compiled body — dispatch the named
                // runtime builtin instead.
                if let Some(name) = self.native_directives.get(def_id) {
                    let name = name.clone();
                    self.dispatch_builtin(&name, &call.args)?
                } else {
                    self.user_call(*def_id, &call.args)?
                }
            }
            // `super(args)` in a constructor: chain to the parent class's
            // constructor, passing the current `this` as the receiver. The
            // instance's fields are already initialized by `new`; this just
            // runs the parent's constructor body. Returns null.
            Callee::SuperConstructor { this, parent_class } => {
                let prog = self.program;
                let ctor = prog
                    .class_by_name(parent_class)
                    .and_then(|c| prog.select_constructor(c, call.args.len()));
                if let Some(ctor_idx) = ctor
                    && let Some(def) = prog.functions[ctor_idx].def_id
                {
                    let Some((fref, sig)) = self.imports.user_fns.get(&def) else {
                        return Err(unsupported("super constructor not compiled"));
                    };
                    let (fref, sig) = (*fref, sig.clone());
                    let user_params = sig.params.len() - 1;
                    if call.args.len() > user_params {
                        return Err(unsupported("super constructor variadic args"));
                    }
                    let ctor_fn = &prog.functions[ctor_idx];
                    let (this_v, this_t) = self.local_value(*this)?;
                    if sig.has_defaults {
                        for i in call.args.len()..user_params {
                            if fillable_default(ctor_fn, ctor_fn.params[i + 1]).is_none() {
                                return Err(unsupported(
                                    "super constructor: omitted param without default",
                                ));
                            }
                        }
                        let mut cl_args = Vec::with_capacity(sig.params.len() + 1);
                        cl_args.push(self.coerce(this_v, this_t, sig.params[0])?);
                        for (i, &pty) in sig.params[1..].iter().enumerate() {
                            if i < call.args.len() {
                                let (v, t) = self.operand(&call.args[i])?;
                                cl_args.push(self.coerce(v, t, pty)?);
                            } else {
                                cl_args.push(self.placeholder(pty));
                            }
                        }
                        cl_args.push(
                            self.b
                                .ins()
                                .iconst(types::I64, (call.args.len() + 1) as i64),
                        );
                        self.b.ins().call(fref, &cl_args);
                    } else {
                        let mut defaults: Vec<Const> = Vec::new();
                        for i in call.args.len()..user_params {
                            match const_default(ctor_fn, ctor_fn.params[i + 1]) {
                                Some(c) => defaults.push(c),
                                None => {
                                    return Err(unsupported(
                                        "super constructor non-constant default arg",
                                    ));
                                }
                            }
                        }
                        let mut cl_args = Vec::with_capacity(sig.params.len());
                        cl_args.push(self.coerce(this_v, this_t, sig.params[0])?);
                        for (i, &pty) in sig.params[1..].iter().enumerate() {
                            let (v, t) = if i < call.args.len() {
                                self.operand(&call.args[i])?
                            } else {
                                self.operand(&Operand::Const(
                                    defaults[i - call.args.len()].clone(),
                                ))?
                            };
                            cl_args.push(self.coerce(v, t, pty)?);
                        }
                        self.b.ins().call(fref, &cl_args);
                    }
                }
                (self.boxed_null()?, ValTy::Ref)
            }
            // `f(args)` where `f` is a value (lambda / function ref): dispatch
            // through the runtime, which invokes a lambda's JIT'd body.
            Callee::Indirect(local) => {
                if self.var_tys[local.0 as usize] != ValTy::Ref {
                    return Err(unsupported("indirect call on non-function value"));
                }
                let (callee, _) = self.local_value(*local)?;
                // Pass cell-local args raw (as the shared cell handle) so the
                // runtime can thread them to a `@x` by-ref param; the dispatch
                // peels + clones them for by-value params.
                let (ptr, n) = self.build_ref_array_opt(&call.args, true)?;
                let f = self.imports.rt("leek_call_value")?;
                let nc = self.b.ins().iconst(types::I64, n as i64);
                let ver = self
                    .b
                    .ins()
                    .iconst(types::I64, i64::from(self.lang.version));
                let inst = self.b.ins().call(f, &[callee, ptr, nc, ver]);
                (self.b.inst_results(inst)[0], ValTy::Ref)
            }
        };
        if let Some(Place::Local(id)) = dest {
            let target = self.var_tys[id.0 as usize];
            let v = self.coerce(res, res_ty, target)?;
            let v = self.coerce_bigint_local(*id, v)?;
            self.b.def_var(self.vars[id.0 as usize], v);
        }
        Ok(())
    }

    /// Dispatch a builtin by name on already-lowered operands (shared by
    /// the free-function and method-call forms).
    pub(super) fn dispatch_builtin(
        &mut self,
        name: &str,
        args: &[Operand],
    ) -> Result<(Value, ValTy), NativeError> {
        if matches!(name, "abs" | "signum") {
            self.unary_poly(name, args)
        } else if matches!(name, "min" | "max") {
            self.min_max(name == "min", args)
        } else if name == "count" {
            self.count_call(args)
        } else if name == "push" {
            self.push_call(args)
        } else if leek_runtime::math_sig(name).is_some() {
            // A dynamic (boxed) arg can't be statically typed as a number; the
            // inline math path would unbox it as a real (`0.0` for a
            // non-number) whereas the shared `call_builtin` returns null for a
            // non-number — route through the generic path to match the interp
            // (`sqrt(instance)` → null, `floor(someRefReal)` → floor).
            if args.iter().any(|op| self.operand_is_ref(op)) {
                self.generic_builtin(name, args)
            } else {
                self.named_builtin(name, args)
            }
        } else if is_generic_builtin(name) {
            self.generic_builtin(name, args)
        } else if leek_runtime::builtin_class_name(name).is_some() {
            // A bare builtin-class name called as a function is constructor
            // sugar (`Array(1, 2)` == `[1, 2]`, `Map()` == `[:]`), exactly
            // mirroring the interpreter's `Callee::Builtin` →
            // `construct_builtin_class` path.
            let name_h = self.const_string(name)?;
            let (ptr, n) = self.build_ref_array(args)?;
            let f = self.imports.rt("leek_construct_builtin")?;
            let nc = self.b.ins().iconst(types::I64, n as i64);
            let inst = self.b.ins().call(f, &[name_h, ptr, nc]);
            Ok((self.b.inst_results(inst)[0], ValTy::Ref))
        } else if self.link_game {
            // Host game function (`getCell`, …): box name + args and forward
            // to the linked game runtime via `leek_game_builtin`.
            let name_h = self.const_string(name)?;
            let (ptr, n) = self.build_ref_array(args)?;
            let f = self.imports.rt("leek_game_builtin")?;
            let nc = self.b.ins().iconst(types::I64, n as i64);
            let inst = self.b.ins().call(f, &[name_h, ptr, nc]);
            Ok((self.b.inst_results(inst)[0], ValTy::Ref))
        } else {
            Err(unsupported(format!("builtin {name}")))
        }
    }

    /// An allowlisted, pure stdlib builtin (string/collection ops) lowered
    /// generically: box the name + each argument and dispatch through the
    /// shared `leek_runtime::call_builtin`. The result is a boxed value.
    pub(super) fn generic_builtin(
        &mut self,
        name: &str,
        args: &[Operand],
    ) -> Result<(Value, ValTy), NativeError> {
        let shim = match args.len() {
            0 => "leek_builtin0",
            1 => "leek_builtin1",
            2 => "leek_builtin2",
            3 => "leek_builtin3",
            4 => "leek_builtin4",
            n => return Err(unsupported(format!("{name}: arity {n} unsupported"))),
        };
        let f = self.imports.rt(shim)?;
        let name_h = self.const_string(name)?;
        let mut cl_args = vec![name_h];
        for op in args {
            let (v, t) = self.operand(op)?;
            cl_args.push(self.coerce(v, t, ValTy::Ref)?);
        }
        let ver = self
            .b
            .ins()
            .iconst(types::I64, i64::from(self.lang.version));
        cl_args.push(ver);
        let inst = self.b.ins().call(f, &cl_args);
        Ok((self.b.inst_results(inst)[0], ValTy::Ref))
    }

    /// Resolve a single parameter's default to a `DefaultArg`, if it's
    /// self-contained (a constant or a constant composite literal). `None`
    /// when the default references earlier params / calls a function / has no
    /// default — the caller then bails (skips or falls back to builtin sugar).
    pub(super) fn param_default(&self, callee: &MirFunction, param: LocalId) -> Option<DefaultArg> {
        if let Some(c) = const_default(callee, param) {
            Some(DefaultArg::Const(c))
        } else {
            const_eval_default(callee, param, self.lang.version).map(DefaultArg::Composite)
        }
    }

    /// Materialise a resolved default into a Cranelift value at the call site.
    pub(super) fn default_arg_value(
        &mut self,
        d: &DefaultArg,
    ) -> Result<(Value, ValTy), NativeError> {
        match d {
            DefaultArg::Const(c) => self.operand(&Operand::Const(c.clone())),
            DefaultArg::Composite(v) => self.fresh_composite_default(v),
        }
    }

    /// If callee parameter `i` is a `@x` by-ref param that needs the caller's
    /// shared `Value::Cell` and the argument there is a local already backed by
    /// a cell in this function, return the *raw* cell handle to pass — so the
    /// callee's rebinding (v2+: a reassigned/aliased param) or its capturing
    /// lambda's mutation (v1: a captured-escaping param, [`byref_param_capture_
    /// threadable`]) propagates back to the caller's storage. `None` means use
    /// the normal peeled-value argument path.
    pub(super) fn byref_cell_arg(
        &mut self,
        def_id: DefId,
        i: usize,
        args: &[Operand],
    ) -> Option<Value> {
        let Operand::Local(l) = args.get(i)? else {
            return None;
        };
        let l = *l;
        if !self.cell_locals.contains(&l) {
            return None;
        }
        let is_cell_param = {
            let g = self.program.function(def_id)?;
            let pid = *g.params.get(i)?;
            // Reassigned / aliased-onward params thread their cell in every
            // version; v1 additionally threads escaping (captured / returned)
            // params (`byref_param_escape_threadable`, whose params are already
            // `is_shared` cells rather than `byref_cell_params`).
            byref_cell_params(g, self.program).contains(&pid)
                || byref_param_escape_threadable(g, pid)
        };
        if !is_cell_param {
            return None;
        }
        Some(self.b.use_var(self.vars[l.0 as usize]))
    }

    /// Lower a call to a user function: coerce each argument to the
    /// callee's parameter kind and `call` it. Omitted trailing parameters
    /// with self-contained defaults are padded (see [`Self::param_default`]).
    pub(super) fn user_call(
        &mut self,
        def_id: DefId,
        args: &[Operand],
    ) -> Result<(Value, ValTy), NativeError> {
        let Some((fref, sig)) = self.imports.user_fns.get(&def_id) else {
            return Err(unsupported("call to unsupported function"));
        };
        let fref = *fref;
        let sig = sig.clone();
        // The callee's `@x` by-ref param mask. In v1, an arg passed to a by-ref
        // param is NOT deep-cloned (it shares the caller's backing store so an
        // in-place mutation propagates); `needs_cell_semantics` already skipped
        // the programs where a by-ref param escapes that one-level sharing.
        let byref_mask: Vec<bool> = self
            .program
            .function(def_id)
            .map(|c| {
                c.params
                    .iter()
                    .map(|p| c.locals[p.0 as usize].is_by_ref)
                    .collect()
            })
            .unwrap_or_default();
        // Too many args (variadic) isn't modeled. Too few is OK *if* every
        // omitted (trailing) parameter has a self-contained default — either a
        // single constant (`f(x = 2)`) or a constant composite literal
        // (`f(a = [1, [2, 3]])`), both evaluated at the call site. A default
        // that references earlier params or calls a function must run in the
        // callee's frame — skip those.
        if args.len() > sig.params.len() {
            return Err(unsupported("user call: variadic arguments"));
        }
        // `has_defaults`: the callee fills omitted defaulted params itself (via
        // the hidden `argc`). Every omitted param must carry a default (else
        // it's null — not modeled); pass placeholders + the provided-arg count.
        if sig.has_defaults {
            let callee = self
                .program
                .function(def_id)
                .ok_or_else(|| unsupported("user call: missing callee for defaults"))?;
            for i in args.len()..sig.params.len() {
                if fillable_default(callee, callee.params[i]).is_none() {
                    return Err(unsupported("user call: omitted param without default"));
                }
            }
            let mut cl_args = Vec::with_capacity(sig.params.len() + 1);
            for (i, &pty) in sig.params.iter().enumerate() {
                if let Some(cell) = self.byref_cell_arg(def_id, i, args) {
                    // Pass the shared cell handle raw (pty is `Ref` here).
                    cl_args.push(cell);
                } else if i < args.len() {
                    let (v, t) = self.operand(&args[i])?;
                    let mut a = self.coerce(v, t, pty)?;
                    if self.lang.version <= 1
                        && pty == ValTy::Ref
                        && !byref_mask.get(i).copied().unwrap_or(false)
                    {
                        let f = self.imports.rt("leek_clone_v1")?;
                        let inst = self.b.ins().call(f, &[a]);
                        a = self.b.inst_results(inst)[0];
                    }
                    cl_args.push(a);
                } else {
                    cl_args.push(self.placeholder(pty));
                }
            }
            cl_args.push(self.b.ins().iconst(types::I64, args.len() as i64));
            let inst = self.b.ins().call(fref, &cl_args);
            return Ok((self.b.inst_results(inst)[0], sig.ret));
        }
        // No non-const defaults: omitted trailing params (if any) are padded
        // from their self-contained constant/composite defaults at the call
        // site. A non-self-contained default → skip.
        let mut defaults: Vec<DefaultArg> = Vec::new();
        if args.len() < sig.params.len() {
            let callee = self
                .program
                .function(def_id)
                .ok_or_else(|| unsupported("user call: missing callee for defaults"))?;
            for i in args.len()..sig.params.len() {
                match self.param_default(callee, callee.params[i]) {
                    Some(d) => defaults.push(d),
                    None => return Err(unsupported("user call: non-constant default argument")),
                }
            }
        }
        let mut cl_args = Vec::with_capacity(sig.params.len());
        for (i, &pty) in sig.params.iter().enumerate() {
            if let Some(cell) = self.byref_cell_arg(def_id, i, args) {
                // Pass the shared cell handle raw (pty is `Ref` here).
                cl_args.push(cell);
                continue;
            }
            let (v, t) = if i < args.len() {
                self.operand(&args[i])?
            } else {
                self.default_arg_value(&defaults[i - args.len()])?
            };
            let mut a = self.coerce(v, t, pty)?;
            // v1 passes composites by value — deep-clone each handle arg, EXCEPT
            // an `@x` by-ref arg (shares the caller's store; see `byref_mask`).
            if self.lang.version <= 1
                && pty == ValTy::Ref
                && !byref_mask.get(i).copied().unwrap_or(false)
            {
                let f = self.imports.rt("leek_clone_v1")?;
                let inst = self.b.ins().call(f, &[a]);
                a = self.b.inst_results(inst)[0];
            }
            cl_args.push(a);
        }
        let inst = self.b.ins().call(fref, &cl_args);
        Ok((self.b.inst_results(inst)[0], sig.ret))
    }

    /// Emit a *fresh* copy of a compile-time-folded composite default value.
    /// The value is boxed once into a leaked handle (like a string literal);
    /// each call deep-clones it so callees that mutate the default don't alias
    /// across calls — matching the interpreter's per-call default
    /// re-evaluation. (`leek_clone_v1` is the deep-clone shim; despite the
    /// name it clones in every version.)
    pub(super) fn fresh_composite_default(
        &mut self,
        val: &leek_runtime::Value,
    ) -> Result<(Value, ValTy), NativeError> {
        let ptr = crate::runtime::box_value(val.clone()) as i64;
        let h = self.b.ins().iconst(types::I64, ptr);
        let f = self.imports.rt("leek_clone_v1")?;
        let inst = self.b.ins().call(f, &[h]);
        Ok((self.b.inst_results(inst)[0], ValTy::Ref))
    }

    /// A shared-runtime math builtin (`sqrt`, `floor`, `pow`, `atan2`, …)
    /// resolved as an import. Arity-tolerant like upstream: missing args
    /// default to `0`.
    pub(super) fn named_builtin(
        &mut self,
        name: &str,
        args: &[Operand],
    ) -> Result<(Value, ValTy), NativeError> {
        let Some(&(fref, sig)) = self.imports.named.get(name) else {
            return Err(unsupported(format!("builtin {name}")));
        };
        let arity = match sig {
            MathSig::RealToReal | MathSig::RealToInt => 1,
            MathSig::RealRealToReal => 2,
        };
        let mut cl_args = Vec::with_capacity(arity);
        for i in 0..arity {
            let v = match args.get(i) {
                Some(op) => {
                    let (v, t) = self.operand(op)?;
                    self.coerce(v, t, ValTy::Real)?
                }
                None => self.b.ins().f64const(0.0),
            };
            cl_args.push(v);
        }
        // Static per-call cost from the shared catalog (`sqrt` 8, `sin` 30,
        // …), mirroring the Java emitter's `builtin_call_cost`. The generic
        // (boxed-arg) path charges the same cost at runtime instead.
        self.charge(leek_runtime::builtin_cost(name))?;
        let inst = self.b.ins().call(fref, &cl_args);
        let res = self.b.inst_results(inst)[0];
        let res_ty = match sig {
            MathSig::RealToReal | MathSig::RealRealToReal => ValTy::Real,
            MathSig::RealToInt => ValTy::Int,
        };
        Ok((res, res_ty))
    }

    /// `abs` (result kind = argument kind) and `signum` (always int),
    /// lowered inline.
    pub(super) fn unary_poly(
        &mut self,
        name: &str,
        args: &[Operand],
    ) -> Result<(Value, ValTy), NativeError> {
        let (v, ty) = match args.first() {
            Some(op) => self.operand(op)?,
            None => (self.b.ins().iconst(types::I64, 0), ValTy::Int),
        };
        // A dynamic (boxed) arg can't be statically typed int-vs-real, so
        // the inline forms below don't apply — dispatch through the shared
        // `call_builtin` (handles `abs`/`signum` on any `Value`). EXCEPT a
        // literal `null`: `abs(null)` is `0.0` (real) in v2+ but `0` (int) in
        // v1 — emit that constant directly (its kind matches `call_result_ty`,
        // so the dest doesn't truncate). `signum(null)` and other null cases
        // still skip.
        if ty == ValTy::Ref {
            if name == "abs" && matches!(args.first(), Some(Operand::Const(Const::Null))) {
                self.charge(leek_runtime::builtin_cost(name))?;
                return if self.lang.version <= 1 {
                    Ok((self.b.ins().iconst(types::I64, 0), ValTy::Int))
                } else {
                    Ok((self.b.ins().f64const(0.0), ValTy::Real))
                };
            }
            if matches!(args.first(), Some(Operand::Const(Const::Null))) {
                return Err(unsupported(format!("{name}(null) — real/int result")));
            }
            // Runtime-charged via `charge_builtin_ops` in the shim.
            return self.generic_builtin(name, args);
        }
        // Static per-call cost (`abs` 2; `signum` uncatalogued → 0), as the
        // Java emitter charges via `builtin_call_cost`.
        self.charge(leek_runtime::builtin_cost(name))?;
        match name {
            "abs" => match ty {
                ValTy::Real => Ok((self.b.ins().fabs(v), ValTy::Real)),
                _ => Ok((self.b.ins().iabs(v), ValTy::Int)),
            },
            // signum: (x > 0) - (x < 0), as int.
            "signum" => {
                let (gt, lt) = if ty == ValTy::Real {
                    let z = self.b.ins().f64const(0.0);
                    (
                        self.b.ins().fcmp(FloatCC::GreaterThan, v, z),
                        self.b.ins().fcmp(FloatCC::LessThan, v, z),
                    )
                } else {
                    let z = self.b.ins().iconst(types::I64, 0);
                    (
                        self.b.ins().icmp(IntCC::SignedGreaterThan, v, z),
                        self.b.ins().icmp(IntCC::SignedLessThan, v, z),
                    )
                };
                let gt = self.b.ins().uextend(types::I64, gt);
                let lt = self.b.ins().uextend(types::I64, lt);
                Ok((self.b.ins().isub(gt, lt), ValTy::Int))
            }
            _ => Err(unsupported(format!("builtin {name}"))),
        }
    }

    /// `min` / `max` of two scalars: both-int stays int, any-real
    /// promotes to real (matching the interpreter's `min_max_pair`).
    pub(super) fn min_max(
        &mut self,
        want_min: bool,
        args: &[Operand],
    ) -> Result<(Value, ValTy), NativeError> {
        if args.len() != 2 {
            return Err(unsupported("min/max: expected 2 args"));
        }
        let (a, at) = self.operand(&args[0])?;
        let (b, bt) = self.operand(&args[1])?;
        // A dynamic (boxed) operand routes through the shared builtin
        // catalog (`call_builtin("min"/"max", …)`), which handles mixed /
        // non-numeric values exactly like the interpreter.
        if at == ValTy::Ref || bt == ValTy::Ref {
            return self.generic_builtin(if want_min { "min" } else { "max" }, args);
        }
        // min/max are uncatalogued upstream → 0 ops; charge the shared
        // catalog cost anyway so a future catalog change stays in sync.
        self.charge(leek_runtime::builtin_cost(if want_min {
            "min"
        } else {
            "max"
        }))?;
        if at == ValTy::Real || bt == ValTy::Real {
            let a = self.coerce(a, at, ValTy::Real)?;
            let b = self.coerce(b, bt, ValTy::Real)?;
            // min: pick a when a <= b; max: pick a when a >= b.
            let cc = if want_min {
                FloatCC::LessThanOrEqual
            } else {
                FloatCC::GreaterThanOrEqual
            };
            let pick_a = self.b.ins().fcmp(cc, a, b);
            Ok((self.b.ins().select(pick_a, a, b), ValTy::Real))
        } else {
            let cc = if want_min {
                IntCC::SignedLessThanOrEqual
            } else {
                IntCC::SignedGreaterThanOrEqual
            };
            let pick_a = self.b.ins().icmp(cc, a, b);
            Ok((self.b.ins().select(pick_a, a, b), ValTy::Int))
        }
    }

    /// `count(x)` — element count of an array/map/set/string handle.
    pub(super) fn count_call(&mut self, args: &[Operand]) -> Result<(Value, ValTy), NativeError> {
        // Catalog cost (`count` → 1), as the Java emitter charges statically.
        self.charge(leek_runtime::builtin_cost("count"))?;
        let count = self.imports.rt("leek_count")?;
        let (v, t) = match args.first() {
            Some(op) => self.operand(op)?,
            None => return Err(unsupported("count: missing argument")),
        };
        let h = self.coerce(v, t, ValTy::Ref)?;
        let ver = self
            .b
            .ins()
            .iconst(types::I64, i64::from(self.lang.version));
        let inst = self.b.ins().call(count, &[h, ver]);
        Ok((self.b.inst_results(inst)[0], ValTy::Int))
    }

    /// `push(arr, elem)` — append in place. Returns `null` (a handle), per
    /// the interpreter.
    pub(super) fn push_call(&mut self, args: &[Operand]) -> Result<(Value, ValTy), NativeError> {
        if args.len() != 2 {
            return Err(unsupported("push: expected 2 args"));
        }
        // Catalog cost (`push` → 2), as the Java emitter charges statically;
        // the shim adds the legacy per-insert cost on top for v1-3.
        self.charge(leek_runtime::builtin_cost("push"))?;
        let push = self.imports.rt("leek_array_push")?;
        let null = self.imports.rt("leek_box_null")?;
        let (arr, at) = self.operand(&args[0])?;
        if at != ValTy::Ref {
            return Err(unsupported("push to non-array"));
        }
        let (e, et) = self.operand(&args[1])?;
        let mut elem = self.coerce(e, et, ValTy::Ref)?;
        // v1 stores a *copy* of the pushed composite (value semantics),
        // matching `leek_runtime`'s push.
        if self.lang.version <= 1 {
            let f = self.imports.rt("leek_clone_v1")?;
            let inst = self.b.ins().call(f, &[elem]);
            elem = self.b.inst_results(inst)[0];
        }
        let ver = self
            .b
            .ins()
            .iconst(types::I64, i64::from(self.lang.version));
        self.b.ins().call(push, &[arr, elem, ver]);
        let inst = self.b.ins().call(null, &[]);
        Ok((self.b.inst_results(inst)[0], ValTy::Ref))
    }
}
