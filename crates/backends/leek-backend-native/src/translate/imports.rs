//! Runtime-import declarations (moved verbatim from translate/mod.rs).

use super::{FunctionBuilder, MirFunction, Module, HashMap, DefId, FnSig, MirProgram, Lang, Imports, NativeError, new_class_locals, aliased_class_locals, classref_locals, super_locals, stmt_has_string_const, Statement, Callee, is_generic_builtin, receiver_class, resolve_static_method, Rvalue, Place, BinOp, const_pow_exp, Terminator, Operand, Const, scalar_valty, ValTy, unsupported, ClType, FuncRef, AbiParam, Linkage, MathSig, types};

/// Declare, as imports in `module`, the runtime functions `main` needs —
/// named scalar math builtins plus the `**` operator's `leek_pow` /
/// `leek_ipow` helpers — resolving each to a `FuncRef` usable in `main`.
/// Bails if such a call is present but there's no module (text dumps).
pub(super) fn declare_imports(
    builder: &mut FunctionBuilder,
    mir_fn: &MirFunction,
    mut module: Option<&mut dyn Module>,
    callees: &HashMap<DefId, (cranelift_module::FuncId, FnSig)>,
    field_init_callees: &HashMap<usize, (cranelift_module::FuncId, FnSig)>,
    program: &MirProgram,
    // `DefId` → `@native-backend:` directive. Calls to these dispatch a
    // runtime builtin, so they need that builtin's imports (not a user
    // FuncRef).
    native_directives: &HashMap<DefId, String>,
    // True if any local is a boxed `Ref` (so the body may do dynamic
    // value-ops / box/unbox) — forces the composite shims to be declared.
    any_ref_local: bool,
    lang: Lang,
    // Declare the `leek_dbg_safepoint` debug shim (statement safepoints).
    debug_hooks: bool,
    // Declare the `leek_game_builtin` shim (host game functions).
    link_game: bool,
) -> Result<Imports, NativeError> {
    let version = lang.version;
    // Which named math builtins are called, is `**` used, and which user
    // functions are invoked?
    let mut needed: Vec<&str> = Vec::new();
    let mut pow_real = false;
    let mut pow_int = false;
    let mut called_fns: Vec<DefId> = Vec::new();
    // Class field-initializer functions referenced by a `new` here.
    let mut field_init_idxs: Vec<usize> = Vec::new();
    let new_classes = new_class_locals(mir_fn);
    let aliased_classes = aliased_class_locals(mir_fn);
    let classrefs = classref_locals(mir_fn);
    let supers = super_locals(mir_fn);
    let mut uses_composite = any_ref_local;
    for block in &mir_fn.blocks {
        for s in &block.statements {
            // A string literal anywhere boxes into a handle and needs the
            // composite shims (box/unbox + dynamic ops for concat).
            if stmt_has_string_const(s) {
                uses_composite = true;
            }
            match s {
                Statement::Call { call, .. } => {
                    // A builtin name, whether called free (`f(x)`) or as a
                    // method (`x.f()`).
                    let builtin = match &call.callee {
                        Callee::Builtin(n) => Some(n.as_str()),
                        Callee::Method { method, .. } => Some(method.as_str()),
                        // A `@native-backend:` directive call dispatches a
                        // runtime builtin — declare that builtin's imports.
                        Callee::Function(def_id) => {
                            native_directives.get(def_id).map(std::string::String::as_str)
                        }
                        _ => None,
                    };
                    // Builtin imports needed when the call dispatches as a
                    // builtin — a free `f(x)` or method-sugar `x.f()`.
                    if let Some(n) = builtin {
                        if matches!(n, "count" | "push") || is_generic_builtin(n) {
                            uses_composite = true;
                        } else if (leek_runtime::math_sig(n).is_some()
                            || matches!(n, "abs" | "signum" | "min" | "max"))
                            && !needed.contains(&n)
                        {
                            needed.push(n);
                        }
                    }
                    // User-function / method / constructor callee edges. This
                    // runs INDEPENDENTLY of the builtin check above so a method
                    // whose name also happens to be a builtin (`A.sqrt()` where
                    // `sqrt` is a user static method) still imports its user
                    // target — otherwise the call site can't reach the body.
                    match &call.callee {
                        Callee::Function(def_id)
                            if !called_fns.contains(def_id)
                                && !native_directives.contains_key(def_id) =>
                        {
                            called_fns.push(*def_id);
                        }
                        // A user-class method call (receiver is a known
                        // `ClassInstance`): resolve it through the vtable
                        // and pull its function in as a callee.
                        Callee::Method { receiver, method } => {
                            if let Some(name) =
                                receiver_class(&mir_fn.locals, &new_classes, &aliased_classes, *receiver)
                                && let Some(c) = program.class_by_name(name)
                                && let Some(vt) =
                                    program.resolve_method(c, method, Some(call.args.len()))
                                && let Some(d) = program.functions[vt.function_idx].def_id
                            {
                                if !called_fns.contains(&d) {
                                    called_fns.push(d);
                                }
                                uses_composite = true;
                            }
                            // `C.staticMethod()` — receiver is a class ref.
                            if let Some(cls) = classrefs.get(receiver)
                                && let Some(idx) =
                                    resolve_static_method(program, cls, method, call.args.len())
                                && let Some(d) = program.functions[idx].def_id
                            {
                                if !called_fns.contains(&d) {
                                    called_fns.push(d);
                                }
                                uses_composite = true;
                            }
                            // `super.m()` — receiver is a `MakeSuper`; resolve
                            // against the parent class.
                            if let Some((_, parent)) = supers.get(receiver)
                                && let Some(c) = program.class_by_name(parent)
                                && let Some(vt) =
                                    program.resolve_method(c, method, Some(call.args.len()))
                                && let Some(d) = program.functions[vt.function_idx].def_id
                            {
                                if !called_fns.contains(&d) {
                                    called_fns.push(d);
                                }
                                uses_composite = true;
                            }
                        }
                        Callee::SuperConstructor { parent_class, .. } => {
                            if let Some(c) = program.class_by_name(parent_class)
                                && let Some(ci) = program.select_constructor(c, call.args.len())
                                && let Some(d) = program.functions[ci].def_id
                            {
                                if !called_fns.contains(&d) {
                                    called_fns.push(d);
                                }
                                uses_composite = true;
                            }
                        }
                        // `clazz()` — a class ref called directly constructs the
                        // class; import its field-inits + constructor (like `new`).
                        Callee::Indirect(local) => {
                            if let Some(cls) = classrefs.get(local)
                                && let Some(c) = program.class_by_name(cls)
                            {
                                uses_composite = true;
                                for fs in &c.field_layout {
                                    if let Some(fi) = fs.init_fn
                                        && !field_init_idxs.contains(&fi)
                                    {
                                        field_init_idxs.push(fi);
                                    }
                                }
                                if let Some(ci) = program.select_constructor(c, call.args.len())
                                    && let Some(d) = program.functions[ci].def_id
                                    && !called_fns.contains(&d)
                                {
                                    called_fns.push(d);
                                }
                            }
                        }
                        _ => {}
                    }
                }
                // `new C(...)`: needs the instance-alloc + field-set shims,
                // the class's field-initializer functions, and the selected
                // constructor.
                Statement::Assign(_, Rvalue::New { class, args }) => {
                    uses_composite = true;
                    if let Some(c) = program.class_by_name(class) {
                        for fs in &c.field_layout {
                            if let Some(fi) = fs.init_fn
                                && !field_init_idxs.contains(&fi)
                            {
                                field_init_idxs.push(fi);
                            }
                        }
                        if let Some(ci) = program.select_constructor(c, args.len())
                            && let Some(d) = program.functions[ci].def_id
                            && !called_fns.contains(&d)
                        {
                            called_fns.push(d);
                        }
                    }
                }
                Statement::Assign(
                    _,
                    Rvalue::Array(_)
                    | Rvalue::Index(..)
                    | Rvalue::Slice(..)
                    | Rvalue::MakeForeachIter(_)
                    | Rvalue::Map(_)
                    | Rvalue::Set(_)
                    | Rvalue::Interval(_)
                    | Rvalue::Object(_)
                    | Rvalue::Field(..)
                    | Rvalue::Cast(..)
                    | Rvalue::GlobalRef(..),
                )
                | Statement::Assign(
                    Place::Index(..) | Place::Field(..) | Place::Global(..),
                    _,
                ) => {
                    uses_composite = true;
                }
                Statement::Assign(_, Rvalue::Binary(op, _l, r))
                    if matches!(op, BinOp::Pow)
                        // v1 `^=` is power-assign, sharing the `**` path.
                        || (matches!(op, BinOp::CompoundXor) && version <= 1) =>
                {
                    // Real path always uses leek_pow; int path uses
                    // leek_ipow when the exponent is a small constant.
                    pow_real = true;
                    if matches!(const_pow_exp(r), Some(e) if (0..64).contains(&e)) {
                        pow_int = true;
                    }
                }
                _ => {}
            }
        }
        // A `Branch` whose condition is an inline `Ref` const (`if (null)`, or
        // a string literal) routes through the `leek_truthy` shim. A `Ref`
        // *local* condition is already covered by `any_ref_local`; only the
        // inline-const case needs flagging here.
        if let Terminator::Branch { cond, .. } = &block.terminator
            && matches!(cond, Operand::Const(Const::Null | Const::String(_)))
        {
            uses_composite = true;
        }
    }
    // `abs`/`signum`/`min`/`max` are inlined — they need no import.
    let needs_named: Vec<&str> = needed
        .into_iter()
        .filter(|n| leek_runtime::math_sig(n).is_some())
        .collect();

    // A non-scalar (boxed `Ref`) parameter means the body box/unboxes and
    // does dynamic value-ops — pull in the composite shims.
    if mir_fn
        .params
        .iter()
        .any(|p| scalar_valty(&mir_fn.locals[p.0 as usize].ty).is_none())
    {
        uses_composite = true;
    }
    // Any `Return(None)` lowers to a boxed null when the result kind is
    // `Ref` (a void function, or an unreachable dead block in `main` after
    // an exhaustive switch), so the function needs `leek_box_null`.
    if mir_fn
        .blocks
        .iter()
        .any(|b| matches!(b.terminator, Terminator::Return(None)))
    {
        uses_composite = true;
    }
    // Calling a function whose ABI involves a `Ref` means the caller must
    // box arguments / unbox the result.
    if called_fns.iter().any(|d| {
        callees
            .get(d)
            .is_some_and(|(_, sig)| sig.ret == ValTy::Ref || sig.params.contains(&ValTy::Ref))
    }) {
        uses_composite = true;
    }

    // The version gate for composites lives in the `Tx` lowering methods
    // (they bail on v1–v3 before touching a shim), so declaring the shims
    // here is harmless even when the function will ultimately skip.
    let mut imports = Imports::default();
    // Op-counting shims are needed by *every* function that charges (binary
    // ops, branches, calls, builtins) — i.e. essentially all of them — so
    // declare them up front, before the "no other imports" early return. Only
    // text-dump mode (no module) skips them; charging is a no-op there.
    if let Some(m) = module.as_deref_mut() {
        let i = types::I64;
        let f = types::F64;
        let op_shims: &[(&'static str, &[ClType], Option<ClType>)] = &[
            ("leek_charge_ops", &[i], None),
            ("leek_op_budget_exceeded", &[], Some(i)),
            ("leek_charge_concat", &[i], None),
            // Scalar box/unbox: a typed function can need a scalar↔handle
            // coercion (e.g. an `-> integer` function whose body yields a
            // boxed value) even when the `uses_composite` heuristic is off, so
            // declare these unconditionally — `coerce` must never miss a shim.
            ("leek_box_int", &[i], Some(i)),
            ("leek_box_real", &[f], Some(i)),
            ("leek_box_bool", &[i], Some(i)),
            ("leek_unbox_int", &[i], Some(i)),
            ("leek_unbox_real", &[i], Some(f)),
            ("leek_unbox_bool", &[i], Some(i)),
        ];
        for (sym, params, ret) in op_shims {
            let mut sig = m.make_signature();
            for &p in *params {
                sig.params.push(AbiParam::new(p));
            }
            if let Some(r) = ret {
                sig.returns.push(AbiParam::new(*r));
            }
            let id = m
                .declare_function(sym, Linkage::Import, &sig)
                .map_err(|e| NativeError::Compile(e.to_string()))?;
            imports.rt.insert(sym, m.declare_func_in_func(id, builder.func));
        }
    }

    if needs_named.is_empty()
        && !pow_real
        && !pow_int
        && called_fns.is_empty()
        && field_init_idxs.is_empty()
        && !uses_composite
        && !debug_hooks
        && !link_game
    {
        return Ok(imports);
    }
    let Some(module) = module else {
        return Err(unsupported("call in text-dump emit mode"));
    };

    // Resolve each called user function to a `FuncRef` in this function.
    for def_id in called_fns {
        let Some((func_id, sig)) = callees.get(&def_id) else {
            return Err(unsupported("call to unsupported function"));
        };
        let fref = module.declare_func_in_func(*func_id, builder.func);
        imports.user_fns.insert(def_id, (fref, sig.clone()));
    }
    // Resolve each referenced field-initializer (keyed by function index).
    for idx in field_init_idxs {
        let Some((func_id, sig)) = field_init_callees.get(&idx) else {
            return Err(unsupported("field-init not compiled"));
        };
        let fref = module.declare_func_in_func(*func_id, builder.func);
        imports.field_init_fns.insert(idx, (fref, sig.clone()));
    }

    // Helper: declare an import with a given (params, ret) and resolve it.
    let declare = |module: &mut dyn Module,
                       builder: &mut FunctionBuilder,
                       symbol: &str,
                       params: &[ClType],
                       ret: ClType|
     -> Result<FuncRef, NativeError> {
        let mut sig = module.make_signature();
        for &p in params {
            sig.params.push(AbiParam::new(p));
        }
        sig.returns.push(AbiParam::new(ret));
        let id = module
            .declare_function(symbol, Linkage::Import, &sig)
            .map_err(|e| NativeError::Compile(e.to_string()))?;
        Ok(module.declare_func_in_func(id, builder.func))
    };

    let table = leek_runtime::math_builtins();
    for name in needs_named {
        let Some(b) = table.iter().find(|b| b.leek_name == name) else {
            continue;
        };
        let (params, ret): (&[ClType], ClType) = match b.sig {
            MathSig::RealToReal => (&[types::F64], types::F64),
            MathSig::RealToInt => (&[types::F64], types::I64),
            MathSig::RealRealToReal => (&[types::F64, types::F64], types::F64),
        };
        let fref = declare(module, builder, b.symbol, params, ret)?;
        imports.named.insert(name.to_string(), (fref, b.sig));
    }
    if pow_real {
        let fref = declare(module, builder, "leek_pow", &[types::F64, types::F64], types::F64)?;
        imports.pow_real = Some(fref);
    }
    if pow_int {
        let (sym, _addr) = leek_runtime::ipow_addr();
        let fref = declare(module, builder, sym, &[types::I64, types::I64], types::I64)?;
        imports.pow_int = Some(fref);
    }
    if uses_composite {
        // Declare every composite-value shim (a handle is an i64; reals
        // box/unbox through f64). `leek_array_push` returns nothing.
        let i = types::I64;
        let shims: &[(&'static str, &[ClType], Option<ClType>)] = &[
            // Scalar box/unbox (`leek_box_int` etc.) are declared
            // unconditionally above; `leek_box_null` stays here (composite).
            ("leek_box_null", &[], Some(i)),
            ("leek_array_new", &[], Some(i)),
            ("leek_array_push", &[i, i], None),
            ("leek_value_index", &[i, i, i], Some(i)),
            ("leek_value_set_index", &[i, i, i, i], None),
            ("leek_map_new", &[], Some(i)),
            ("leek_map_put", &[i, i, i], None),
            ("leek_set_new", &[], Some(i)),
            ("leek_set_add", &[i, i], None),
            ("leek_object_new", &[], Some(i)),
            ("leek_instance_new", &[i, i], Some(i)),
            ("leek_global_get", &[i], Some(i)),
            ("leek_global_set", &[i, i], None),
            ("leek_ref_or_builtin", &[i], Some(i)),
            ("leek_call_ref_or_builtin", &[i, i, i, i], Some(i)),
            ("leek_static_get", &[i, i], Some(i)),
            ("leek_static_set", &[i, i, i], None),
            ("leek_coerce_scalar", &[i, i], Some(i)),
            ("leek_slice", &[i, i, i, i], Some(i)),
            ("leek_interval", &[i, i, i], Some(i)),
            ("leek_count", &[i, i], Some(i)),
            ("leek_truthy", &[i], Some(i)),
            ("leek_value_unary", &[i, i], Some(i)),
            ("leek_apply_cast", &[i, i], Some(i)),
            ("leek_clone_v1", &[i], Some(i)),
            ("leek_make_cell", &[i], Some(i)),
            ("leek_cell_get", &[i], Some(i)),
            ("leek_cell_set", &[i, i], None),
            ("leek_apply_promotion", &[i], Some(i)),
            ("leek_make_lambda", &[i, i, i], Some(i)),
            ("leek_call_value", &[i, i, i, i], Some(i)),
            ("leek_call_method", &[i, i, i, i, i], Some(i)),
            ("leek_value_binop", &[i, i, i, i], Some(i)),
            ("leek_foreach_iter", &[i], Some(i)),
            ("leek_class_of", &[i], Some(i)),
            ("leek_class_super", &[i], Some(i)),
            ("leek_construct_builtin", &[i, i, i], Some(i)),
            ("leek_builtin0", &[i, i], Some(i)),
            ("leek_builtin1", &[i, i, i], Some(i)),
            ("leek_builtin2", &[i, i, i, i], Some(i)),
            ("leek_builtin3", &[i, i, i, i, i], Some(i)),
            ("leek_builtin4", &[i, i, i, i, i, i], Some(i)),
        ];
        for (sym, params, ret) in shims {
            let mut sig = module.make_signature();
            for &p in *params {
                sig.params.push(AbiParam::new(p));
            }
            if let Some(r) = ret {
                sig.returns.push(AbiParam::new(*r));
            }
            let id = module
                .declare_function(sym, Linkage::Import, &sig)
                .map_err(|e| NativeError::Compile(e.to_string()))?;
            imports.rt.insert(sym, module.declare_func_in_func(id, builder.func));
        }
    }
    if debug_hooks {
        // `leek_dbg_safepoint(offset, frame_desc, frame_values) -> ()` —
        // declared independently of the composite shims so even a scalar-only
        // program can be paused and have its locals inspected.
        let mut sig = module.make_signature();
        sig.params.push(AbiParam::new(types::I64));
        sig.params.push(AbiParam::new(types::I64));
        sig.params.push(AbiParam::new(types::I64));
        let id = module
            .declare_function("leek_dbg_safepoint", Linkage::Import, &sig)
            .map_err(|e| NativeError::Compile(e.to_string()))?;
        imports
            .rt
            .insert("leek_dbg_safepoint", module.declare_func_in_func(id, builder.func));

        // `leek_dbg_enter(frame_desc: i64) -> ()` — pushes a shadow frame.
        let mut enter_sig = module.make_signature();
        enter_sig.params.push(AbiParam::new(types::I64));
        let enter_id = module
            .declare_function("leek_dbg_enter", Linkage::Import, &enter_sig)
            .map_err(|e| NativeError::Compile(e.to_string()))?;
        imports
            .rt
            .insert("leek_dbg_enter", module.declare_func_in_func(enter_id, builder.func));

        // `leek_dbg_leave() -> ()` — pops the top shadow frame.
        let leave_sig = module.make_signature();
        let leave_id = module
            .declare_function("leek_dbg_leave", Linkage::Import, &leave_sig)
            .map_err(|e| NativeError::Compile(e.to_string()))?;
        imports
            .rt
            .insert("leek_dbg_leave", module.declare_func_in_func(leave_id, builder.func));
    }
    if link_game {
        // `leek_game_builtin(name, argv, argc) -> i64` — host game functions.
        let mut sig = module.make_signature();
        sig.params.push(AbiParam::new(types::I64));
        sig.params.push(AbiParam::new(types::I64));
        sig.params.push(AbiParam::new(types::I64));
        sig.returns.push(AbiParam::new(types::I64));
        let id = module
            .declare_function("leek_game_builtin", Linkage::Import, &sig)
            .map_err(|e| NativeError::Compile(e.to_string()))?;
        imports
            .rt
            .insert("leek_game_builtin", module.declare_func_in_func(id, builder.func));
    }
    Ok(imports)
}
