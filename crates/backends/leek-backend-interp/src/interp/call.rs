//! MIR interpreter — call.

use std::cell::RefCell;
use std::rc::Rc;

use leek_mir::{
    BasicBlock, BlockId, CallExpr, Callee, MirFunction, MirProgram, Operand,
};

use crate::value::{Function as FnValue, Value};

use super::value::{coerce_to_type, builtin_class_name, construct_builtin_class};
use super::{Interpreter, Outcome, StepResult};

impl<'a> Interpreter<'a> {
    pub(crate) fn function(&self, idx: usize) -> &'a MirFunction {
        let program: &'a MirProgram = self.program;
        &program.functions[idx]
    }

    pub(crate) fn run_function(&mut self, idx: usize, args: Vec<Value>) -> Result<Value, Outcome> {
        // Guard against runaway recursion before the host stack
        // overflows. Upstream's interpreter dies with
        // `STACK_OVERFLOW` at roughly the same depth; we map our
        // bail-out to that error code so corpus tests asserting
        // the error pass.
        // Generous enough for typical recursive corpus tests
        // (factorial chains, tail-recursive sums up to ~1000
        // levels) — matches Java's default thread stack size
        // for the upstream sandbox. Tuned against the corpus
        // recursion fixtures rather than to a fixed multiple of
        // the host stack.
        const MAX_CALL_DEPTH: u32 = 2000;
        if self.call_depth >= MAX_CALL_DEPTH {
            return Err(Outcome::Error("STACK_OVERFLOW".into()));
        }
        self.call_depth += 1;
        // Upstream charges 1 op on entry to a *called* function body
        // (the call site itself is free — the callee pays for its own
        // work). The synthetic `<main>` frame is exempt so a top-level
        // `var x = 42 return x` stays at 1 op.
        if self.main_idx != Some(idx)
            && let Some(o) = self.charge_ops(1)
        {
            self.call_depth -= 1;
            return Err(o);
        }
        let result = self.run_function_inner(idx, args);
        self.call_depth -= 1;
        result
    }

    pub(crate) fn run_function_inner(
        &mut self,
        idx: usize,
        args: Vec<Value>,
    ) -> Result<Value, Outcome> {
        self.function_stack.push(idx);
        if self.profiler.is_some() {
            let name = self
                .program
                .functions
                .get(idx).map_or_else(|| format!("<fn#{idx}>"), |f| f.name.clone());
            let ops_now = self.op_count;
            // Borrow split: take the profiler out, call, put it
            // back. Avoids overlapping borrows of `self`.
            if let Some(mut p) = self.profiler.take() {
                p.enter(name, ops_now);
                self.profiler = Some(p);
            }
        }
        let r = self.run_function_body(idx, args);
        self.function_stack.pop();
        if self.profiler.is_some() {
            let ops_now = self.op_count;
            if let Some(mut p) = self.profiler.take() {
                p.exit(ops_now);
                self.profiler = Some(p);
            }
        }
        r
    }

    pub(crate) fn run_function_body(
        &mut self,
        idx: usize,
        args: Vec<Value>,
    ) -> Result<Value, Outcome> {
        let f = self.function(idx);
        let mut locals: Vec<Value> = vec![Value::Null; f.locals.len()];
        // Wrap every captured local in a fresh `Value::Cell` so
        // closure captures point at a shared `Rc<RefCell<Value>>`.
        // Param slots that ARE the capture slot (passed in from a
        // parent frame as an already-Cell value) will overwrite
        // this in the loop below.
        for (slot, decl) in f.locals.iter().enumerate() {
            if decl.is_shared {
                locals[slot] = Value::Cell(Rc::new(RefCell::new(Value::Null)));
            }
        }
        let argc = args.len();
        for (i, arg) in args.into_iter().enumerate() {
            if i >= f.params.len() {
                break;
            }
            let slot = f.params[i].0 as usize;
            let ty = &f.locals[slot].ty;
            // A capture slot — the caller passes in the
            // already-wrapped `Value::Cell`. Use the caller's cell
            // directly so the closure shares the outer scope's
            // storage. For ordinary params just coerce + assign.
            if matches!(arg, Value::Cell(_)) {
                locals[slot] = arg;
            } else if let Value::Cell(cell) = &locals[slot] {
                *cell.borrow_mut() = coerce_to_type(&arg, ty);
            } else {
                locals[slot] = coerce_to_type(&arg, ty);
            }
        }
        // For each parameter the caller didn't provide, run its
        // default-init block (if any) to compute the value.
        // Defaults can reference earlier params, so we walk in
        // declaration order.
        for i in argc..f.params.len() {
            let slot = f.params[i].0 as usize;
            if let Some(default_bb) = f.locals[slot].default_init {
                let v = self.run_blocks_from(f, &mut locals, default_bb)?;
                let ty = &f.locals[slot].ty;
                if let Value::Cell(cell) = &locals[slot] {
                    *cell.borrow_mut() = coerce_to_type(&v, ty);
                } else {
                    locals[slot] = coerce_to_type(&v, ty);
                }
            }
        }

        let v = self.run_blocks_from(f, &mut locals, f.entry)?;
        Ok(coerce_to_type(&v, &f.return_ty))
    }

    /// Execute basic blocks starting at `start`, returning the
    /// value of the first `Return` terminator hit. `locals` is
    /// shared across the run so defaults can write into param
    /// slots that the body subsequently reads.
    pub(crate) fn run_blocks_from(
        &mut self,
        f: &'a MirFunction,
        locals: &mut Vec<Value>,
        start: BlockId,
    ) -> Result<Value, Outcome> {
        let mut block = start;
        loop {
            let bb: &'a BasicBlock = &f.blocks[block.0 as usize];
            for stmt in &bb.statements {
                // Internal-only per-statement tick — keeps loops on
                // a runtime clock without polluting the user-facing
                // op count.
                if self.block_tick().is_some() {
                    return Err(Outcome::Error("TOO_MUCH_OPERATIONS".into()));
                }
                self.exec_stmt(locals, stmt)?;
            }
            if self.block_tick().is_some() {
                return Err(Outcome::Error("TOO_MUCH_OPERATIONS".into()));
            }
            match self.exec_terminator(locals, &bb.terminator)? {
                StepResult::Goto(next) => block = next,
                StepResult::Return(v) => return Ok(v),
            }
        }
    }

    // ---- Statements ----

    #[allow(clippy::ptr_arg)] // `locals` frame stack threaded as `&mut Vec`; see `write_place`
    pub(crate) fn exec_call(
        &mut self,
        locals: &mut Vec<Value>,
        call: &CallExpr,
    ) -> Result<Value, Outcome> {
        // For Function/Indirect callees we may need to wrap caller
        // slots in `Value::Cell` for `@x` params. Determine the
        // target's MIR function up-front to inspect its params.
        let target_fn_idx: Option<usize> = match &call.callee {
            Callee::Function(def) => self.fn_by_def.get(def).copied(),
            Callee::Indirect(local) => {
                let v = locals
                    .get(local.0 as usize)
                    .cloned()
                    .unwrap_or(Value::Null)
                    .unbox();
                match v {
                    Value::Function(FnValue::User(def)) => self.fn_by_def.get(&def).copied(),
                    // Lambdas carry the function index directly;
                    // their captures occupy the first slots of the
                    // function's params list, so by-ref-arg
                    // detection needs to account for that offset.
                    Value::Function(FnValue::Lambda(cap)) => Some(cap.function_idx),
                    _ => None,
                }
            }
            _ => None,
        };
        // Lambda functions interleave capture slots before
        // user-visible params — find the user-param start so the
        // `is_by_ref` lookup below maps call args correctly.
        // `@a` on a *lambda* param is only honored in v1; v2+
        // treats it as a no-op (see `TestReference.run::3`).
        let (user_param_start, is_lambda_call) = match &call.callee {
            Callee::Indirect(local) => {
                let v = locals
                    .get(local.0 as usize)
                    .cloned()
                    .unwrap_or(Value::Null)
                    .unbox();
                if let Value::Function(FnValue::Lambda(cap)) = v {
                    (cap.captured.borrow().len(), true)
                } else {
                    (0, false)
                }
            }
            _ => (0, false),
        };
        let suppress_byref = is_lambda_call && self.version > 1;
        // For user functions, peek the param `is_by_ref` flags so
        // we can promote any matching arg slot before evaluation.
        // (`is_shared` alone — set when a nested closure captures
        // the local — does NOT trigger promotion: the closure
        // shares with the callee's frame, not the caller's.)
        let by_ref_flags: Vec<bool> = if suppress_byref {
            Vec::new()
        } else if let Some(idx) = target_fn_idx {
            let f = self.function(idx);
            f.params
                .iter()
                .skip(user_param_start)
                .map(|pid| f.locals[pid.0 as usize].is_by_ref)
                .collect()
        } else {
            Vec::new()
        };
        for (i, arg) in call.args.iter().enumerate() {
            if by_ref_flags.get(i).copied().unwrap_or(false) {
                if let Operand::Local(id) = arg {
                    let slot = id.0 as usize;
                    if let Some(v) = locals.get_mut(slot) {
                        if !matches!(v, Value::Cell(_)) {
                            let prior = std::mem::replace(v, Value::Null);
                            *v = Value::Cell(Rc::new(RefCell::new(prior)));
                        }
                    }
                }
            }
        }
        // Pre-evaluate args, then dispatch. For by-ref params the
        // raw read returns the freshly-installed `Value::Cell`. v1
        // `LegacyArray` semantics treat regular (non-@) array/map
        // args as pass-by-value, so we clone them at the boundary.
        let v1_clone_args = self.version <= 1;
        let args: Vec<Value> = call
            .args
            .iter()
            .enumerate()
            .map(|(i, o)| {
                let is_by_ref = by_ref_flags.get(i).copied().unwrap_or(false);
                if is_by_ref {
                    self.read_operand_raw(locals, o)
                } else {
                    let v = self.read_operand(locals, o);
                    if v1_clone_args && target_fn_idx.is_some() {
                        leek_runtime::deep_clone_for_v1(&v)
                    } else {
                        v
                    }
                }
            })
            .collect();
        match &call.callee {
            Callee::Function(def) => match self.fn_by_def.get(def).copied() {
                Some(idx) => self.run_function(idx, args),
                // No compiled body: a signature-file builtin dispatches the
                // named runtime builtin instead.
                None => match self.bodiless_builtins.get(def).cloned() {
                    Some(name) => self.run_builtin(&name, &args),
                    None => Err(Outcome::Error(format!(
                        "call to unknown user function {def}"
                    ))),
                },
            },
            Callee::Builtin(name) => {
                // User code can shadow a builtin name by
                // assignment (`isEmpty = function() {...}`). When
                // that happened the global store has the new
                // callable; route through `call_value` so it gets
                // dispatched correctly.
                if let Some(v) = self.globals.get(name).cloned() {
                    return self.call_value(&v, args);
                }
                // Built-in class names (`Array`, `Map`, `Set`, etc.)
                // used as bare calls are sugar for constructing the
                // class — `Array(1, 2, 3)` is `[1, 2, 3]`. Intercept
                // these before the function-style builtin dispatch
                // which has no entry for them.
                if let Some(canonical) = builtin_class_name(name) {
                    Ok(construct_builtin_class(canonical, args))
                } else {
                    self.run_builtin(name, &args)
                }
            }
            Callee::Method { receiver, method } => {
                let recv = locals
                    .get(receiver.0 as usize)
                    .cloned()
                    .unwrap_or(Value::Null)
                    .unbox();
                self.dispatch_method_call(&recv, method, args)
            }
            Callee::Indirect(local) => {
                let callee = locals
                    .get(local.0 as usize)
                    .cloned()
                    .unwrap_or(Value::Null)
                    .unbox();
                self.call_value(&callee, args)
            }
            Callee::SuperConstructor { this, parent_class } => {
                let this_val = locals
                    .get(this.0 as usize)
                    .cloned()
                    .unwrap_or(Value::Null)
                    .unbox();
                // When the parent class declares no explicit
                // constructor, `super()` is a no-op (matches upstream
                // — the default ctor is implicitly synthesised).
                let Some(ctor_idx) = self.find_constructor(parent_class, args.len()) else {
                    return Ok(Value::Null);
                };
                let mut full = Vec::with_capacity(args.len() + 1);
                full.push(this_val);
                full.extend(args);
                self.run_function(ctor_idx, full)
            }
        }
    }

    /// Dispatch `receiver.method(args)` for the given receiver
    /// kind. Instance methods climb the receiver's class chain;
    /// `super` dispatches against a statically-fixed parent class;
    /// `ClassRef` looks up a static method on the class.
    /// Everything else falls back to the builtin
    /// `name(receiver, args…)` sugar.
    pub(crate) fn dispatch_method_call(
        &mut self,
        receiver: &Value,
        method: &str,
        args: Vec<Value>,
    ) -> Result<Value, Outcome> {
        match receiver {
            Value::Instance(inst) => {
                let class_name = inst.borrow().class_name.clone();
                let class_def = inst.borrow().class;
                if let Some(fn_idx) = self.find_method_arity(&class_name, method, Some(args.len()))
                {
                    if let Some((owner, vis)) = self.method_visibility(class_def, method, false) {
                        if !self.member_visible(owner, vis) {
                            return Ok(Value::Null);
                        }
                    }
                    let mut full = Vec::with_capacity(args.len() + 1);
                    full.push(receiver.clone());
                    full.extend(args);
                    return self.run_function(fn_idx, full);
                }
                // No method by that name — maybe it's a callable
                // held in an instance FIELD (`class X { x = A.m }`,
                // then `new X().x(...)`). Read the field and
                // dispatch through `call_value`.
                let field_val = inst.borrow().fields.get(method).cloned();
                if let Some(v) = field_val {
                    if matches!(v, Value::Function(_)) {
                        return self.call_value(&v, args);
                    }
                }
                // Not on this class — fall back to builtins for
                // `obj.toString()`-style methods upstream provides
                // on every value.
                self.run_method(receiver, method, &args)
            }
            Value::Object(o) => {
                // Plain object: look up the field; if it holds a
                // callable (function, class reference, or builtin
                // class), invoke it with the given args (no
                // implicit `this`, matching upstream's record-style
                // semantics).
                let v = o.borrow().get(method).cloned();
                if let Some(v) = v {
                    if matches!(
                        v,
                        Value::Function(_) | Value::ClassRef(_, _) | Value::BuiltinClass(_)
                    ) {
                        return self.call_value(&v, args);
                    }
                }
                self.run_method(receiver, method, &args)
            }
            Value::Super {
                parent_class,
                receiver: inner,
            } => {
                if let Some(fn_idx) = self.find_method(parent_class, method) {
                    let mut full = Vec::with_capacity(args.len() + 1);
                    full.push((**inner).clone());
                    full.extend(args);
                    return self.run_function(fn_idx, full);
                }
                Err(Outcome::Error(format!(
                    "super.{method}: parent class `{parent_class}` has no such method"
                )))
            }
            Value::ClassRef(class_def, _) => {
                let class_name = self
                    .program
                    .class(*class_def)
                    .map(|c| c.name.clone())
                    .unwrap_or_default();
                if let Some(fn_idx) =
                    self.find_static_method_arity(&class_name, method, Some(args.len()))
                {
                    if let Some((owner, vis)) = self.method_visibility(*class_def, method, true) {
                        if !self.member_visible(owner, vis) {
                            return Ok(Value::Null);
                        }
                    }
                    return self.run_function(fn_idx, args);
                }
                // No static method by that name — maybe it's a
                // callable held in a static FIELD (`static f =
                // function() {...}`). Read the field and dispatch
                // through `call_value`.
                if self.has_static_field(*class_def, method) {
                    let v = self.read_static_field(*class_def, method);
                    return self.call_value(&v, args);
                }
                self.run_method(receiver, method, &args)
            }
            _ => self.run_method(receiver, method, &args),
        }
    }

    /// `DefId` of the class whose method/constructor/field-init we
    /// are currently inside. `None` when running plain user code
    /// (top-level / a non-method function).
    pub(crate) fn param_byref_mask(&self, callee: &Value) -> Option<Vec<bool>> {
        match callee {
            Value::Function(FnValue::Lambda(cap)) => {
                let f = self.function(cap.function_idx);
                let captures = cap.captured.borrow().len();
                let user_start = captures;
                Some(
                    f.params[user_start..]
                        .iter()
                        .map(|pid| f.locals[pid.0 as usize].is_by_ref)
                        .collect(),
                )
            }
            Value::Function(FnValue::User(def)) => {
                let idx = *self.fn_by_def.get(def)?;
                let f = self.function(idx);
                Some(
                    f.params
                        .iter()
                        .map(|pid| f.locals[pid.0 as usize].is_by_ref)
                        .collect(),
                )
            }
            _ => None,
        }
    }

    pub(crate) fn callback_arity(&self, callee: &Value) -> Option<usize> {
        match callee {
            Value::Function(FnValue::Lambda(cap)) => {
                let f = self.function(cap.function_idx);
                let captures = cap.captured.borrow().len();
                Some(f.params.len().saturating_sub(captures))
            }
            Value::Function(FnValue::User(def)) => self
                .fn_by_def
                .get(def)
                .copied()
                .map(|idx| self.function(idx).params.len()),
            Value::Function(FnValue::Builtin(name)) => leek_runtime::builtin_arity(name),
            _ => None,
        }
    }

    /// Public callable used by the stdlib builtins in `leek_runtime` for higher-order
    /// functions (`arrayMap`, `arrayReduce`, etc.).
    pub(crate) fn call_value(
        &mut self,
        callee: &Value,
        args: Vec<Value>,
    ) -> Result<Value, Outcome> {
        match callee {
            Value::Function(FnValue::User(id)) => match self.fn_by_def.get(id).copied() {
                Some(idx) => {
                    // Methods called via an indirect function ref
                    // (`var f = A.m; f(...)`) require exact arity
                    // including the implicit `this` receiver —
                    // upstream returns `null` on mismatch instead
                    // of binding missing params to null.
                    let f = self.function(idx);
                    if f.owning_class.is_some() && args.len() != f.params.len() {
                        return Ok(Value::Null);
                    }
                    self.run_function(idx, args)
                }
                None => Ok(Value::Null),
            },
            Value::Function(FnValue::Builtin(name)) => {
                // Indirect builtin calls (`[cos][0]()`) don't get
                // the compile-time default-arg injection that direct
                // calls do. If the builtin needs at least one arg
                // and we have none, return `null` instead of falling
                // through to the 0-default math dispatch.
                if args.is_empty() && leek_runtime::needs_at_least_one_arg(name) {
                    return Ok(Value::Null);
                }
                self.run_builtin(name, &args)
            }
            Value::Function(FnValue::Lambda(cap)) => {
                // Prepend captured values to the user args. The
                // lambda's MirFunction has its capture slots first;
                // run_function sees the combined vector as `args`
                // and binds positionally.
                let mut full_args = cap.captured.borrow().clone();
                full_args.extend(args);
                self.run_function(cap.function_idx, full_args)
            }
            Value::Function(FnValue::BoundMethod {
                function_idx,
                receiver,
            }) => {
                // BoundMethod usually prepends the stored receiver
                // — `t['test_fct'](15)` → `m(t, 15)`. But when the
                // caller passes one EXTRA arg over the method's
                // user-arity, treat the first arg as the receiver
                // and ignore the stored one — that's the shape
                // `[a['m']][0](a, 5)` expects (m has 1 user param,
                // call has 2 args).
                let params = self.function(*function_idx).params.len();
                let user_arity = params.saturating_sub(1);
                if args.len() == user_arity + 1 {
                    return self.run_function(*function_idx, args);
                }
                let mut full = Vec::with_capacity(args.len() + 1);
                full.push((**receiver).clone());
                full.extend(args);
                self.run_function(*function_idx, full)
            }
            Value::BuiltinClass(name) => Ok(construct_builtin_class(name, args)),
            Value::ClassRef(class_def, _) => {
                let class_name = self
                    .program
                    .class(*class_def)
                    .map(|c| c.name.clone())
                    .ok_or_else(|| Outcome::Error("unknown ClassRef".into()))?;
                self.construct_user_class(&class_name, args)
            }
            _ => Ok(Value::Null),
        }
    }

    /// Internal-only counter for block transitions. Doesn't
    /// contribute to `op_count` (so `.ops(N)` tests stay aligned
    /// with upstream's per-expression model), but caps total
    /// blocks executed so a `while (true) {}` body still aborts.
    pub(crate) fn block_tick(&mut self) -> Option<()> {
        self.block_count = self.block_count.saturating_add(1);
        // Cap equal to the op budget keeps stress-test pathological
        // loops from running for seconds while still admitting
        // legitimate tight iterations (each loop iter has at least
        // one cond+body block).
        let cap = self.op_limit.unwrap_or(u64::MAX);
        if self.block_count > cap {
            Some(())
        } else {
            None
        }
    }

    /// Debit `n` operations against the budget. Returns
    /// `Some(Outcome::Error("TOO_MUCH_OPERATIONS"))` when the
    /// limit is exceeded. Used by builtin dispatch to charge each
    /// call its registered cost (see `builtins::builtin_cost`).
    pub(crate) fn charge_ops(&mut self, n: u64) -> Option<Outcome> {
        self.op_count = self.op_count.saturating_add(n);
        if let Some(limit) = self.op_limit
            && self.op_count > limit
        {
            return Some(Outcome::Error("TOO_MUCH_OPERATIONS".into()));
        }
        None
    }

    /// Charge a builtin's op cost, then dispatch it through the shared
    /// (host-based) catalog, converting any callback control-flow back to
    /// an `Outcome`.
    pub(crate) fn run_builtin(&mut self, name: &str, args: &[Value]) -> Result<Value, Outcome> {
        let cost = leek_runtime::builtin_op_cost(name, args, self.version);
        if let Some(o) = self.charge_ops(cost) {
            return Err(o);
        }
        leek_runtime::call_builtin(self, name, args).map_err(flow_to_outcome)
    }

    /// `receiver.method(args)` — builtin-method sugar for
    /// `method(receiver, args…)`.
    pub(crate) fn run_method(
        &mut self,
        receiver: &Value,
        method: &str,
        args: &[Value],
    ) -> Result<Value, Outcome> {
        let mut combined = Vec::with_capacity(args.len() + 1);
        combined.push(receiver.clone());
        combined.extend(args.iter().cloned());
        self.run_builtin(method, &combined)
    }
}

/// Convert a builtin-callback control-flow signal back into the
/// interpreter's `Outcome`.
pub(crate) fn flow_to_outcome(flow: leek_runtime::BuiltinFlow) -> Outcome {
    use leek_runtime::BuiltinFlow;
    match flow {
        BuiltinFlow::Return(v) => Outcome::Return(v),
        BuiltinFlow::Break => Outcome::Break,
        BuiltinFlow::Continue => Outcome::Continue,
        BuiltinFlow::Error(s) => Outcome::Error(s),
    }
}

/// The interpreter satisfies the stdlib builtins' [`BuiltinHost`] needs by
/// delegating to its own state. This is what lets the builtin catalog be
/// written against `leek-runtime` alone (so the native backend can supply
/// its own trivial host).
impl leek_runtime::BuiltinHost for Interpreter<'_> {
    fn version(&self) -> u8 {
        self.version
    }

    fn rng_int(&mut self, lo: i64, hi: i64) -> i64 {
        self.rng.int_in(lo, hi)
    }

    fn rng_real(&mut self, lo: f64, hi: f64) -> f64 {
        self.rng.real_in(lo, hi)
    }

    fn callback_arity(&self, callee: &Value) -> Option<usize> {
        Interpreter::callback_arity(self, callee)
    }

    fn param_byref_mask(&self, callee: &Value) -> Option<Vec<bool>> {
        Interpreter::param_byref_mask(self, callee)
    }

    fn call_value(
        &mut self,
        callee: &Value,
        args: Vec<Value>,
    ) -> Result<Value, leek_runtime::BuiltinFlow> {
        use leek_runtime::BuiltinFlow;
        Interpreter::call_value(self, callee, args).map_err(|o| match o {
            Outcome::Return(v) => BuiltinFlow::Return(v),
            Outcome::Break => BuiltinFlow::Break,
            Outcome::Continue => BuiltinFlow::Continue,
            Outcome::Error(s) => BuiltinFlow::Error(s),
            Outcome::Next => BuiltinFlow::Error("unexpected control flow".into()),
        })
    }
}
