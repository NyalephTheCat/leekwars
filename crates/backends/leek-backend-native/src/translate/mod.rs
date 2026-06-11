//! MIR → Cranelift IR translation for the scalar (integer / real /
//! boolean) + control-flow subset. Anything outside that subset returns
//! [`NativeError::Unsupported`] so callers can fall back / skip.

use std::collections::{HashMap, HashSet};

use cranelift::codegen;
use cranelift::codegen::ir::FuncRef;
use cranelift::prelude::{
    AbiParam, Block, FloatCC, InstBuilder, IntCC, MemFlags, StackSlotData, StackSlotKind, TrapCode,
    Type as ClType, Value, types,
};
use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext, Variable};
use cranelift_module::{Linkage, Module};

use leek_hir::DefId;
use leek_mir::ir::{
    BasicBlock, BinOp, BlockId, Callee, CastKind, Const, FunctionKind, LocalDecl, LocalId,
    LocalKind, MirFunction, MirProgram, Operand, Place, Rvalue, SetElem, Statement, Terminator,
    UnOp, Visibility,
};
use leek_runtime::MathSig;
use leek_types::Type;

use crate::options::NativeError;

mod imports;
use imports::declare_imports;

mod type_infer;
use type_infer::infer_local_tys;

mod builtins;
use builtins::{is_dispatchable_builtin, is_generic_builtin};

mod analysis;
use analysis::{
    byref_arg_cell_locals, byref_captured_arg_cell_locals, byref_cell_params,
    byref_param_escape_threadable, dynamic_method_targets, index_method_targets,
    indirect_arg_cell_locals, noop_byref_params, virtual_method_targets,
};
// Surface the three passes `lib.rs` drives at the `translate::` path.
pub use analysis::{lambda_body_idxs, method_value_info, needs_cell_semantics};

mod classes;
use classes::{
    aliased_class_locals, builtin_ancestor, class_extends_builtin, class_reflect, classref_locals,
    new_class_locals, object_field_srcs, object_locals, program_writes_global, receiver_class,
    resolve_instance_method_value, resolve_static_field, resolve_static_method,
    resolve_static_method_value, static_field_accesses, static_method_value_refs, super_locals,
};
// Surface the resolution tables `lib.rs` builds at the `translate::` path.
pub use classes::{function_ref_info, static_field_info, static_method_value_info};

/// Static value kind we track so the result can be wrapped back into the
/// right [`leek_runtime::Value`]. `Int`/`Bool` are `i64` (bool as 0/1) in
/// the generated code; `Real` is `f64`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ValTy {
    Int,
    Bool,
    Real,
    /// A handle (`*mut leek_runtime::Value`, held as an `i64`) to a heap
    /// value — arrays and other composites, plus boxed scalars. The
    /// universal fallback kind: any value can be represented as a `Ref`.
    Ref,
}

impl ValTy {
    /// Cranelift type used to represent this kind.
    pub fn cl_type(self) -> ClType {
        match self {
            ValTy::Int | ValTy::Bool | ValTy::Ref => types::I64,
            ValTy::Real => types::F64,
        }
    }
}

/// Leekscript language semantics the generated code must honor.
#[derive(Debug, Clone, Copy)]
pub struct Lang {
    /// Language version (1–4).
    pub version: u8,
    /// Strict typing — untyped `var` slots coerce to their inferred type.
    pub strict: bool,
}

/// A user function's scalar calling convention: parameter kinds + result.
#[derive(Debug, Clone)]
pub struct FnSig {
    pub params: Vec<ValTy>,
    pub ret: ValTy,
    /// True when the function has ≥1 parameter with a *non-constant* default
    /// (one that references earlier params, calls a function, etc.). Such a
    /// function takes a hidden trailing `i64` `argc` parameter: the callee
    /// fills every omitted defaulted param by running its `default_init`
    /// block at entry (see `translate_function`). Constant-only defaults are
    /// still padded at the call site, so those functions keep `has_defaults`
    /// false and the unchanged ABI.
    pub has_defaults: bool,
}

/// Per-function result kinds, keyed by `DefId`. Built by [`compute_fn_rets`]
/// and consulted when typing a local assigned from a function call.
pub type FnRets = HashMap<DefId, ValTy>;

/// Compute every user function's result kind via a fixpoint: seed each at
/// `Int`, then repeatedly recompute from return operands (resolving
/// call-result kinds through the current estimates) until stable. Handles
/// (mutual) recursion. `main` has no `DefId`, so it's excluded.
pub fn compute_fn_rets(program: &MirProgram, lang: Lang) -> FnRets {
    let mut rets: FnRets = HashMap::new();
    for f in &program.functions {
        if let Some(id) = f.def_id {
            rets.insert(id, ValTy::Int);
        }
    }
    loop {
        let mut changed = false;
        for f in &program.functions {
            let Some(id) = f.def_id else { continue };
            // Inference may bail (non-scalar function); such functions
            // can't be compiled anyway, so leave their estimate as-is.
            let Ok(tys) = infer_local_tys(f, lang, &rets, false, program) else {
                continue;
            };
            // Must match `function_sig`: a concrete declared return type
            // coerces the result, otherwise it's the operands' join.
            let new = scalar_valty(&f.return_ty).unwrap_or_else(|| ret_valty(f, &tys));
            if rets.get(&id) != Some(&new) {
                rets.insert(id, new);
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    rets
}

/// Parameter type specialization. An untyped (`Any`) parameter of an eligible
/// free function that is provably ONLY ever passed one scalar kind — at every
/// call site, program-wide — has its declared type pinned to that kind
/// (`integer` or `real`), so the rest of the backend compiles it **unboxed**
/// (no per-operation boxing). This closes the gap on call-heavy numeric code:
/// with `fib`'s `n` unboxed, `n - 1` / `n < 2` become register `isub`/`icmp`
/// instead of allocating shims.
///
/// Mutates `program` in place — the native backend owns its freshly-lowered MIR,
/// so the interpreter/Java backends (which lower separately) are unaffected.
///
/// Soundness: pinning an `Any` param to `integer`/`real` makes it coerce its
/// argument to that kind. The coercion is a no-op precisely when the argument
/// is already that kind — so we pin a param ONLY when every call site provably
/// passes one. We find that set per kind by an *optimistic* fixpoint: assume
/// every candidate is the kind, then demote any contradicted by a call site,
/// until stable (recursion like `fib`, where `n`'s type feeds `fib(n-1)`'s
/// argument, needs the optimism). `integer` and `real` are incomparable, so we
/// run two passes — `integer` first, then `real` over whatever stayed `Any`.
/// Codegen runs after convergence on the final (sound) types; the corpus
/// regression is the backstop.
pub fn specialize_param_types(program: &mut MirProgram, lang: Lang) {
    use leek_mir::ir::{Const, FunctionKind};

    // Functions whose value is taken (`var f = myFn`) can be invoked indirectly
    // with arguments we can't see — never specialize their params.
    let mut escaped: HashSet<DefId> = HashSet::new();
    for f in &program.functions {
        for s in f.blocks.iter().flat_map(|b| &b.statements) {
            if let Statement::Assign(_, Rvalue::FunctionRef(d)) = s {
                escaped.insert(*d);
            }
        }
    }

    // A function whose callee fills a NON-const default uses the intricate
    // `has_defaults` ABI — see `function_sig`; we don't specialize its params.
    // (A const default is materialized at the call site, so it's fine.)
    let fills_nonconst_default = |f: &MirFunction| {
        f.params.iter().any(|&p| {
            fillable_default(f, p).is_some()
                && const_default(f, p).is_none()
                && const_eval_default(f, p, lang.version).is_none()
        })
    };
    // An untyped, read-only, value-use-only required param (no default of its
    // own, not shared/by-ref) — the shape we can pin to an unboxed scalar.
    let eligible_param = |f: &MirFunction, pid: LocalId| {
        let d = &f.locals[pid.0 as usize];
        matches!(d.ty, Type::Any)
            && d.default_init.is_none()
            && !d.is_shared
            && !d.is_by_ref
            && param_value_only(f, pid)
    };

    // Candidate (fn index, param local, ARG index, def id). For a free function
    // the arg index is the param position; for a method it's `position - 1`
    // (the receiver `this` is `params[0]`, not part of the call's `args`).
    let mut candidates: Vec<(usize, LocalId, usize, DefId)> = Vec::new();
    for (fi, f) in program.functions.iter().enumerate() {
        let Some(def) = f.def_id else { continue };
        if f.owning_class.is_some() || f.kind == FunctionKind::Main || escaped.contains(&def) {
            continue;
        }
        if fills_nonconst_default(f) {
            continue;
        }
        for (pi, &pid) in f.params.iter().enumerate() {
            if eligible_param(f, pid) {
                candidates.push((fi, pid, pi, def));
            }
        }
    }

    // --- Methods (Phase 3) -------------------------------------------------
    // A `recv.m(args)` call dispatches by method name with virtual overrides, so
    // attributing a call site's args to one method is only sound when the method
    // is unambiguous. We require it to be a method of a *standalone* class (no
    // parent, never extended → no overrides, no `super.m()`), with a name that is
    // GLOBALLY UNIQUE among user methods, never shadowed by a field, and never
    // read as a value (`obj.m` / `obj['m']` → a bound method callable later with
    // unseen args). Under those gates EVERY `Callee::Method{ method: m }` site
    // provably calls this one method, so its user params infer just like a free
    // function's. `method_def` maps such names to the method's `DefId` so the
    // pass can fold those call sites in.
    let mut method_name_count: HashMap<&str, usize> = HashMap::new();
    let mut field_names: HashSet<&str> = HashSet::new();
    for c in &program.classes {
        for m in c.methods.iter().filter(|m| !m.is_static) {
            *method_name_count.entry(m.name.as_str()).or_default() += 1;
        }
        for fld in c.instance_fields.iter().chain(&c.static_fields) {
            field_names.insert(fld.name.as_str());
        }
    }
    let mut value_read: HashSet<&str> = HashSet::new();
    for f in &program.functions {
        for s in f.blocks.iter().flat_map(|b| &b.statements) {
            match s {
                Statement::Assign(_, Rvalue::Field(_, name)) => {
                    value_read.insert(name.as_str());
                }
                Statement::Assign(_, Rvalue::Index(_, Operand::Const(Const::String(name)))) => {
                    value_read.insert(name.as_str());
                }
                _ => {}
            }
        }
    }
    let extended: HashSet<DefId> = program
        .classes
        .iter()
        .filter_map(|c| c.parent_def)
        .collect();
    let mut method_def: HashMap<String, DefId> = HashMap::new();
    for c in &program.classes {
        if c.parent_def.is_some() || extended.contains(&c.def_id) {
            continue; // not standalone — overrides / super possible
        }
        for m in &c.methods {
            if m.is_static
                || m.user_arity == 0
                || method_name_count.get(m.name.as_str()) != Some(&1)
                || field_names.contains(m.name.as_str())
                || value_read.contains(m.name.as_str())
            {
                continue;
            }
            let fi = m.function_idx;
            let f = &program.functions[fi];
            let Some(def) = f.def_id else { continue };
            if fills_nonconst_default(f) {
                continue;
            }
            method_def.insert(m.name.clone(), def);
            // `params[0]` is `this`; user param at position `pp` ↔ `args[pp - 1]`.
            for (pp, &pid) in f.params.iter().enumerate().skip(1) {
                if eligible_param(f, pid) {
                    candidates.push((fi, pid, pp - 1, def));
                }
            }
        }
    }

    if candidates.is_empty() {
        return;
    }
    // `integer` first; then `real` over the params that stayed `Any`.
    specialize_pass(
        program,
        lang,
        &candidates,
        &method_def,
        &Type::Integer,
        ValTy::Int,
    );
    let remaining: Vec<_> = candidates
        .iter()
        .copied()
        .filter(|&(fi, pid, _, _)| {
            matches!(program.functions[fi].locals[pid.0 as usize].ty, Type::Any)
        })
        .collect();
    specialize_pass(
        program,
        lang,
        &remaining,
        &method_def,
        &Type::Real,
        ValTy::Real,
    );
}

/// One optimistic-then-demote pass for a single scalar kind (`pin` / `want`):
/// pin every `candidate` to `pin`, then repeatedly demote back to `Any` any
/// whose call sites don't all (and provably) pass a `want`-typed argument, to a
/// fixpoint. Only candidates currently typed `pin` are considered (so a prior
/// pass's pins/demotions are respected).
fn specialize_pass(
    program: &mut MirProgram,
    lang: Lang,
    candidates: &[(usize, LocalId, usize, DefId)],
    method_def: &HashMap<String, DefId>,
    pin: &Type,
    want: ValTy,
) {
    use leek_mir::ir::Const;
    if candidates.is_empty() {
        return;
    }
    for &(fi, pid, _, _) in candidates {
        program.functions[fi].locals[pid.0 as usize].ty = pin.clone();
    }
    loop {
        let demote: HashSet<(usize, LocalId)> = {
            let rets = compute_fn_rets(program, lang);
            let tys: Vec<Option<HashMap<LocalId, ValTy>>> = program
                .functions
                .iter()
                .map(|f| infer_local_tys(f, lang, &rets, false, program).ok())
                .collect();
            // Call sites of each candidate callee: (caller index, arg list).
            // A direct `Callee::Function`; or a `Callee::Method` whose name is in
            // `method_def` (a specialization-eligible unique method — every such
            // call provably dispatches to it).
            let mut sites: HashMap<DefId, Vec<(usize, &Vec<Operand>)>> = HashMap::new();
            for (ci, caller) in program.functions.iter().enumerate() {
                for s in caller.blocks.iter().flat_map(|b| &b.statements) {
                    if let Statement::Call { call, .. } = s {
                        match &call.callee {
                            Callee::Function(d) => {
                                sites.entry(*d).or_default().push((ci, &call.args));
                            }
                            Callee::Method { method, .. } => {
                                if let Some(d) = method_def.get(method) {
                                    sites.entry(*d).or_default().push((ci, &call.args));
                                }
                            }
                            _ => {}
                        }
                    }
                }
            }
            let arg_ty = |caller: usize, arg: &Operand| -> ValTy {
                match arg {
                    Operand::Const(Const::Int(_)) => ValTy::Int,
                    Operand::Const(Const::Bool(_)) => ValTy::Bool,
                    Operand::Const(Const::Real(_)) => ValTy::Real,
                    Operand::Const(_) => ValTy::Ref,
                    Operand::Local(id) => tys[caller]
                        .as_ref()
                        .and_then(|t| t.get(id).copied())
                        .unwrap_or(ValTy::Ref),
                }
            };
            let mut demote = HashSet::new();
            for &(fi, pid, pi, def) in candidates {
                // Only this pass's still-pinned candidates.
                if program.functions[fi].locals[pid.0 as usize].ty != *pin {
                    continue;
                }
                // Keep `pin` only with ≥1 call site, all passing a `want`. (No
                // direct call site — e.g. a dead or only-indirect fn — fails this
                // and is demoted, the safe choice.)
                let ok = sites.get(&def).is_some_and(|v| {
                    !v.is_empty()
                        && v.iter()
                            .all(|&(ci, args)| pi < args.len() && arg_ty(ci, &args[pi]) == want)
                });
                if !ok {
                    demote.insert((fi, pid));
                }
            }
            demote
        };
        if demote.is_empty() {
            break;
        }
        for (fi, pid) in demote {
            program.functions[fi].locals[pid.0 as usize].ty = Type::Any;
        }
    }
}

/// True when parameter `id` is used ONLY in value positions safe to read as an
/// unboxed scalar: a `Binary`/`Unary`/`Cast` operand, a `Use` source, a call
/// argument, or a returned operand. It must never be reassigned (a typed slot
/// would coerce the new value), used as a composite/object base
/// (`id[..]`/`id.x`/`push(id,..)`), a method/indirect/super receiver, a lambda
/// capture, or promoted — any of which need the boxed dynamic representation.
fn param_value_only(f: &MirFunction, id: LocalId) -> bool {
    let op_is = |o: &Operand| matches!(o, Operand::Local(l) if *l == id);
    let base_is = |p: &Place| {
        matches!(p,
            Place::Local(l) | Place::Field(l, _) | Place::Index(l, _) | Place::Slice(l, _)
                if *l == id)
            || matches!(p, Place::LambdaCapture { lambda, .. } if *lambda == id)
    };
    for s in f.blocks.iter().flat_map(|b| &b.statements) {
        match s {
            Statement::Assign(place, rv) => {
                if base_is(place) {
                    return false;
                }
                match rv {
                    Rvalue::Field(l, _) | Rvalue::Index(l, _) | Rvalue::Slice(l, _) if *l == id => {
                        return false;
                    }
                    Rvalue::MakeForeachIter(o) if op_is(o) => return false,
                    Rvalue::MakeLambda { captures, .. } if captures.iter().any(op_is) => {
                        return false;
                    }
                    _ => {}
                }
            }
            Statement::Call { dest, call } => {
                if dest.as_ref().is_some_and(base_is) {
                    return false;
                }
                match &call.callee {
                    Callee::Method { receiver, .. } | Callee::Indirect(receiver)
                        if *receiver == id =>
                    {
                        return false;
                    }
                    Callee::SuperConstructor { this, .. } if *this == id => return false,
                    _ => {}
                }
                // Passing `id` as a call ARGUMENT is fine — it propagates the int.
            }
            Statement::ApplyPromotion(l) if *l == id => return false,
            _ => {}
        }
    }
    true
}

/// The declared scalar kind of each typed global (`global real x` → Real),
/// so global writes coerce the stored value. Untyped / composite globals
/// are absent (no coercion).
pub fn global_scalar_tys(program: &MirProgram) -> HashMap<String, ValTy> {
    program
        .globals
        .iter()
        .filter_map(|g| scalar_valty(&g.ty).map(|vt| (g.name.clone(), vt)))
        .collect()
}

/// The functions reachable from `main` that must be compiled alongside it.
/// `defs` are functions addressable by `DefId` (free functions, methods,
/// constructors); `field_inits` are class field-initializer functions,
/// which carry no `DefId` and so are tracked by their index in
/// `program.functions`.
pub struct Reachable {
    pub defs: Vec<DefId>,
    pub field_inits: Vec<usize>,
}

/// The function indices `f` references through call / construction edges:
/// direct calls (`Callee::Function`), user-method dispatch (resolved via
/// the receiver's static `ClassInstance` type + the class vtable),
/// `super(...)` constructors, and `new C(...)` (the class's field
/// initializers + selected constructor).
fn fn_edges(program: &MirProgram, f: &MirFunction) -> Vec<usize> {
    let mut out: Vec<usize> = Vec::new();
    let new_classes = new_class_locals(f);
    let aliased = aliased_class_locals(f);
    let classrefs = classref_locals(f);
    let supers = super_locals(f);
    let push = |idx: usize, out: &mut Vec<usize>| {
        if !out.contains(&idx) {
            out.push(idx);
        }
    };
    let idx_of = |d: DefId| program.functions.iter().position(|g| g.def_id == Some(d));
    for b in &f.blocks {
        for s in &b.statements {
            match s {
                Statement::Call { call, .. } => match &call.callee {
                    Callee::Function(d) => {
                        if let Some(i) = idx_of(*d) {
                            push(i, &mut out);
                        }
                    }
                    Callee::Method { receiver, method } => {
                        if let Some(name) =
                            receiver_class(&f.locals, &new_classes, &aliased, *receiver)
                            && let Some(c) = program.class_by_name(name)
                            && let Some(vt) =
                                program.resolve_method(c, method, Some(call.args.len()))
                        {
                            push(vt.function_idx, &mut out);
                        }
                        // `C.staticMethod()` — receiver is a class reference.
                        if let Some(cls) = classrefs.get(receiver)
                            && let Some(idx) =
                                resolve_static_method(program, cls, method, call.args.len())
                        {
                            push(idx, &mut out);
                        }
                        // `super.m()` — receiver is a `MakeSuper`; resolve the
                        // method against the parent class.
                        if let Some((_, parent)) = supers.get(receiver)
                            && let Some(c) = program.class_by_name(parent)
                            && let Some(vt) =
                                program.resolve_method(c, method, Some(call.args.len()))
                        {
                            push(vt.function_idx, &mut out);
                        }
                    }
                    Callee::SuperConstructor { parent_class, .. } => {
                        if let Some(c) = program.class_by_name(parent_class)
                            && let Some(ci) = program.select_constructor(c, call.args.len())
                        {
                            push(ci, &mut out);
                        }
                    }
                    // `Class clazz = A; clazz()` — a class ref called directly
                    // constructs the class, so its field-inits + constructor
                    // must be compiled (same edges as `new A(...)`).
                    Callee::Indirect(local) => {
                        if let Some(cls) = classrefs.get(local)
                            && let Some(c) = program.class_by_name(cls)
                        {
                            for fs in &c.field_layout {
                                if let Some(fi) = fs.init_fn {
                                    push(fi, &mut out);
                                }
                            }
                            if let Some(ci) = program.select_constructor(c, call.args.len()) {
                                push(ci, &mut out);
                            }
                        }
                    }
                    _ => {}
                },
                Statement::Assign(_, Rvalue::New { class, args }) => {
                    if let Some(c) = program.class_by_name(class) {
                        for fs in &c.field_layout {
                            if let Some(fi) = fs.init_fn {
                                push(fi, &mut out);
                            }
                        }
                        if let Some(ci) = program.select_constructor(c, args.len()) {
                            push(ci, &mut out);
                        }
                    }
                }
                // `x -> …` references a lambda body to compile.
                Statement::Assign(_, Rvalue::MakeLambda { function_idx, .. }) => {
                    push(*function_idx, &mut out);
                }
                // `var f = foo` references a named function to compile.
                Statement::Assign(_, Rvalue::FunctionRef(d)) => {
                    if let Some(i) = idx_of(*d) {
                        push(i, &mut out);
                    }
                }
                _ => {}
            }
        }
    }
    // `obj['m']` value reads make method `m` reachable as a bound method.
    for (fidx, _, _) in index_method_targets(program, f) {
        push(fidx, &mut out);
    }
    // Virtually-dispatched methods (an override exists in a subclass) become
    // reachable — every override + the base — for runtime bound-method dispatch.
    for (fidx, _, _) in virtual_method_targets(program, f) {
        push(fidx, &mut out);
    }
    // Unknown-receiver method calls (`this.n()` from a captured `this`, `(x as
    // C).m()`) dispatch dynamically at runtime, so every candidate method must
    // be compiled + registered.
    for (fidx, _, _) in dynamic_method_targets(program, f) {
        push(fidx, &mut out);
    }
    // `var f = C.staticMethod` (read as a value) makes the static method
    // reachable + uniform-compiled for indirect dispatch.
    for (_, idx) in static_method_value_refs(program, f) {
        push(idx, &mut out);
    }
    // `C.staticField` reads make the field's initialiser reachable.
    for (_, _, init) in static_field_accesses(program, f) {
        if let Some(idx) = init {
            push(idx, &mut out);
        }
    }
    out
}

pub fn reachable_user_fns(program: &MirProgram, main: &MirFunction) -> Reachable {
    let mut seen: HashSet<usize> = HashSet::new();
    let mut order: Vec<usize> = Vec::new();
    let mut stack = fn_edges(program, main);
    while let Some(i) = stack.pop() {
        if !seen.insert(i) {
            continue;
        }
        order.push(i);
        stack.extend(fn_edges(program, &program.functions[i]));
    }
    let mut defs = Vec::new();
    let mut field_inits = Vec::new();
    for i in order {
        match program.functions[i].def_id {
            Some(d) => defs.push(d),
            None => field_inits.push(i),
        }
    }
    Reachable { defs, field_inits }
}

/// Class names whose class reference is used as a *value* somewhere — passed
/// as a call argument, or stored as an element of a composite literal
/// (`arrayMap(a, A)`, `{c: A}`). Such a value can later be *invoked* (a HOF
/// callback, or `o.c(...)`), which constructs the class — so it needs a
/// runtime constructor thunk. A class reference called *directly*
/// (`Class c = A; c()`) is handled at compile time and is NOT collected here.
pub fn classes_used_as_value(program: &MirProgram) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let add = |cls: &String, out: &mut Vec<String>| {
        if !out.contains(cls) {
            out.push(cls.clone());
        }
    };
    for f in &program.functions {
        let crefs = classref_locals(f);
        if crefs.is_empty() {
            continue;
        }
        let cls_of = |op: &Operand| -> Option<&String> {
            match op {
                Operand::Local(id) => crefs.get(id),
                _ => None,
            }
        };
        for b in &f.blocks {
            for s in &b.statements {
                match s {
                    // A class ref passed as an argument to any call.
                    Statement::Call { call, .. } => {
                        for a in &call.args {
                            if let Some(c) = cls_of(a) {
                                add(c, &mut out);
                            }
                        }
                    }
                    // A class ref stored as a composite-literal element.
                    Statement::Assign(_, rv) => {
                        let elems: Vec<&Operand> = match rv {
                            Rvalue::Array(es) => es.iter().collect(),
                            Rvalue::Set(es) => es.iter().flat_map(SetElem::operands).collect(),
                            Rvalue::Object(fs) => fs.iter().map(|(_, v)| v).collect(),
                            Rvalue::Map(kvs) => kvs.iter().flat_map(|(k, v)| [k, v]).collect(),
                            _ => Vec::new(),
                        };
                        for e in elems {
                            if let Some(c) = cls_of(e) {
                                add(c, &mut out);
                            }
                        }
                    }
                    _ => {}
                }
            }
            // A class ref returned from a function escapes as a value too.
            if let Terminator::Return(Some(op)) = &b.terminator
                && let Some(c) = cls_of(op)
            {
                add(c, &mut out);
            }
        }
    }
    out
}

/// Build a synthetic constructor *thunk* for class `name` with `arity`
/// constructor parameters: `fn(p0, …, p{arity-1}) { t = new name(p0…); return t }`.
/// Compiled with the uniform `(argv, argc)` ABI like a lambda, it lets a class
/// reference used as a value construct an instance when invoked. All locals are
/// untyped (`Any` → boxed `Ref` under the uniform ABI).
fn make_ctor_thunk(name: &str, arity: usize) -> MirFunction {
    let span = leek_span::Span::synthetic();
    let decl = |kind: LocalKind| LocalDecl {
        name: None,
        ty: Type::Any,
        kind,
        span,
        default_init: None,
        inferred_ty: None,
        is_shared: false,
        is_by_ref: false,
    };
    // params p0..p{arity-1}, then the New result temp `t` at index `arity`.
    let mut locals: Vec<LocalDecl> = (0..arity).map(|_| decl(LocalKind::Param)).collect();
    let result = LocalId(arity as u32);
    locals.push(decl(LocalKind::Temp));
    let params: Vec<LocalId> = (0..arity as u32).map(LocalId).collect();
    let args: Vec<Operand> = params.iter().map(|p| Operand::Local(*p)).collect();
    let block = BasicBlock {
        id: BlockId(0),
        statements: vec![Statement::Assign(
            Place::Local(result),
            Rvalue::New {
                class: name.to_string(),
                args,
            },
        )],
        statement_spans: vec![span],
        terminator: Terminator::Return(Some(Operand::Local(result))),
        terminator_span: span,
    };
    MirFunction {
        def_id: None,
        kind: FunctionKind::User,
        name: format!("<ctor-thunk {name}>"),
        params,
        return_ty: Type::Any,
        locals,
        blocks: vec![block],
        entry: BlockId(0),
        owning_class: None,
        span,
    }
}

/// Append a constructor thunk to `program.functions` for every class that is
/// used as a value (`classes_used_as_value`) and is actually constructible by
/// the native `new` path (a user class that doesn't extend a builtin and has no
/// `string()` override). Returns `class DefId raw → thunk function index`.
/// v1's deep-clone value semantics aren't modeled by `new`, so v1 is skipped
/// (the class-ref-as-value sites keep their existing skip).
pub fn append_ctor_thunks(program: &mut MirProgram, version: u8) -> HashMap<u32, usize> {
    if version < 2 {
        return HashMap::new();
    }
    // Resolve each used-as-value class to `(def raw, ctor arity, name)` while we
    // hold only shared borrows, then append the thunks.
    let specs: Vec<(u32, usize, String)> = classes_used_as_value(program)
        .into_iter()
        .filter_map(|cls_name| {
            if leek_runtime::builtin_class_name(&cls_name).is_some() {
                return None;
            }
            let c = program.class_by_name(&cls_name)?;
            if class_extends_builtin(program, c) || c.vtable.iter().any(|e| e.name == "string") {
                return None;
            }
            let arity = c.constructors.first().map_or(0, |k| k.user_arity);
            Some((c.def_id.0, arity, cls_name))
        })
        .collect();
    let mut map = HashMap::new();
    for (class_def, arity, name) in specs {
        let idx = program.functions.len();
        program.functions.push(make_ctor_thunk(&name, arity));
        map.insert(class_def, idx);
    }
    map
}

/// Per-class reflection name tables for runtime `x.class.<member>` reads:
/// `class DefId raw → { member → [names] }` for the string-array reflection
/// members (`fields`/`methods`/`static_fields`/`static_methods`, both
/// snake_case and camelCase spellings). Reuses [`class_reflect`] so the runtime
/// path matches the compile-time `C.fields` path exactly.
pub fn reflect_name_tables(program: &MirProgram) -> HashMap<u32, HashMap<String, Vec<String>>> {
    const MEMBERS: &[&str] = &[
        "fields",
        "methods",
        "static_fields",
        "static_methods",
        "staticFields",
        "staticMethods",
    ];
    let mut out = HashMap::new();
    for c in &program.classes {
        let mut by_member = HashMap::new();
        for &mem in MEMBERS {
            if let Some(leek_runtime::Value::Array(arr)) = class_reflect(program, &c.name, mem) {
                let names: Vec<String> = arr
                    .borrow()
                    .iter()
                    .filter_map(|v| match v {
                        leek_runtime::Value::String(s) => Some(s.as_ref().clone()),
                        _ => None,
                    })
                    .collect();
                by_member.insert(mem.to_string(), names);
            }
        }
        out.insert(c.def_id.0, by_member);
    }
    out
}

/// Classes that are *constructed* anywhere reachable (a `new C(…)` in a
/// reachable body, or a class-ref constructor thunk) AND declare a 0-arg
/// `string()` method — returned as `(class DefId raw, string() function idx)`.
/// Their instances can be the top-level program result, where `string()` is
/// applied (mirroring the interpreter), so `string()` must be force-compiled
/// and registered.
pub fn string_display_classes(
    program: &MirProgram,
    reachable_indices: &[usize],
    thunk_classes: &HashMap<u32, usize>,
) -> Vec<(u32, usize)> {
    let mut constructed: HashSet<String> = HashSet::new();
    for &fi in reachable_indices {
        for b in &program.functions[fi].blocks {
            for s in &b.statements {
                if let Statement::Assign(_, Rvalue::New { class, .. }) = s {
                    constructed.insert(class.clone());
                }
            }
        }
    }
    for &cd in thunk_classes.keys() {
        if let Some(c) = program.class(DefId(cd)) {
            constructed.insert(c.name.clone());
        }
    }
    let mut out = Vec::new();
    for name in constructed {
        if let Some(c) = program.class_by_name(&name)
            && let Some(vt) = program.resolve_method(c, "string", Some(0))
        {
            out.push((c.def_id.0, vt.function_idx));
        }
    }
    out
}

/// Extend `reachable` with each seed function in `seed_idxs` and its transitive
/// call/construct callees — used to pull in `string()` display methods (and
/// their callees) that aren't otherwise reachable. Unlike
/// [`extend_reachable_for_thunks`], the seeds themselves ARE added.
pub fn extend_reachable_with(program: &MirProgram, seed_idxs: &[usize], reachable: &mut Reachable) {
    let idx_of = |d: DefId| program.functions.iter().position(|g| g.def_id == Some(d));
    let mut seen: HashSet<usize> = reachable
        .defs
        .iter()
        .filter_map(|d| idx_of(*d))
        .chain(reachable.field_inits.iter().copied())
        .collect();
    let mut stack: Vec<usize> = seed_idxs.to_vec();
    while let Some(i) = stack.pop() {
        if !seen.insert(i) {
            continue;
        }
        match program.functions[i].def_id {
            Some(d) => reachable.defs.push(d),
            None => reachable.field_inits.push(i),
        }
        stack.extend(fn_edges(program, &program.functions[i]));
    }
}

/// Extend `reachable` with the constructor + field initializers (transitively)
/// that each thunk in `thunk_idxs` constructs, so `new_instance` finds them
/// compiled. The thunks themselves are compiled separately (uniform ABI) and
/// are NOT added here.
pub fn extend_reachable_for_thunks(
    program: &MirProgram,
    thunk_idxs: &[usize],
    reachable: &mut Reachable,
) {
    let idx_of = |d: DefId| program.functions.iter().position(|g| g.def_id == Some(d));
    let mut seen: HashSet<usize> = reachable
        .defs
        .iter()
        .filter_map(|d| idx_of(*d))
        .chain(reachable.field_inits.iter().copied())
        .collect();
    let mut stack: Vec<usize> = thunk_idxs
        .iter()
        .flat_map(|&t| fn_edges(program, &program.functions[t]))
        .collect();
    while let Some(i) = stack.pop() {
        if !seen.insert(i) {
            continue;
        }
        match program.functions[i].def_id {
            Some(d) => reachable.defs.push(d),
            None => reachable.field_inits.push(i),
        }
        stack.extend(fn_edges(program, &program.functions[i]));
    }
}

/// The calling convention of `f`: parameter kinds (from declared types)
/// and result kind. A scalar-typed parameter (`integer`/`real`/`boolean`)
/// passes unboxed; any other (untyped `var`, or a composite type) passes
/// as a boxed `Ref` handle, so the body operates on it dynamically.
pub fn function_sig(
    f: &MirFunction,
    lang: Lang,
    rets: &FnRets,
    program: &MirProgram,
) -> Result<FnSig, NativeError> {
    // `@x` by-reference parameters: an in-place mutation (`push(t, …)`,
    // `t[0] = …`) propagates to the caller via the shared `Rc` (v2+ reference
    // semantics) with no cell needed — `operand()` peels any cell so the
    // callee shares the same backing store. Only a *reassignment* of the param
    // (`t = …`) needs true caller/callee cell aliasing, which is gated at the
    // program level by `needs_cell_semantics` (so such programs skip). So we no
    // longer reject a by-ref param here.
    // A pure-void function (including `main`) returns null, modeled as a
    // boxed-null `Ref`. Only a *mixed* value/null result is rejected.
    reject_null_result(f, true)?;
    let tys = infer_local_tys(f, lang, rets, false, program)?;
    // A param that becomes a `Value::Cell` local is passed as its shared cell
    // handle, so its ABI is `Ref` regardless of the declared scalar type. This
    // must stay consistent with `translate_function`'s `cell_locals`: an
    // `is_shared` param (captured by / returned-`@`'d to a closure) or a
    // reassigned / aliased-onward `byref_cell_params` param (any version) — but
    // NOT a v2+ no-op param (a pure-local `@x` on a method, which is removed from
    // `cell_locals` and compiled by-value).
    let byref_cells = byref_cell_params(f, program);
    let noop = program
        .functions
        .iter()
        .position(|g| std::ptr::eq(g, f))
        .map(|fi| noop_byref_params(program, fi, lang.version))
        .unwrap_or_default();
    let params = f
        .params
        .iter()
        .map(|pid| {
            let cell = f.locals[pid.0 as usize].is_shared || byref_cells.contains(pid);
            if cell && !noop.contains(pid) {
                ValTy::Ref
            } else {
                scalar_valty(&f.locals[pid.0 as usize].ty).unwrap_or(ValTy::Ref)
            }
        })
        .collect();
    // A concrete scalar return type (`=> integer`) coerces the result —
    // `function f(real r) => integer { return r }` returns an int. Otherwise
    // the result kind is whatever the return operands produce.
    let ret = scalar_valty(&f.return_ty).unwrap_or_else(|| ret_valty(f, &tys));
    // A param has a *non-constant* default when its `default_init` block
    // can't be folded to a self-contained constant/composite — then the
    // callee must evaluate it at entry (hidden `argc` ABI).
    let has_defaults = f.params.iter().any(|pid| {
        fillable_default(f, *pid).is_some()
            && const_default(f, *pid).is_none()
            && const_eval_default(f, *pid, lang.version).is_none()
    });
    Ok(FnSig {
        params,
        ret,
        has_defaults,
    })
}

/// Join the kinds of all value-returns in `f`. Defaults to `Int` when `f`
/// has no value return (callers reject that separately).
fn ret_valty(f: &MirFunction, tys: &HashMap<LocalId, ValTy>) -> ValTy {
    // Walk only *reachable* blocks, folding constant branches exactly like
    // `reject_null_result`, so a dead `return 12` (after `if (false)`)
    // doesn't make an otherwise-void function look like it returns an int.
    let by_id: HashMap<BlockId, &leek_mir::ir::BasicBlock> =
        f.blocks.iter().map(|b| (b.id, b)).collect();
    let mut seen: HashSet<BlockId> = HashSet::new();
    let mut stack = vec![f.entry];
    let mut acc: Option<ValTy> = None;
    while let Some(bid) = stack.pop() {
        if !seen.insert(bid) {
            continue;
        }
        let Some(b) = by_id.get(&bid) else { continue };
        match &b.terminator {
            Terminator::Goto(t) => stack.push(*t),
            Terminator::Branch {
                cond,
                then_block,
                else_block,
            } => match const_bool(cond) {
                Some(true) => stack.push(*then_block),
                Some(false) => stack.push(*else_block),
                None => {
                    stack.push(*then_block);
                    stack.push(*else_block);
                }
            },
            Terminator::Switch { arms, default, .. } => {
                for (_, t) in arms {
                    stack.push(*t);
                }
                stack.push(*default);
            }
            Terminator::Return(Some(op)) => {
                let t = operand_ty(op, tys).unwrap_or(ValTy::Int);
                acc = Some(acc.map_or(t, |a| join(a, t)));
            }
            // An explicit null return contributes `Ref` (null is only
            // representable boxed), so a function mixing value and null
            // returns gets a boxed result.
            Terminator::Return(None) => {
                acc = Some(acc.map_or(ValTy::Ref, |a| join(a, ValTy::Ref)));
            }
            Terminator::Unreachable => {}
        }
    }
    // No reachable value return → a void function, which returns null:
    // model the result as a boxed-null `Ref`.
    acc.unwrap_or(ValTy::Ref)
}

/// Join of two value kinds. `Ref` is the universal fallback (a slot used
/// for both a composite and a scalar boxes the scalar), so it dominates;
/// otherwise `Real` dominates, two bools stay bool, and anything else is
/// `Int`.
fn join(a: ValTy, b: ValTy) -> ValTy {
    if a == ValTy::Ref || b == ValTy::Ref {
        ValTy::Ref
    } else if a == ValTy::Real || b == ValTy::Real {
        ValTy::Real
    } else if a == ValTy::Bool && b == ValTy::Bool {
        ValTy::Bool
    } else {
        ValTy::Int
    }
}

/// Translate `mir_fn` into `func` (whose signature already matches `sig`).
/// `callees` maps each user function's `DefId` to its declared `FuncId`
/// and signature, so calls can be lowered. `module` is `None` only for the
/// text-dump emit modes (Clif / Disasm), where calls aren't lowered.
#[allow(clippy::too_many_arguments)]
/// Box a scalar value (`from` kind) into a `Ref` handle, or pass a handle
/// through unchanged. A free function (mirrors `Tx::coerce`'s box path) for
/// use during entry var-init, before a `Tx` exists.
fn box_to_ref(
    builder: &mut FunctionBuilder,
    imports: &Imports,
    v: Value,
    from: ValTy,
) -> Result<Value, NativeError> {
    let sym = match from {
        ValTy::Ref => return Ok(v),
        ValTy::Int => "leek_box_int",
        ValTy::Bool => "leek_box_bool",
        ValTy::Real => "leek_box_real",
    };
    let fref = imports.rt(sym)?;
    let inst = builder.ins().call(fref, &[v]);
    Ok(builder.inst_results(inst)[0])
}

/// A function's debug frame: leaked descriptor-table pointer, optional
/// value-spill stack slot, and the (local index, kind) of each named local.
type DbgFrame = Option<(
    i64,
    Option<cranelift::codegen::ir::StackSlot>,
    Vec<(usize, u8)>,
)>;

/// Emit a `leek_dbg_safepoint(offset, desc, values)` call, spilling the
/// frame's named locals into the value slot first. Used before each
/// statement and before each `return` terminator.
fn emit_dbg_safepoint(
    tx: &mut Tx<'_, '_>,
    frame: &DbgFrame,
    offset: u32,
) -> Result<(), NativeError> {
    let (desc_v, values_v) = if let Some((table_ptr, slot, slots)) = frame {
        let dv = tx.b.ins().iconst(types::I64, *table_ptr);
        let vv = if let Some(slot) = slot {
            for (idx, (local, _kind)) in slots.iter().enumerate() {
                let mut v = tx.b.use_var(tx.vars[*local]);
                // A cell local's var holds the shared cell handle; peel it to
                // the boxed value the user sees.
                if tx.cell_locals.contains(&LocalId(*local as u32)) {
                    let cg = tx.imports.rt("leek_cell_get")?;
                    let inst = tx.b.ins().call(cg, &[v]);
                    v = tx.b.inst_results(inst)[0];
                }
                // Store raw 8-byte value (int/bool/ptr, or f64 bits);
                // `render_frame_vars` reinterprets per kind.
                tx.b.ins()
                    .stack_store(v, *slot, i32::try_from(idx * 8).unwrap_or(0));
            }
            tx.b.ins().stack_addr(types::I64, *slot, 0)
        } else {
            tx.b.ins().iconst(types::I64, 0)
        };
        (dv, vv)
    } else {
        let zero = tx.b.ins().iconst(types::I64, 0);
        (zero, zero)
    };
    let sp = tx.imports.rt("leek_dbg_safepoint")?;
    let off = tx.b.ins().iconst(types::I64, i64::from(offset));
    tx.b.ins().call(sp, &[off, desc_v, values_v]);
    Ok(())
}

pub fn translate_function(
    func: &mut codegen::ir::Function,
    fb_ctx: &mut FunctionBuilderContext,
    mir_fn: &MirFunction,
    sig: &FnSig,
    lang: Lang,
    rets: &FnRets,
    module: Option<&mut dyn Module>,
    callees: &HashMap<DefId, (cranelift_module::FuncId, FnSig)>,
    // Field-initializer FuncIds keyed by `program.functions` index (they
    // carry no `DefId`), referenced when lowering `new`.
    field_init_callees: &HashMap<usize, (cranelift_module::FuncId, FnSig)>,
    program: &MirProgram,
    // Declared scalar kind per typed global (`global real x` → Real), so
    // writes coerce to it.
    global_tys: &HashMap<String, ValTy>,
    // `DefId` → `@native-backend:` directive (the runtime builtin to
    // dispatch). A call to such a function emits `dispatch_builtin`
    // rather than a normal user call (the function is a bodiless
    // signature with no compiled body).
    native_directives: &HashMap<DefId, String>,
    // Raw `DefId`s of classes that have a synthetic constructor thunk — so a
    // class reference used as a value (`arrayMap(a, A)`, object-slot-held `A`)
    // can flow as a callable instead of skipping. A class NOT in this set keeps
    // the skip (the runtime would have no way to construct it).
    ctor_thunk_classes: &HashSet<u32>,
    // True when compiling a lambda body with the uniform `(argv, argc) ->
    // result` ABI (all params boxed `Ref`, loaded from `argv`).
    uniform_abi: bool,
    // Emit a `leek_dbg_safepoint(offset)` call before each statement so a
    // debugger can pause at source lines (see `crate::debug`).
    debug_hooks: bool,
    // Route otherwise-unknown builtins to the host game runtime (see
    // `crate::game`) instead of failing with `Unsupported`.
    link_game: bool,
) -> Result<(), NativeError> {
    // Reject functions whose result can be `null` — the scalar subset
    // can't represent it. Walk reachable blocks (folding constant
    // branches so `if (false) return 12` is seen as the null fall-
    // through), and bail if a reachable path returns no value or returns
    // an uninitialized local.
    reject_null_result(mir_fn, true)?;

    let local_tys = infer_local_tys(mir_fn, lang, rets, uniform_abi, program)?;

    // The parameter slot for each local (its index in the signature), so
    // entry-block params bind to the right locals instead of zero-init.
    let mut param_index: HashMap<LocalId, usize> = HashMap::new();
    for (i, pid) in mir_fn.params.iter().enumerate() {
        param_index.insert(*pid, i);
    }

    // Cell locals: every `is_shared` local (a lambda-captured variable) gets
    // shared `Value::Cell` storage — exactly like the interpreter — so a
    // closure and its enclosing scope observe each other's reassignments.
    // The var holds the cell handle; `local_value` peels it (`cell_get`) at
    // every *value* read (operand / field / index / receiver / callee),
    // writes go through `cell_set`, and lambda captures pass the handle raw
    // (sharing the `Rc`). `@x` by-ref params are still gated by `function_sig`.
    let mut cell_locals: HashSet<LocalId> = (0..mir_fn.locals.len() as u32)
        .map(LocalId)
        .filter(|id| mir_fn.locals[id.0 as usize].is_shared)
        .collect();
    // Cross-function cells (all versions): a reassigned / aliased-onward `@x`
    // by-ref param needs a real `Value::Cell` so its rebinding propagates to the
    // caller, and any local this function passes into such a param must itself
    // be a cell so the shared handle (not a snapshot) is what's passed. Gated to
    // threadable programs by `needs_cell_semantics`; here we just promote the
    // locals. (v1's value semantics are preserved by the cell-write deep-clone.)
    cell_locals.extend(byref_cell_params(mir_fn, program));
    cell_locals.extend(byref_arg_cell_locals(mir_fn, program));
    // A local passed *directly* to a callee whose `@x` by-ref param escapes via
    // an inner lambda / `return @x` (`byref_param_escape_threadable`) must be a
    // cell, so `byref_cell_arg` hands the shared handle over and the escaped
    // value (the capturing lambda, or the returned alias the caller binds)
    // shares the caller's storage. Applies in EVERY version — `function f(@a){
    // return function(){ a += 2 } }; f(x)()` sees `x` change in v1 and v2+ alike.
    cell_locals.extend(byref_captured_arg_cell_locals(mir_fn, program));
    if lang.version >= 2 {
        // A v2+ no-op by-ref param (a pure-local `@x` on a lambda or a method)
        // compiles as a plain by-value param: its `@x` has no observable effect
        // in v2+, so it must NOT be cell-backed. Must agree with the
        // `needs_cell_semantics` gate exemption (`noop_byref_params`) and
        // `function_sig`'s by-value ABI for the same params.
        if let Some(fi) = program
            .functions
            .iter()
            .position(|g| std::ptr::eq(g, mir_fn))
        {
            for p in noop_byref_params(program, fi, lang.version) {
                cell_locals.remove(&p);
            }
        }
    } else {
        // v1: a local passed to an indirect (function-value) call becomes a cell
        // so a callee `@x` by-ref param shares the caller's storage and the
        // mutation propagates (the runtime dispatch threads the cell). `@x`
        // params of THIS body are already cells via `is_shared`.
        cell_locals.extend(indirect_arg_cell_locals(mir_fn));
    }

    let mut builder = FunctionBuilder::new(func, fb_ctx);

    // A clif block per MIR block, plus a dedicated entry that initialises
    // every local exactly once before jumping into the MIR entry (so loop
    // back-edges don't re-init): params bind to incoming block params,
    // others zero-init.
    let mut blocks: HashMap<BlockId, Block> = HashMap::new();
    for b in &mir_fn.blocks {
        blocks.insert(b.id, builder.create_block());
    }
    let entry = builder.create_block();
    builder.append_block_params_for_function_params(entry);
    builder.switch_to_block(entry);
    let entry_params = builder.block_params(entry).to_vec();

    // A lambda compiled with the uniform ABI receives `(argv, argc)`: all
    // its params (captures first, then user params) are boxed `Ref`s loaded
    // from `argv[i]`, and it returns a boxed `Ref`.
    let argv = if uniform_abi {
        Some(entry_params[0])
    } else {
        None
    };
    let ret_ty = if uniform_abi { ValTy::Ref } else { sig.ret };

    // Declare runtime/user-fn imports up front (before the entry var-init
    // loop) so non-parameter cell locals can allocate their `Value::Cell`
    // via `leek_make_cell` at entry. `any_ref_local` (forces the composite
    // shims) is computed without `var_tys`: any `Ref` param/local, a
    // uniform-ABI body (boxes its result), or any cell local.
    let any_ref_local = uniform_abi
        || !cell_locals.is_empty()
        || sig.params.contains(&ValTy::Ref)
        || local_tys.values().any(|&t| t == ValTy::Ref);
    let imports = declare_imports(
        &mut builder,
        mir_fn,
        module,
        callees,
        field_init_callees,
        program,
        native_directives,
        any_ref_local,
        lang,
        debug_hooks,
        link_game,
    )?;

    let mut vars: Vec<Variable> = Vec::with_capacity(mir_fn.locals.len());
    let mut var_tys: Vec<ValTy> = Vec::with_capacity(mir_fn.locals.len());
    for i in 0..mir_fn.locals.len() {
        let lid = LocalId(i as u32);
        // `src_ty` is the local's natural kind: a param's signature ABI
        // type (or `Ref` under the uniform ABI), else inference. `var_ty`
        // is what the cranelift variable holds — `Ref` for a cell local
        // (its shared cell handle), otherwise `src_ty`.
        let src_ty = match param_index.get(&lid) {
            Some(&pi) => {
                if uniform_abi {
                    ValTy::Ref
                } else {
                    sig.params[pi]
                }
            }
            None => local_tys.get(&lid).copied().unwrap_or(ValTy::Int),
        };
        let is_cell = cell_locals.contains(&lid);
        let var_ty = if is_cell { ValTy::Ref } else { src_ty };
        let var = builder.declare_var(var_ty.cl_type());
        // The raw initial value at `src_ty`.
        let raw = match param_index.get(&lid) {
            Some(&pi) => match argv {
                // Uniform ABI: load the i-th boxed handle from `argv`.
                Some(argv) => {
                    builder
                        .ins()
                        .load(types::I64, MemFlags::trusted(), argv, (pi * 8) as i32)
                }
                None => entry_params[pi],
            },
            None => match src_ty {
                ValTy::Real => builder.ins().f64const(0.0),
                // An uninitialized `var x` is null — a `Ref` local must
                // start as a valid boxed-null handle, not a 0 pointer that
                // a dynamic op would dereference.
                ValTy::Ref => {
                    let f = imports.rt("leek_box_null")?;
                    let inst = builder.ins().call(f, &[]);
                    builder.inst_results(inst)[0]
                }
                _ => builder.ins().iconst(types::I64, 0),
            },
        };
        let init = if is_cell {
            // A cell local's var holds a shared `Value::Cell` handle. Box
            // the incoming value to a `Ref` first (a captured scalar param
            // arrives unboxed), then `leek_make_cell` — which *reuses* an
            // existing cell (a lambda capture-param already holds the
            // enclosing scope's cell) or wraps a fresh one.
            let boxed = box_to_ref(&mut builder, &imports, raw, src_ty)?;
            let mk = imports.rt("leek_make_cell")?;
            let inst = builder.ins().call(mk, &[boxed]);
            builder.inst_results(inst)[0]
        } else {
            raw
        };
        builder.def_var(var, init);
        vars.push(var);
        var_tys.push(var_ty);
    }

    // Debug variable inspection: a leaked descriptor table (names + kinds)
    // for this function's named locals plus a stack-slot array their values
    // are spilled into before each safepoint. `None` when `debug_hooks` is
    // off or the function has no named locals.
    let dbg_frame: DbgFrame = if debug_hooks {
        let mut descs = Vec::new();
        let mut slots = Vec::new();
        for (k, local) in mir_fn.locals.iter().enumerate() {
            let Some(name) = &local.name else { continue };
            let kind = if cell_locals.contains(&LocalId(k as u32)) {
                3 // a cell holds a boxed `Value` handle
            } else {
                match var_tys[k] {
                    ValTy::Int => 0,
                    ValTy::Real => 1,
                    ValTy::Bool => 2,
                    ValTy::Ref => 3,
                }
            };
            descs.push(crate::debug::VarDesc {
                name: name.clone(),
                kind,
            });
            slots.push((k, kind));
        }
        // Always build a table (it carries the function name for the call
        // stack); allocate a value-spill slot only when there are locals.
        let table: &'static crate::debug::VarTable = Box::leak(Box::new(crate::debug::VarTable {
            func_name: mir_fn.name.clone(),
            vars: descs,
        }));
        let table_ptr = std::ptr::from_ref(table) as i64;
        let slot = if slots.is_empty() {
            None
        } else {
            let n = u32::try_from(slots.len()).unwrap_or(0);
            Some(builder.create_sized_stack_slot(StackSlotData::new(
                StackSlotKind::ExplicitSlot,
                n * 8,
                3,
            )))
        };
        Some((table_ptr, slot, slots))
    } else {
        None
    };

    let mir_entry = blocks[&mir_fn.entry];

    // Non-constant default arguments: a function with `has_defaults` takes a
    // hidden trailing `argc` param (the number of args actually provided). At
    // entry — after binding params (omitted ones hold caller placeholders) —
    // run each omitted defaulted param's `default_init` block to fill it, in
    // ascending param order so a later default can read an earlier one.
    // `default_fill` tells the block's terminator to store its value into the
    // param var + jump to the continuation instead of returning.
    let mut default_fill: HashMap<BlockId, (LocalId, Block)> = HashMap::new();
    if sig.has_defaults && !uniform_abi {
        let argc = entry_params[sig.params.len()];
        for (i, &param_local) in mir_fn.params.iter().enumerate() {
            // Fillable defaults (a sub-CFG of control-flow + `Return(Some)`
            // exits) are run by the entry chain; an unfillable param is never
            // reached here (the call site requires every omitted param to be
            // fillable, else it skips). The `brif` jumps to the default's ENTRY
            // block; each `Return` block in its sub-CFG stores the value into
            // the param var + jumps to the continuation (`default_fill`).
            let Some(default_bb) = fillable_default(mir_fn, param_local) else {
                continue;
            };
            let Some(&default_clif) = blocks.get(&default_bb) else {
                continue;
            };
            let cont = builder.create_block();
            let i_val = builder.ins().iconst(types::I64, i as i64);
            // `argc <= i` ⇒ param i was omitted ⇒ run its default sub-CFG.
            let cond = builder
                .ins()
                .icmp(IntCC::SignedLessThanOrEqual, argc, i_val);
            builder.ins().brif(cond, default_clif, &[], cont, &[]);
            for ret_bb in default_return_blocks(mir_fn, default_bb) {
                default_fill.insert(ret_bb, (param_local, cont));
            }
            builder.switch_to_block(cont);
        }
        builder.ins().jump(mir_entry, &[]);
    } else {
        builder.ins().jump(mir_entry, &[]);
    }

    let elem_tys: Vec<Option<ValTy>> = mir_fn
        .locals
        .iter()
        .map(|d| array_elem_valty(&d.ty))
        .collect();
    let new_classes = new_class_locals(mir_fn);
    let aliased_classes = aliased_class_locals(mir_fn);
    let classref_locals = classref_locals(mir_fn);
    let super_locals = super_locals(mir_fn);
    let object_locals = object_locals(mir_fn);
    let object_field_srcs = object_field_srcs(mir_fn);

    for b in &mir_fn.blocks {
        let clif_block = blocks[&b.id];
        builder.switch_to_block(clif_block);
        let mut tx = Tx {
            b: &mut builder,
            blocks: &blocks,
            vars: &vars,
            var_tys: &var_tys,
            ret_ty,
            lang,
            link_game,
            imports: &imports,
            elem_tys: &elem_tys,
            global_tys,
            program,
            native_directives,
            mir_locals: &mir_fn.locals,
            new_classes: &new_classes,
            aliased_classes: &aliased_classes,
            classref_locals: &classref_locals,
            ctor_thunk_classes,
            owning_class: mir_fn.owning_class,
            cell_locals: &cell_locals,
            super_locals: &super_locals,
            default_fill: &default_fill,
            object_locals: &object_locals,
            object_field_srcs: &object_field_srcs,
            pending_charge: 0,
        };
        // On function entry, push a shadow call frame.
        if debug_hooks
            && b.id == mir_fn.entry
            && let Some((table_ptr, _, _)) = &dbg_frame
        {
            let enter = tx.imports.rt("leek_dbg_enter")?;
            let d = tx.b.ins().iconst(types::I64, *table_ptr);
            tx.b.ins().call(enter, &[d]);
        }
        // Charge 1 op on entry to a *called* function body (interp `call.rs` —
        // the synthetic `<main>` frame is exempt, so a top-level
        // `var x = 42 return x` isn't charged a phantom call op).
        if b.id == mir_fn.entry && mir_fn.kind != leek_mir::ir::FunctionKind::Main {
            tx.charge(1)?;
        }

        for (i, stmt) in b.statements.iter().enumerate() {
            // Debug safepoint: spill this function's named locals into the
            // frame slot, then call out with (offset, descriptor, values) so
            // a debugger can pause and inspect them.
            if debug_hooks && let Some(span) = b.statement_spans.get(i) {
                emit_dbg_safepoint(&mut tx, &dbg_frame, span.start)?;
            }
            tx.stmt(stmt)?;
        }

        // Real return (default-fill "return" blocks store-and-jump instead,
        // so skip those): safepoint on the return line first — catching bare
        // `return x` lines that produce no statement — while the frame is
        // still live, then pop it.
        if debug_hooks
            && dbg_frame.is_some()
            && matches!(b.terminator, Terminator::Return(_))
            && !default_fill.contains_key(&b.id)
        {
            if b.terminator_span.source != leek_span::Span::SYNTHETIC_SOURCE {
                emit_dbg_safepoint(&mut tx, &dbg_frame, b.terminator_span.start)?;
            }
            let leave = tx.imports.rt("leek_dbg_leave")?;
            tx.b.ins().call(leave, &[]);
        }
        tx.terminator(b.id, &b.terminator)?;
    }

    builder.seal_all_blocks();
    builder.finalize();
    Ok(())
}

/// Imported runtime functions resolved for the function being compiled.
#[derive(Default)]
struct Imports {
    /// Named scalar math builtins, by Leekscript name → (import, sig).
    named: HashMap<String, (FuncRef, MathSig)>,
    /// `leek_pow` for the `**` operator's real path.
    pow_real: Option<FuncRef>,
    /// `leek_ipow` for the `**` operator's all-integer path.
    pow_int: Option<FuncRef>,
    /// User functions this one calls, by `DefId` → (import, signature).
    /// Includes methods and constructors (they carry `DefId`s).
    user_fns: HashMap<DefId, (FuncRef, FnSig)>,
    /// Class field-initializer functions referenced by `new` here, by
    /// their `program.functions` index → (import, signature). Field-inits
    /// carry no `DefId`, so they're keyed separately from `user_fns`.
    field_init_fns: HashMap<usize, (FuncRef, FnSig)>,
    /// Composite-value runtime shims, by symbol (`leek_array_new`, …).
    rt: HashMap<&'static str, FuncRef>,
}

impl Imports {
    /// A required runtime shim's resolved import.
    fn rt(&self, sym: &str) -> Result<FuncRef, NativeError> {
        self.rt
            .get(sym)
            .copied()
            .ok_or_else(|| unsupported(format!("runtime shim {sym} not declared")))
    }
}

struct Tx<'a, 'b> {
    b: &'a mut FunctionBuilder<'b>,
    blocks: &'a HashMap<BlockId, Block>,
    vars: &'a [Variable],
    var_tys: &'a [ValTy],
    ret_ty: ValTy,
    lang: Lang,
    /// Route unknown builtins to the host game runtime (see `crate::game`).
    link_game: bool,
    imports: &'a Imports,
    /// Per-local declared array element kind, if the local is a typed
    /// numeric array (`Array<integer>` / `Array<real>`). Drives element
    /// coercion on `a[i] = x` writes, matching the interpreter.
    elem_tys: &'a [Option<ValTy>],
    /// Declared scalar kind per typed global, for write coercion.
    global_tys: &'a HashMap<String, ValTy>,
    /// The whole program — used to resolve class layouts for `new` and
    /// user-method dispatch.
    program: &'a MirProgram,
    /// `DefId` → `@native-backend:` directive: a call to one of these
    /// functions dispatches the named runtime builtin instead of a
    /// normal user call (the function is a bodiless signature).
    native_directives: &'a HashMap<DefId, String>,
    /// Declared MIR types of this function's locals — used to read a
    /// method-call receiver's static `ClassInstance` for dispatch.
    mir_locals: &'a [LocalDecl],
    /// Locals proven to hold a `new C(...)` instance — lets inline
    /// `new C().m()` temps dispatch.
    new_classes: &'a HashMap<LocalId, String>,
    /// Locals whose class is known but NOT exact (`var x = this`, `obj as C`)
    /// — dispatch on them is virtual when the method is overridden.
    aliased_classes: &'a HashMap<LocalId, String>,
    /// Locals proven to hold a `ClassRef(C)` — lets `C.staticMethod()`
    /// dispatch to the static method.
    classref_locals: &'a HashMap<LocalId, String>,
    /// Raw `DefId`s of classes with a constructor thunk — a class ref of one
    /// of these may flow as a value (it constructs via `dispatch_call_value`);
    /// others keep skipping at the use-as-value sites.
    ctor_thunk_classes: &'a HashSet<u32>,
    /// The class this function belongs to (a method/constructor), or
    /// `None` outside any class — drives method-visibility checks.
    owning_class: Option<DefId>,
    /// Locals backed by a shared `Value::Cell` (lambda captures). Reads
    /// peel the cell, writes store into it, captures pass it raw.
    cell_locals: &'a HashSet<LocalId>,
    /// Locals holding a `MakeSuper` value — `(this local, parent class)`.
    /// A method call on one dispatches statically against the parent.
    super_locals: &'a HashMap<LocalId, (LocalId, String)>,
    /// Default-init blocks being used as entry-time param fillers (non-const
    /// default args): block id → (param local to store into, continuation
    /// block). Such a block's `Return(op)` stores `op` into the param and
    /// jumps to the continuation instead of returning from the function.
    default_fill: &'a HashMap<BlockId, (LocalId, Block)>,
    /// Locals holding an object literal — `o.field(args)` reads the field and
    /// invokes its (callable) value.
    object_locals: &'a HashSet<LocalId>,
    /// Per object-literal local, the field → value-operand map (for skipping a
    /// field that holds a user class reference, which can't be runtime-built).
    object_field_srcs: &'a HashMap<LocalId, HashMap<String, Operand>>,
    /// Coalesced op-budget charge for the current basic block. Per-op
    /// `charge(n)` calls accumulate here instead of emitting a runtime call
    /// each; [`flush_charge`](Self::flush_charge) emits a single
    /// `leek_charge_ops(pending)` at the block boundary (and before a
    /// back-edge budget check). Because a MIR block translates to straight-line
    /// code (no conditional sub-paths — short-circuits/ternaries are their own
    /// blocks), every accumulated op executes iff the block runs, so the total
    /// op count for a completing program is identical to per-op charging.
    pending_charge: u64,
}

/// A/B benchmark escape hatch (read once): when `LEEK_NATIVE_NO_COALESCE=1`,
/// op charges are emitted per-op rather than coalesced per block, reproducing
/// the pre-coalescing codegen for back-to-back comparison. Off by default.
fn no_coalesce() -> bool {
    static FLAG: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *FLAG.get_or_init(|| std::env::var_os("LEEK_NATIVE_NO_COALESCE").is_some_and(|v| v == "1"))
}

mod emit_call;
mod emit_method;
mod emit_rvalue;
mod emit_stmt;
mod emit_value;

fn unsupported(msg: impl Into<String>) -> NativeError {
    NativeError::Unsupported(msg.into())
}

/// The result kind of a builtin call, if it's a scalar math builtin the
/// native backend supports. Used both to type call-destination locals and
/// to wrap the JIT result. `tys` resolves the kinds of argument locals
/// (for the polymorphic `abs`/`min`/`max`).
fn call_result_ty(
    call: &leek_mir::ir::CallExpr,
    tys: &HashMap<LocalId, ValTy>,
    rets: &FnRets,
    program: &MirProgram,
    f_locals: &[LocalDecl],
    new_classes: &HashMap<LocalId, String>,
    aliased: &HashMap<LocalId, String>,
    classrefs: &HashMap<LocalId, String>,
    lang: Lang,
) -> Option<ValTy> {
    // A builtin call shadowed by a same-named global resolves to the global's
    // (dynamic) value at runtime — a boxed `Ref` — not the builtin's typed
    // result. Inferring the builtin's kind here would mis-coerce (e.g.
    // `cos = …; cos(…)` would narrow a boxed int to a real).
    if let Callee::Builtin(name) = &call.callee
        && program_writes_global(program, name)
    {
        return Some(ValTy::Ref);
    }
    // A method call that resolves to a USER method (instance or static) yields
    // that method's value — typed `Ref` (boxed), re-coerced to the dest's kind
    // at the call site by `user_call`. This must take precedence over the
    // builtin-name matching below so a method whose name collides with a math
    // builtin (`A.sqrt()`, `Debug.log()`) isn't mis-typed as the builtin's
    // scalar result. (A non-colliding user method already hits the
    // `None if is_method => Ref` fallback, so this only realigns collisions.)
    if let Callee::Method { receiver, method } = &call.callee {
        if let Some(c) = receiver_class(f_locals, new_classes, aliased, *receiver)
            .and_then(|cls| program.class_by_name(cls))
            && let Some(vt) = program.resolve_method(c, method, Some(call.args.len()))
        {
            // For a method on a *standalone* class (no parent, never extended →
            // dispatch is statically this method, no override can return a
            // different kind) that is *public* (so the call is always accessible
            // → never the null-on-inaccessible path), use the resolved method's
            // own return kind. That lets a recursive numeric method
            // (`this.fib(n-1) + this.fib(n-2)`) stay unboxed end-to-end. Every
            // other user method call stays a boxed `Ref`, re-coerced at the dest.
            let standalone = c.parent_def.is_none()
                && !program
                    .classes
                    .iter()
                    .any(|o| o.parent_def == Some(c.def_id));
            if standalone
                && matches!(vt.visibility, Visibility::Public)
                && let Some(def) = program.functions[vt.function_idx].def_id
            {
                return Some(rets.get(&def).copied().unwrap_or(ValTy::Ref));
            }
            return Some(ValTy::Ref);
        }
        if classrefs
            .get(receiver)
            .and_then(|cls| resolve_static_method(program, cls, method, call.args.len()))
            .is_some()
        {
            return Some(ValTy::Ref);
        }
    }
    let (name, is_method) = match &call.callee {
        Callee::Builtin(name) => (name.as_str(), false),
        // Method-form: a builtin (`x.f()`) keys off the name; a user method
        // (unrecognized name) yields a boxed value — typed `Ref` below.
        Callee::Method { method, .. } => (method.as_str(), true),
        // A user function call: its result kind comes from the program-wide
        // return-kind map.
        Callee::Function(def_id) => return rets.get(def_id).copied(),
        // An indirect call (lambda / function value) yields a boxed result.
        Callee::Indirect(_) => return Some(ValTy::Ref),
        _ => return None,
    };
    match name {
        // `abs(null)` is `0.0` (real) in v2+ but `0` (int) in v1 — the result
        // kind is version-dependent for a literal null arg.
        "abs" if matches!(call.args.first(), Some(Operand::Const(Const::Null))) => {
            if lang.version <= 1 {
                Some(ValTy::Int)
            } else {
                Some(ValTy::Real)
            }
        }
        // `abs` otherwise keeps the argument kind (real → real, dynamic →
        // boxed `Ref` — a bigint's `abs` is a bigint and must stay boxed —
        // else int).
        "abs" => match call.args.first().and_then(|op| operand_ty(op, tys)) {
            Some(ValTy::Real) => Some(ValTy::Real),
            Some(ValTy::Ref) => Some(ValTy::Ref),
            _ => Some(ValTy::Int),
        },
        "signum" => Some(ValTy::Int),
        "count" => Some(ValTy::Int),
        // `push` and generic builtins return a (boxed) dynamic value.
        "push" => Some(ValTy::Ref),
        n if is_generic_builtin(n) => Some(ValTy::Ref),
        // A bare builtin-class name called as a function (`Array()`, `Map()`)
        // constructs a (boxed) value.
        n if !is_method && leek_runtime::builtin_class_name(n).is_some() => Some(ValTy::Ref),
        "min" | "max" => {
            let a = call.args.first().and_then(|op| operand_ty(op, tys));
            let b = call.args.get(1).and_then(|op| operand_ty(op, tys));
            match (a, b) {
                (Some(x), Some(y)) => Some(join(x, y)),
                (Some(x), None) | (None, Some(x)) => Some(x),
                (None, None) => Some(ValTy::Int),
            }
        }
        // A math builtin with a dynamic (boxed) arg goes through the generic
        // `call_builtin` path, which returns null for a non-number — so the
        // result is a boxed `Ref`, not the math kind (else a null result would
        // mis-coerce to `0`/`0.0`). For the method form (`recv.sqrt()`), the
        // receiver is the implicit first arg (not in `call.args`).
        _ if leek_runtime::math_sig(name).is_some()
            && (call
                .args
                .iter()
                .any(|op| operand_ty(op, tys) == Some(ValTy::Ref))
                || matches!(&call.callee, Callee::Method { receiver, .. }
                    if operand_ty(&Operand::Local(*receiver), tys) == Some(ValTy::Ref))) =>
        {
            Some(ValTy::Ref)
        }
        _ => match leek_runtime::math_sig(name) {
            Some(MathSig::RealToReal | MathSig::RealRealToReal) => Some(ValTy::Real),
            Some(MathSig::RealToInt) => Some(ValTy::Int),
            // An unrecognized method name is a user-class method: its
            // (boxed) result is typed `Ref`. An unrecognized free builtin
            // stays `None` (untyped from this site).
            None if is_method => Some(ValTy::Ref),
            None => None,
        },
    }
}

fn const_bool(op: &Operand) -> Option<bool> {
    match op {
        Operand::Const(Const::Bool(b)) => Some(*b),
        Operand::Const(Const::Int(n)) => Some(*n != 0),
        _ => None,
    }
}

/// Whether a statement references a string-literal constant (which boxes
/// into a handle and pulls in the composite shims).
fn stmt_has_string_const(s: &Statement) -> bool {
    // A string, null, or bigint literal boxes into a handle and pulls in the
    // composite shims (box/unbox, `leek_truthy` for `!null` / dynamic
    // branches, and `leek_builtinN` for a math builtin on a boxed arg).
    let is_str = |o: &Operand| {
        matches!(
            o,
            Operand::Const(Const::String(_) | Const::BigInt(_) | Const::Null)
        )
    };
    match s {
        Statement::Assign(_, rv) => match rv {
            Rvalue::Use(o) | Rvalue::UseFresh(o) | Rvalue::Unary(_, o) | Rvalue::Index(_, o) => {
                is_str(o)
            }
            Rvalue::Binary(_, a, b) => is_str(a) || is_str(b),
            Rvalue::Array(es) => es.iter().any(is_str),
            _ => false,
        },
        Statement::Call { call, .. } => call.args.iter().any(is_str),
        _ => false,
    }
}

/// The constant integer value of an operand, if it is one.
/// A constant `**` exponent as an integer — an int literal, or a bool literal
/// (`true` → 1, `false` → 0, matching Leekscript's numeric coercion). Used to
/// decide the integer-power range; the exponent operand itself already lowers
/// to the same 0/1 i64 for the `leek_ipow` call.
fn const_pow_exp(op: &Operand) -> Option<i64> {
    match op {
        Operand::Const(Const::Int(n)) => Some(*n),
        Operand::Const(Const::Bool(b)) => Some(*b as i64),
        _ => None,
    }
}

/// A constant operand equal to zero (int `0`, bool `false`, or real `0.0`).
fn is_const_zero(op: &Operand) -> bool {
    match op {
        Operand::Const(Const::Int(0)) => true,
        Operand::Const(Const::Bool(false)) => true,
        Operand::Const(Const::Real(bits)) => f64::from_bits(*bits) == 0.0,
        _ => false,
    }
}

/// Bail with `Unsupported` if the program can produce a `null` result:
/// a reachable value-less `return`, no reachable value return at all, or
/// a reachable `return` of a never-assigned (null) local.
fn reject_null_result(main: &MirFunction, allow_void: bool) -> Result<(), NativeError> {
    let by_id: HashMap<BlockId, &leek_mir::ir::BasicBlock> =
        main.blocks.iter().map(|b| (b.id, b)).collect();
    // Parameters are initialized by the caller, so a returned parameter
    // (`function id(x) { return x }`) is not a null result.
    let mut assigned: HashSet<LocalId> = main.params.iter().copied().collect();
    assigned.extend(
        main.blocks
            .iter()
            .flat_map(|b| &b.statements)
            .filter_map(|s| match s {
                Statement::Assign(Place::Local(id), _) => Some(*id),
                Statement::Call {
                    dest: Some(Place::Local(id)),
                    ..
                } => Some(*id),
                _ => None,
            }),
    );

    let mut seen = HashSet::new();
    let mut stack = vec![main.entry];
    let mut any_value_return = false;
    let mut any_null_return = false;
    while let Some(bid) = stack.pop() {
        if !seen.insert(bid) {
            continue;
        }
        let Some(block) = by_id.get(&bid) else {
            continue;
        };
        match &block.terminator {
            Terminator::Goto(t) => stack.push(*t),
            Terminator::Branch {
                cond,
                then_block,
                else_block,
            } => match const_bool(cond) {
                Some(true) => stack.push(*then_block),
                Some(false) => stack.push(*else_block),
                None => {
                    stack.push(*then_block);
                    stack.push(*else_block);
                }
            },
            Terminator::Switch { arms, default, .. } => {
                for (_, t) in arms {
                    stack.push(*t);
                }
                stack.push(*default);
            }
            Terminator::Return(None) => any_null_return = true,
            // A returned-but-never-assigned local is null at runtime. An
            // untyped (`Any`) local is modeled as a boxed `Ref` null (see
            // `infer_local_tys`), so returning it yields the right value — let
            // it through as a null return. A scalar/boolean-typed uninitialized
            // local can't hold null (it would return a bogus `0`/`false`), so
            // keep skipping those.
            Terminator::Return(Some(Operand::Local(id))) if !assigned.contains(id) => {
                if matches!(main.locals[id.0 as usize].ty, Type::Any) {
                    any_null_return = true;
                } else {
                    return Err(unsupported("returns an uninitialized (null) local"));
                }
            }
            Terminator::Return(Some(_)) => any_value_return = true,
            Terminator::Unreachable => {}
        }
    }
    // A function mixing value and explicit-null returns is modeled with a
    // boxed `Ref` result (`ret_valty` joins the null return to `Ref`) — as
    // long as it has no *concrete scalar* return type (which couldn't hold
    // null). A `=> integer`-typed function that also returns null is the
    // subtle coerce-null-to-0 case; keep skipping it.
    if any_value_return && any_null_return && scalar_valty(&main.return_ty).is_some() {
        return Err(unsupported("mixed value / null result (typed return)"));
    }
    if !any_value_return && !allow_void {
        return Err(unsupported("null / void result"));
    }
    Ok(())
}

fn rvalue_name(rv: &Rvalue) -> &'static str {
    match rv {
        Rvalue::Use(_) => "use",
        Rvalue::UseFresh(_) => "use-fresh",
        Rvalue::Binary(..) => "binary",
        Rvalue::Unary(..) => "unary",
        Rvalue::Cast(..) => "cast",
        Rvalue::Field(..) => "field",
        Rvalue::Index(..) => "index",
        Rvalue::Slice(..) => "slice",
        Rvalue::Array(_) => "array",
        Rvalue::Map(_) => "map",
        Rvalue::Set(_) => "set",
        Rvalue::Object(_) => "object",
        Rvalue::New { .. } => "new",
        Rvalue::Interval(_) => "interval",
        Rvalue::MakeForeachIter(_) => "foreach-iter",
        Rvalue::MakeLambda { .. } => "make-lambda",
        Rvalue::FunctionRef(_) => "function-ref",
        Rvalue::GlobalRef(..) => "global-ref",
        Rvalue::BuiltinRef(_) => "builtin-ref",
        Rvalue::This => "this",
        Rvalue::ClassSelf => "class-self",
        Rvalue::MakeSuper { .. } => "make-super",
        Rvalue::Super => "super",
        Rvalue::ClassRef(..) => "class-ref",
        Rvalue::Unsupported(_) => "unsupported",
    }
}

fn rvalue_ty(rv: &Rvalue, tys: &HashMap<LocalId, ValTy>) -> Option<ValTy> {
    match rv {
        Rvalue::Use(op) | Rvalue::UseFresh(op) => operand_ty(op, tys),
        Rvalue::Binary(op, l, r) => {
            let lt = operand_ty(l, tys);
            let rt = operand_ty(r, tys);
            // A boxed operand makes the whole op dynamic — the runtime
            // `apply_binary` returns a (boxed) value of unknown kind.
            if lt == Some(ValTy::Ref) || rt == Some(ValTy::Ref) {
                return Some(ValTy::Ref);
            }
            Some(match op {
                BinOp::Eq
                | BinOp::Ne
                | BinOp::IdentityEq
                | BinOp::IdentityNe
                | BinOp::Lt
                | BinOp::Le
                | BinOp::Gt
                | BinOp::Ge
                | BinOp::Xor => ValTy::Bool,
                BinOp::Div => ValTy::Real,
                // Bitwise / shift ops always yield an integer (a real operand
                // truncates), even when an operand is typed `real`.
                BinOp::BitAnd
                | BinOp::BitOr
                | BinOp::BitXor
                | BinOp::CompoundXor
                | BinOp::ShiftL
                | BinOp::ShiftR
                | BinOp::UShiftR => ValTy::Int,
                _ => {
                    if lt == Some(ValTy::Real) || rt == Some(ValTy::Real) {
                        ValTy::Real
                    } else {
                        ValTy::Int
                    }
                }
            })
        }
        Rvalue::Unary(UnOp::Not, _) => Some(ValTy::Bool),
        Rvalue::Unary(_, x) => operand_ty(x, tys),
        // Composite literals, element/field reads, globals, and foreach
        // iterators are all handles.
        Rvalue::Array(_)
        | Rvalue::Map(_)
        | Rvalue::Set(_)
        | Rvalue::Interval(_)
        | Rvalue::Object(_)
        | Rvalue::Index(..)
        | Rvalue::Slice(..)
        | Rvalue::Field(..)
        | Rvalue::GlobalRef(..)
        | Rvalue::New { .. }
        | Rvalue::BuiltinRef(_)
        | Rvalue::MakeLambda { .. }
        | Rvalue::ClassRef(..)
        | Rvalue::FunctionRef(_)
        // `expr as T` is lowered through the `leek_apply_cast` shim, which
        // always returns a boxed handle — so the cast result is a `Ref`
        // regardless of the target kind (a numeric-typed destination unboxes
        // it on coercion).
        | Rvalue::Cast(..)
        | Rvalue::MakeForeachIter(_) => Some(ValTy::Ref),
        _ => None,
    }
}

fn operand_ty(op: &Operand, tys: &HashMap<LocalId, ValTy>) -> Option<ValTy> {
    match op {
        Operand::Local(id) => tys.get(id).copied(),
        Operand::Const(Const::Int(_)) => Some(ValTy::Int),
        Operand::Const(Const::Bool(_)) => Some(ValTy::Bool),
        Operand::Const(Const::Real(_)) => Some(ValTy::Real),
        Operand::Const(Const::String(_) | Const::BigInt(_) | Const::Null) => Some(ValTy::Ref),
    }
}

/// The pinned value-kind for an explicit numeric declared type, if any.
/// `integer`/`real` (and their `?`-nullable forms) pin; everything else
/// (including `boolean`) is left to assignment inference.
fn pinned_valty(t: &Type) -> Option<ValTy> {
    match t {
        Type::Integer => Some(ValTy::Int),
        Type::Real => Some(ValTy::Real),
        // A `big_integer` slot always holds a boxed `Value::BigInt` —
        // pin it to `Ref` so inference can't narrow it to an unboxed
        // int from its (int-constant) assignments. Likewise nullable
        // (`Ref` holds null fine, and the store coercion still applies).
        Type::BigInteger => Some(ValTy::Ref),
        Type::Nullable(t) if matches!(t.as_ref(), Type::BigInteger) => Some(ValTy::Ref),
        // A nullable type can hold null, so it isn't a fixed scalar.
        _ => None,
    }
}

/// The element kind of a typed numeric array (`Array<integer>` /
/// `Array<real>`), if `t` is one. `None` for untyped arrays (`Array` /
/// `Array<Any>`) and non-arrays — those don't coerce element writes.
fn array_elem_valty(t: &Type) -> Option<ValTy> {
    match t {
        Type::Array(inner) => match inner.as_ref() {
            Type::Integer => Some(ValTy::Int),
            Type::Real => Some(ValTy::Real),
            _ => None,
        },
        Type::Nullable(inner) => array_elem_valty(inner),
        _ => None,
    }
}

/// A parameter's `default_init` entry block id IF the default is "fillable" by
/// the callee-side entry mechanism: its sub-CFG (reachable from the entry
/// block) consists only of control-flow (`Goto`/`Branch`/`Switch`) and
/// value-returning `Return(Some(_))` exits — covering both a single
/// `return <expr>` block and a multi-block conditional (`y = c ? a : b`). A
/// `Return(None)` / `Unreachable` makes it unfillable (the function still
/// compiles, with the default sub-CFG dead, and the call site skips on omit).
fn fillable_default(f: &MirFunction, param: LocalId) -> Option<BlockId> {
    let entry = f.locals[param.0 as usize].default_init?;
    let by_id: HashMap<BlockId, &leek_mir::ir::BasicBlock> =
        f.blocks.iter().map(|b| (b.id, b)).collect();
    let mut seen: HashSet<BlockId> = HashSet::new();
    let mut stack = vec![entry];
    while let Some(bid) = stack.pop() {
        if !seen.insert(bid) {
            continue;
        }
        let b = by_id.get(&bid)?;
        match &b.terminator {
            Terminator::Return(Some(_)) => {}
            Terminator::Return(None) | Terminator::Unreachable => return None,
            Terminator::Goto(t) => stack.push(*t),
            Terminator::Branch {
                then_block,
                else_block,
                ..
            } => {
                stack.push(*then_block);
                stack.push(*else_block);
            }
            Terminator::Switch { arms, default, .. } => {
                stack.extend(arms.iter().map(|(_, t)| *t));
                stack.push(*default);
            }
        }
    }
    Some(entry)
}

/// The `Return(Some(_))`-terminated blocks in a default's sub-CFG (reachable
/// from `entry`) — each becomes an entry-time param filler (store the returned
/// value into the param var + jump to the continuation).
fn default_return_blocks(f: &MirFunction, entry: BlockId) -> Vec<BlockId> {
    let by_id: HashMap<BlockId, &leek_mir::ir::BasicBlock> =
        f.blocks.iter().map(|b| (b.id, b)).collect();
    let mut seen: HashSet<BlockId> = HashSet::new();
    let mut stack = vec![entry];
    let mut out = Vec::new();
    while let Some(bid) = stack.pop() {
        if !seen.insert(bid) {
            continue;
        }
        let Some(b) = by_id.get(&bid) else { continue };
        match &b.terminator {
            Terminator::Return(Some(_)) => out.push(bid),
            Terminator::Goto(t) => stack.push(*t),
            Terminator::Branch {
                then_block,
                else_block,
                ..
            } => {
                stack.push(*then_block);
                stack.push(*else_block);
            }
            Terminator::Switch { arms, default, .. } => {
                stack.extend(arms.iter().map(|(_, t)| *t));
                stack.push(*default);
            }
            _ => {}
        }
    }
    out
}

/// A parameter's *self-contained constant* default value, if any — the
/// `default_init` block is a single `Return` of a constant (directly, or a
/// const assigned to the returned local). `None` when the param has no
/// default or its default references other params / builds a composite (in
/// which case it must be evaluated in the callee's frame, which the call
/// site can't do).
fn const_default(f: &MirFunction, param: LocalId) -> Option<Const> {
    let bb = f.locals[param.0 as usize].default_init?;
    let block = f.blocks.get(bb.0 as usize).filter(|b| b.id == bb)?;
    match &block.terminator {
        Terminator::Return(Some(Operand::Const(c))) => Some(c.clone()),
        Terminator::Return(Some(Operand::Local(t))) => {
            // `tmp = <const>; return tmp` — only a constant assignment to the
            // returned local (no other statements touching it) qualifies.
            block.statements.iter().rev().find_map(|s| match s {
                Statement::Assign(
                    Place::Local(id),
                    Rvalue::Use(Operand::Const(c)) | Rvalue::UseFresh(Operand::Const(c)),
                ) if id == t => Some(c.clone()),
                _ => None,
            })
        }
        _ => None,
    }
}

/// A resolved default for an omitted trailing call argument: either a single
/// scalar constant (padded via an `Operand::Const`) or a compile-time-folded
/// composite value (boxed + deep-cloned fresh per call).
enum DefaultArg {
    Const(Const),
    Composite(leek_runtime::Value),
}

/// Compile-time-evaluate a parameter's `default_init` block to a constant
/// `leek_runtime::Value` when the default is *self-contained* — only literals,
/// composite literals (`[1, [2, 3]]`, `['x': (1+2)*3]`, sets, objects), and
/// constant arithmetic. Returns `None` when it references a param/`this`/
/// global, indexes/fields, calls a function, or uses any other rvalue (the
/// call site then skips). Composite construction mirrors the interpreter's
/// `Rvalue::{Array,Map,Set,Object}` exactly (same `key_repr` canonicalization),
/// so the folded value matches. The caller boxes it once and deep-clones per
/// call, matching the interpreter's fresh-per-call default re-evaluation.
fn const_eval_default(f: &MirFunction, param: LocalId, version: u8) -> Option<leek_runtime::Value> {
    let bb = f.locals[param.0 as usize].default_init?;
    let block = f.blocks.get(bb.0 as usize).filter(|b| b.id == bb)?;
    let mut scratch: HashMap<LocalId, leek_runtime::Value> = HashMap::new();
    for s in &block.statements {
        match s {
            // Op-metering charges are runtime no-ops for a pure value fold.
            Statement::Charge(_) => {}
            Statement::Assign(Place::Local(id), rv) => {
                let v = const_eval_rvalue(rv, &scratch, version)?;
                scratch.insert(*id, v);
            }
            // Any other statement (field/index/global write) means the default
            // isn't a self-contained value — bail.
            _ => return None,
        }
    }
    match &block.terminator {
        Terminator::Return(Some(op)) => const_eval_operand(op, &scratch, version),
        _ => None,
    }
}

fn const_eval_operand(
    op: &Operand,
    scratch: &HashMap<LocalId, leek_runtime::Value>,
    version: u8,
) -> Option<leek_runtime::Value> {
    match op {
        Operand::Const(c) => Some(const_to_value(c, version)),
        Operand::Local(id) => scratch.get(id).cloned(),
    }
}

fn const_to_value(c: &Const, _version: u8) -> leek_runtime::Value {
    use leek_runtime::Value as V;
    match c {
        Const::Null => V::Null,
        Const::Bool(b) => V::Bool(*b),
        Const::Int(i) => V::Int(*i),
        Const::Real(bits) => V::Real(f64::from_bits(*bits)),
        Const::BigInt(s) => V::BigInt(std::rc::Rc::new(leek_runtime::big_from_decimal(s))),
        Const::String(s) => V::String(std::rc::Rc::new(s.clone())),
    }
}

fn const_eval_rvalue(
    rv: &Rvalue,
    scratch: &HashMap<LocalId, leek_runtime::Value>,
    version: u8,
) -> Option<leek_runtime::Value> {
    use leek_runtime::Value as V;
    use std::cell::RefCell;
    use std::rc::Rc;
    match rv {
        Rvalue::Use(op) | Rvalue::UseFresh(op) => const_eval_operand(op, scratch, version),
        Rvalue::Array(elems) => {
            let vs = elems
                .iter()
                .map(|o| const_eval_operand(o, scratch, version))
                .collect::<Option<Vec<_>>>()?;
            Some(V::Array(Rc::new(RefCell::new(vs))))
        }
        Rvalue::Set(items) => {
            let mut s = leek_runtime::SetData::new();
            for item in items {
                match item {
                    SetElem::One(o) => {
                        s.insert(const_eval_operand(o, scratch, version)?);
                    }
                    // Range length depends on runtime bound values; don't const-fold.
                    SetElem::Range(..) => return None,
                }
            }
            Some(V::Set(Rc::new(RefCell::new(s))))
        }
        Rvalue::Map(pairs) => {
            let mut m = leek_runtime::MapData::new();
            for (k, v) in pairs {
                let kv = const_eval_operand(k, scratch, version)?;
                let vv = const_eval_operand(v, scratch, version)?;
                let canon = leek_runtime::key_repr(&kv);
                m.insert_canonical(canon, kv, vv);
            }
            Some(V::Map(Rc::new(RefCell::new(m))))
        }
        Rvalue::Object(pairs) => {
            let mut o = leek_runtime::ObjectData::new();
            for (k, v) in pairs {
                o.set(k, const_eval_operand(v, scratch, version)?);
            }
            Some(V::Object(Rc::new(RefCell::new(o))))
        }
        Rvalue::Binary(op, l, r) => {
            let lv = const_eval_operand(l, scratch, version)?;
            let rv2 = const_eval_operand(r, scratch, version)?;
            Some(crate::runtime::apply_binop(*op, &lv, &rv2, version))
        }
        // Unary, Index, Field, New, calls, refs — not self-contained / not
        // modelled here. Skip (the call site falls back to the existing
        // const-default-or-skip path).
        _ => None,
    }
}

/// The scalar value-kind of a typed map's *value* type (`Map<K, real>` →
/// `Real`), so `m[k] = v` coerces `v` like a typed array element does.
fn map_value_valty(t: &Type) -> Option<ValTy> {
    match t {
        Type::Map(_, v) => match v.as_ref() {
            Type::Integer => Some(ValTy::Int),
            Type::Real => Some(ValTy::Real),
            _ => None,
        },
        Type::Nullable(inner) => map_value_valty(inner),
        _ => None,
    }
}

/// The scalar value-kind of a declared type, including `boolean`. Used for
/// function parameter/result ABI typing (where a `boolean` slot is a clean
/// `i64`, unlike the in-body inference where it stays untyped).
fn scalar_valty(t: &Type) -> Option<ValTy> {
    match t {
        Type::Integer => Some(ValTy::Int),
        Type::Real => Some(ValTy::Real),
        Type::Boolean => Some(ValTy::Bool),
        // A nullable type (`integer?`) must be a boxed `Ref`: it can be
        // null, which an unboxed scalar can't represent (it would coerce
        // null → 0).
        _ => None,
    }
}

/// The scalar kind a (possibly nullable) declared field/param coerces a
/// *non-null* value to — looks through `Nullable` (`real? x = 5` stores
/// `5.0`). Distinct from [`scalar_valty`], which makes nullable types
/// `Ref` for representation.
fn coerce_target_ty(t: &Type) -> Option<ValTy> {
    match t {
        Type::Integer => Some(ValTy::Int),
        Type::Real => Some(ValTy::Real),
        Type::Boolean => Some(ValTy::Bool),
        Type::Nullable(inner) => coerce_target_ty(inner),
        _ => None,
    }
}
