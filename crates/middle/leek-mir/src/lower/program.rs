//! Program-level HIR → MIR lowering.

use std::collections::HashMap;

use leek_hir::{DefId, Expr, Function, HirFile, Stmt};
use leek_span::Span;
use leek_types::Type;

use crate::ir::{
    FunctionKind, LocalKind, MirClass, MirField, MirFunction, MirGlobal, MirMethod, MirProgram,
    Statement, Terminator,
};

use super::util::{
    captured_by_nested_lambda_body, captured_by_nested_lambda_stmts, lower_visibility,
    placeholder_function,
};
use super::{FnLowerer, MethodCtx, PendingLambda, ProgramCtx};

impl<'a> ProgramCtx<'a> {
    pub(crate) fn new(hir: &'a HirFile) -> Self {
        Self {
            hir,
            program: MirProgram::default(),
            errors: Vec::new(),
            globals: HashMap::new(),
            pending_lambdas: Vec::new(),
        }
    }

    pub(crate) fn lower(&mut self) {
        // First pass: register globals so functions lowered next
        // can resolve `NameRef::Global` references.
        // A `Global` def's `DefId` is its index in `defs` (the HIR doesn't
        // carry it back), so read it straight off `enumerate` rather than
        // re-scanning `defs` for each global.
        for (idx, def) in self.hir.defs.iter().enumerate() {
            if let leek_hir::Def::Global(g) = def {
                let def_id = DefId(u32::try_from(idx).expect("more than u32::MAX defs"));
                self.globals.insert(def_id, g.name.clone());
                self.program.globals.push(MirGlobal {
                    def_id,
                    name: g.name.clone(),
                    ty: g.ty.clone().unwrap_or(Type::Any),
                    span: g.span,
                });
            }
        }

        // Lower each user-defined function.
        for (idx, def) in self.hir.defs.iter().enumerate() {
            let def_id = DefId(u32::try_from(idx).expect("more than u32::MAX defs"));
            match def {
                leek_hir::Def::Function(f) => {
                    if let Some(func) = self.lower_function(def_id, f) {
                        self.program.functions.push(func);
                    }
                }
                leek_hir::Def::Class(c) => {
                    self.lower_class(def_id, c);
                }
                leek_hir::Def::Global(_) | leek_hir::Def::Local(_) => {}
            }
        }

        // Lower the file's top-level statements into a synthetic
        // `main` function. Globals' initializers are *not* run
        // here — HIR keeps each global's init separately. A future
        // slice should prepend a `__init` block; for now we just
        // emit the main statements.
        let main = self.lower_main();
        self.program.functions.push(main);

        // Drain pending lambdas. Lambdas reserve their function
        // slots during parent lowering (pushing placeholder
        // MirFunctions), so we just need to fill those slots now.
        // Process in FIFO order to keep nested-lambda indices
        // intuitive — outer-first, then nested closures encountered
        // during the body lowering.
        while let Some(task) = self.next_pending_lambda() {
            let func = self.lower_pending_lambda(&task);
            self.program.functions[task.function_idx] = func;
        }

        // All classes are now lowered — resolve parent links and
        // build each class's flattened field/method layout so
        // backends consume a structured form instead of re-walking
        // the inheritance chain by name.
        self.program.compute_class_layouts();
    }

    pub(crate) fn next_pending_lambda(&mut self) -> Option<PendingLambda> {
        if self.pending_lambdas.is_empty() {
            None
        } else {
            Some(self.pending_lambdas.remove(0))
        }
    }

    pub(crate) fn lower_pending_lambda(&mut self, task: &PendingLambda) -> MirFunction {
        let lam = &task.lambda;
        // Lambda return type isn't carried on `LambdaExpr` —
        // treat it as `Any` and let coerce_to_type be a no-op.
        let return_ty = Type::Any;
        let mut fl = FnLowerer::new(
            self.hir,
            &self.globals,
            &mut self.errors,
            &mut self.program.functions,
            &mut self.pending_lambdas,
            FunctionKind::User,
            "<lambda>".into(),
            None,
            task.span,
            return_ty,
        );
        // Capture slots come first, in the same order as the
        // parent's MakeLambda operands. The MirFunction's `params`
        // list includes them so the caller can pass
        // `[captures..., user_args...]` as a single arg vector.
        //
        // When the lambda was created inside a method body and
        // references `this` (or `super`/`Class_`/a rewritten field
        // name), an implicit `this` capture sits in slot 0 before
        // any user-bound DefId captures. We rebuild `method_ctx`
        // pointing at that slot so the lambda body's `NameRef::This`
        // resolves to the captured value rather than `Null`.
        if task.needs_this {
            let id = fl.declare_local(
                Some("__cap_this".into()),
                Type::Any,
                LocalKind::Param,
                task.span,
            );
            fl.params.push(id);
            if let Some(ctx) = &task.method_ctx {
                fl.method_ctx = Some(MethodCtx {
                    this_local: Some(id),
                    class_def_id: ctx.class_def_id,
                    class_name: ctx.class_name.clone(),
                    parent_class: ctx.parent_class.clone(),
                });
            }
        }
        for cap_def in &task.captures {
            // Capture name kept anonymous; resolution back to the
            // parent's name is debug-only and we don't carry that
            // info into MIR.
            let id = fl.declare_local(
                Some(format!("__cap{}", fl.captures.len())),
                Type::Any,
                LocalKind::Param,
                task.span,
            );
            // Capture slots hold the `Value::Cell` Rc passed in by
            // the caller — writes through the slot must go *into*
            // the cell so the outer scope sees them. Mark as shared
            // so `write_place` takes that branch.
            fl.locals[id.0 as usize].is_shared = true;
            fl.local_map.insert(*cap_def, id);
            fl.captures.push(*cap_def);
            fl.params.push(id);
        }
        // Then declare the user-visible params.
        for p in &lam.params {
            let id = fl.declare_local(
                Some(p.name.clone()),
                p.ty.clone().unwrap_or(Type::Any),
                LocalKind::Param,
                p.span,
            );
            // `@x` reference param on the lambda — see the
            // equivalent path in `lower_function`.
            if p.is_by_ref {
                fl.locals[id.0 as usize].is_shared = true;
                fl.locals[id.0 as usize].is_by_ref = true;
            }
            fl.local_map.insert(p.def, id);
            fl.params.push(id);
        }
        // Param-binding charges, per call — same model as `lower_function`:
        // v1 binds every by-value lambda param through the 2-arg Box ctor
        // (1 op each); v2+ boxes only params captured by an inner lambda.
        // `@`-ref params alias the caller's box (no charge).
        let v1_boxes = lam.params.iter().filter(|p| !p.is_by_ref).count() as u64;
        let vn_boxes = lam
            .params
            .iter()
            .filter(|p| !p.is_by_ref && captured_by_nested_lambda_body(&lam.body, p.def))
            .count() as u64;
        if v1_boxes > 0 || vn_boxes > 0 {
            fl.push_stmt(Statement::ChargeVersioned {
                v1: v1_boxes,
                vn: vn_boxes,
            });
        }
        match &lam.body {
            leek_hir::LambdaBody::Block(b) => {
                fl.lower_block_stmts(&b.stmts);
                fl.close_with_implicit_return(b.span);
            }
            leek_hir::LambdaBody::Expr(e) => {
                let v = fl.lower_expr_to_operand(e);
                if fl.is_open() {
                    fl.set_terminator(Terminator::Return(Some(v)));
                }
                fl.close_with_implicit_return(task.span);
            }
        }
        // Default expressions for lambda params, same pass as
        // top-level functions.
        for (param_local_idx, p) in lam.params.iter().enumerate() {
            let Some(default_expr) = &p.default else {
                continue;
            };
            let bb = fl.new_block();
            fl.resume(bb);
            let v = fl.lower_expr_to_operand(default_expr);
            if fl.is_open() {
                fl.set_terminator(Terminator::Return(Some(v)));
            }
            // Adjust for the leading capture slots — the implicit
            // `this` slot (when present) plus the by-DefId captures.
            let cap_offset = task.captures.len() + usize::from(task.needs_this);
            let local_id = fl.params[cap_offset + param_local_idx];
            fl.locals[local_id.0 as usize].default_init = Some(bb);
        }
        fl.finish()
    }

    pub(crate) fn lower_function(&mut self, def_id: DefId, f: &Function) -> Option<MirFunction> {
        let body = f.body.as_ref()?;
        let mut fl = FnLowerer::new(
            self.hir,
            &self.globals,
            &mut self.errors,
            &mut self.program.functions,
            &mut self.pending_lambdas,
            FunctionKind::User,
            f.name.clone(),
            Some(def_id),
            f.span,
            f.return_type.clone().unwrap_or(Type::Any),
        );
        for p in &f.params {
            let id = fl.declare_local(
                Some(p.name.clone()),
                p.ty.clone().unwrap_or(Type::Any),
                LocalKind::Param,
                p.span,
            );
            // `@x` reference param — slot is shared with caller's
            // local. `is_shared` triggers the interp to accept a
            // `Value::Cell` on entry; `is_by_ref` additionally
            // tells the call site to PROMOTE the caller's slot
            // before invoking.
            if p.is_by_ref {
                fl.locals[id.0 as usize].is_shared = true;
                fl.locals[id.0 as usize].is_by_ref = true;
            }
            fl.local_map.insert(p.def, id);
            fl.params.push(id);
        }
        // Param-binding charges, per call: at v1 every by-value param binds
        // through a `new Box(ai, …)` whose 2-arg ctor costs 1 op (upstream
        // also emits the equivalent static `ops(n)` for plain params); at
        // v2+ only params captured by a nested lambda get boxed. `@`-ref
        // params alias the caller's box — no charge either way.
        let v1_boxes = f.params.iter().filter(|p| !p.is_by_ref).count() as u64;
        let vn_boxes = f
            .params
            .iter()
            .filter(|p| !p.is_by_ref && captured_by_nested_lambda_stmts(&body.stmts, p.def))
            .count() as u64;
        if v1_boxes > 0 || vn_boxes > 0 {
            fl.push_stmt(Statement::ChargeVersioned {
                v1: v1_boxes,
                vn: vn_boxes,
            });
        }
        fl.lower_block_stmts(&body.stmts);
        fl.close_with_implicit_return(body.span);
        // Lower each param's default expression as a side block
        // ending in `Return(Some(value))`. The interpreter runs
        // these on a per-arg-shortfall basis. Defaults can refer
        // to earlier params (their locals are already in scope),
        // which is why the blocks live inside the same function.
        for (param_idx, p) in f.params.iter().enumerate() {
            let Some(default_expr) = &p.default else {
                continue;
            };
            let bb = fl.new_block();
            fl.resume(bb);
            let v = fl.lower_expr_to_operand(default_expr);
            if fl.is_open() {
                fl.set_terminator(Terminator::Return(Some(v)));
            }
            let local_id = fl.params[param_idx];
            fl.locals[local_id.0 as usize].default_init = Some(bb);
        }
        Some(fl.finish())
    }

    pub(crate) fn lower_class(&mut self, def_id: DefId, c: &leek_hir::Class) {
        let mut instance_fields: Vec<MirField> = Vec::new();
        let mut static_fields: Vec<MirField> = Vec::new();
        let parent = c.parent.clone();

        // Field initializers. Instance-field initializers run with
        // `this` available as a method-shaped first param; static
        // field initializers are nullary. The interpreter calls
        // these on demand (per-instance for instance fields, lazily
        // on first access for static).
        for f in &c.fields {
            let init_fn = f.init.as_ref().map(|init_expr| {
                self.lower_field_init(
                    init_expr,
                    f.span,
                    def_id,
                    c.name.clone(),
                    parent.clone(),
                    f.is_static,
                )
            });
            let field = MirField {
                name: f.name.clone(),
                init_fn,
                visibility: lower_visibility(f.visibility),
                is_final: f.is_final,
                ty: f.ty.clone().unwrap_or(Type::Any),
                span: f.span,
            };
            if f.is_static {
                static_fields.push(field);
            } else {
                instance_fields.push(field);
            }
        }

        let mut methods: Vec<MirMethod> = Vec::new();
        for m in &c.methods {
            let function_idx = self.lower_method(
                m,
                def_id,
                c.name.clone(),
                parent.clone(),
                /*is_constructor=*/ false,
            );
            methods.push(MirMethod {
                name: m.name.clone(),
                function_idx,
                is_static: m.is_static,
                user_arity: m.params.len(),
                visibility: lower_visibility(m.visibility),
                span: m.span,
            });
        }

        let mut constructors: Vec<MirMethod> = Vec::new();
        for m in &c.constructors {
            let function_idx = self.lower_method(
                m,
                def_id,
                c.name.clone(),
                parent.clone(),
                /*is_constructor=*/ true,
            );
            constructors.push(MirMethod {
                name: c.name.clone(),
                function_idx,
                is_static: false,
                user_arity: m.params.len(),
                visibility: lower_visibility(m.visibility),
                span: m.span,
            });
        }

        self.program.classes.push(MirClass {
            def_id,
            name: c.name.clone(),
            parent,
            parent_def: None,
            instance_fields,
            static_fields,
            methods,
            constructors,
            field_layout: Vec::new(),
            vtable: Vec::new(),
            span: c.span,
        });
    }

    pub(crate) fn lower_method(
        &mut self,
        m: &leek_hir::MethodDef,
        class_def_id: DefId,
        class_name: String,
        parent_class: Option<String>,
        _is_constructor: bool,
    ) -> usize {
        let function_idx = self.program.functions.len();
        // Reserve the slot so any nested lambdas push to later
        // slots without clashing with this method's eventual index.
        self.program.functions.push(placeholder_function(m.span));

        let mut fl = FnLowerer::new(
            self.hir,
            &self.globals,
            &mut self.errors,
            &mut self.program.functions,
            &mut self.pending_lambdas,
            FunctionKind::User,
            format!("{class_name}::{}", m.name),
            Some(m.def),
            m.span,
            m.return_type.clone().unwrap_or(Type::Any),
        );

        // Synthetic `this` param for instance methods. Static
        // methods get nothing — `this` references inside a static
        // method are a static error in upstream, but we model them
        // as runtime nulls so we don't crash the lowerer.
        let this_local = if m.is_static {
            None
        } else {
            let id = fl.declare_local(
                Some("this".into()),
                Type::ClassInstance(class_name.clone(), Vec::new()),
                LocalKind::Param,
                m.span,
            );
            fl.params.push(id);
            Some(id)
        };

        fl.method_ctx = Some(MethodCtx {
            this_local,
            class_def_id,
            class_name,
            parent_class,
        });

        // Declare user-visible params.
        for p in &m.params {
            let id = fl.declare_local(
                Some(p.name.clone()),
                p.ty.clone().unwrap_or(Type::Any),
                LocalKind::Param,
                p.span,
            );
            if p.is_by_ref {
                fl.locals[id.0 as usize].is_shared = true;
                fl.locals[id.0 as usize].is_by_ref = true;
            }
            fl.local_map.insert(p.def, id);
            fl.params.push(id);
        }

        if let Some(body) = &m.body {
            fl.lower_block_stmts(&body.stmts);
            fl.close_with_implicit_return(body.span);
        } else {
            fl.close_with_implicit_return(m.span);
        }

        // Default-init blocks for params.
        for (param_local_idx, p) in m.params.iter().enumerate() {
            let Some(default_expr) = &p.default else {
                continue;
            };
            let bb = fl.new_block();
            fl.resume(bb);
            let v = fl.lower_expr_to_operand(default_expr);
            if fl.is_open() {
                fl.set_terminator(Terminator::Return(Some(v)));
            }
            // Account for the leading `this` slot.
            let this_slots = usize::from(this_local.is_some());
            let local_id = fl.params[this_slots + param_local_idx];
            fl.locals[local_id.0 as usize].default_init = Some(bb);
        }

        let func = fl.finish();
        self.program.functions[function_idx] = func;
        function_idx
    }

    pub(crate) fn lower_field_init(
        &mut self,
        init_expr: &Expr,
        span: Span,
        class_def_id: DefId,
        class_name: String,
        parent_class: Option<String>,
        is_static: bool,
    ) -> usize {
        let function_idx = self.program.functions.len();
        self.program.functions.push(placeholder_function(span));

        let mut fl = FnLowerer::new(
            self.hir,
            &self.globals,
            &mut self.errors,
            &mut self.program.functions,
            &mut self.pending_lambdas,
            FunctionKind::User,
            format!("{class_name}::<field-init>"),
            None,
            span,
            Type::Any,
        );

        let this_local = if is_static {
            None
        } else {
            let id = fl.declare_local(
                Some("this".into()),
                Type::ClassInstance(class_name.clone(), Vec::new()),
                LocalKind::Param,
                span,
            );
            fl.params.push(id);
            Some(id)
        };
        fl.method_ctx = Some(MethodCtx {
            this_local,
            class_def_id,
            class_name,
            parent_class,
        });

        let v = fl.lower_expr_to_operand(init_expr);
        if fl.is_open() {
            fl.set_terminator(Terminator::Return(Some(v)));
        }
        fl.close_with_implicit_return(span);

        let func = fl.finish();
        self.program.functions[function_idx] = func;
        function_idx
    }

    pub(crate) fn lower_main(&mut self) -> MirFunction {
        let main_span = self
            .hir
            .main
            .first()
            .map_or_else(Span::synthetic, Stmt::span);
        let mut fl = FnLowerer::new(
            self.hir,
            &self.globals,
            &mut self.errors,
            &mut self.program.functions,
            &mut self.pending_lambdas,
            FunctionKind::Main,
            "<main>".into(),
            None,
            main_span,
            Type::Any,
        );
        // Upstream Leekscript yields the value of the trailing
        // top-level expression when the program has no explicit
        // `return` — `f();` at the bottom of main returns whatever
        // `f()` produced. Lower that by splitting the trailing
        // `Stmt::Expr` and turning it into an implicit return.
        let main = &self.hir.main;
        if let Some((Stmt::Expr(last), head)) = main.split_last() {
            fl.lower_block_stmts(head);
            if fl.is_open() {
                let value = fl.lower_expr_to_operand(last);
                if fl.is_open() {
                    fl.set_terminator(Terminator::Return(Some(value)));
                }
            }
        } else {
            fl.lower_block_stmts(main);
        }
        fl.close_with_implicit_return(main_span);
        fl.finish()
    }
}
