//! MIR → Cranelift IR translation for the scalar (integer / real /
//! boolean) + control-flow subset. Anything outside that subset returns
//! [`NativeError::Unsupported`] so callers can fall back / skip.

use std::collections::{HashMap, HashSet};

use cranelift::codegen;
use cranelift::codegen::ir::FuncRef;
use cranelift::prelude::{
    types, AbiParam, Block, FloatCC, InstBuilder, IntCC, MemFlags, StackSlotData, StackSlotKind,
    TrapCode, Type as ClType, Value,
};
use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext, Variable};
use cranelift_module::{Linkage, Module};

use leek_hir::DefId;
use leek_mir::ir::{
    BasicBlock, BinOp, BlockId, Callee, CastKind, Const, FunctionKind, LocalDecl, LocalId,
    LocalKind, MirFunction, MirProgram, Operand, Place, Rvalue, Statement, Terminator, UnOp,
    Visibility,
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
    let extended: HashSet<DefId> = program.classes.iter().filter_map(|c| c.parent_def).collect();
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
    specialize_pass(program, lang, &candidates, &method_def, Type::Integer, ValTy::Int);
    let remaining: Vec<_> = candidates
        .iter()
        .copied()
        .filter(|&(fi, pid, _, _)| matches!(program.functions[fi].locals[pid.0 as usize].ty, Type::Any))
        .collect();
    specialize_pass(program, lang, &remaining, &method_def, Type::Real, ValTy::Real);
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
    pin: Type,
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
                    Operand::Local(id) => {
                        tys[caller].as_ref().and_then(|t| t.get(id).copied()).unwrap_or(ValTy::Ref)
                    }
                }
            };
            let mut demote = HashSet::new();
            for &(fi, pid, pi, def) in candidates {
                // Only this pass's still-pinned candidates.
                if program.functions[fi].locals[pid.0 as usize].ty != pin {
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
                        if let Some(name) = receiver_class(&f.locals, &new_classes, &aliased, *receiver)
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
                            Rvalue::Array(es) | Rvalue::Set(es) => es.iter().collect(),
                            Rvalue::Object(fs) => fs.iter().map(|(_, v)| v).collect(),
                            Rvalue::Map(kvs) => {
                                kvs.iter().flat_map(|(k, v)| [k, v]).collect()
                            }
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

/// The set of `program.functions` indices that are lambda bodies — i.e.
/// referenced by some `MakeLambda { function_idx }`. A lambda defined
/// inside a method carries that method's `owning_class`, so it can't be
/// told apart from a field initializer by `owning_class` alone; membership
/// here is the authoritative signal that a `DefId`-less function must be
/// compiled with the uniform `(argv, argc)` ABI and registered for dynamic
/// invocation rather than compiled as a (typed) field initializer.
pub fn lambda_body_idxs(program: &MirProgram) -> HashSet<usize> {
    let mut out = HashSet::new();
    for f in &program.functions {
        for b in &f.blocks {
            for s in &b.statements {
                if let Statement::Assign(_, Rvalue::MakeLambda { function_idx, .. }) = s {
                    out.insert(*function_idx);
                }
            }
        }
    }
    out
}

/// True if by-ref param `id` is only ever *read* or *plain-reassigned*
/// (`Assign(Place::Local(id), _)`) in `f` — never composite-mutated in place
/// (`id[k]=`/`push(id,…)`), captured by a nested closure, passed to a call, used
/// as a receiver/indirect-callee, returned, or promoted. In **v2+** such a param
/// on a *lambda* value has no observable by-reference effect (a lambda's `@x`
/// doesn't propagate to the caller in v2+ — only named functions do, via
/// `byref_cells_threadable`), so it compiles as a plain by-value param.
fn byref_param_pure_local(f: &MirFunction, id: LocalId) -> bool {
    let op_is = |o: &Operand| matches!(o, Operand::Local(l) if *l == id);
    for b in &f.blocks {
        for s in &b.statements {
            match s {
                Statement::Assign(place, rv) => {
                    match place {
                        Place::Field(l, _) | Place::Index(l, _) | Place::Slice(l, _)
                            if *l == id =>
                        {
                            return false;
                        }
                        Place::LambdaCapture { lambda, .. } if *lambda == id => return false,
                        _ => {}
                    }
                    if let Rvalue::MakeLambda { captures, .. } = rv
                        && captures.iter().any(op_is)
                    {
                        return false;
                    }
                }
                Statement::Call { dest, call } => {
                    if let Some(Place::Field(l, _) | Place::Index(l, _) | Place::Slice(l, _)) = dest
                        && *l == id
                    {
                        return false;
                    }
                    if call.args.iter().any(op_is) {
                        return false;
                    }
                    match &call.callee {
                        Callee::Method { receiver, .. } if *receiver == id => return false,
                        Callee::Indirect(l) if *l == id => return false,
                        Callee::SuperConstructor { this, .. } if *this == id => return false,
                        _ => {}
                    }
                }
                Statement::ApplyPromotion(l) if *l == id => return false,
                _ => {}
            }
        }
        if let Terminator::Return(Some(op)) = &b.terminator
            && op_is(op)
        {
            return false;
        }
    }
    true
}

/// True if lambda body `lambda_fi` is ever handed to a writeback HOF builtin
/// (`arrayMap`/…) anywhere — directly or through a `Use`-copy of its value. Such
/// a lambda's by-ref params ARE written back by the runtime HOF machinery, so
/// they must NOT be treated as no-op by-value params.
fn lambda_passed_to_hof(program: &MirProgram, lambda_fi: usize) -> bool {
    for f in &program.functions {
        // Locals holding this lambda value (its `MakeLambda` dest + `Use` copies).
        let mut holders: HashSet<LocalId> = HashSet::new();
        loop {
            let before = holders.len();
            for s in f.blocks.iter().flat_map(|b| &b.statements) {
                if let Statement::Assign(Place::Local(d), rv) = s {
                    let holds = match rv {
                        Rvalue::MakeLambda { function_idx, .. } => *function_idx == lambda_fi,
                        Rvalue::Use(Operand::Local(src)) | Rvalue::UseFresh(Operand::Local(src)) => {
                            holders.contains(src)
                        }
                        _ => false,
                    };
                    if holds {
                        holders.insert(*d);
                    }
                }
            }
            if holders.len() == before {
                break;
            }
        }
        if holders.is_empty() {
            continue;
        }
        for s in f.blocks.iter().flat_map(|b| &b.statements) {
            if let Statement::Call { call, .. } = s
                && matches!(&call.callee, Callee::Builtin(n) if WRITEBACK_HOFS.contains(&n.as_str()))
                && call.args.iter().any(|a| matches!(a, Operand::Local(l) if holders.contains(l)))
            {
                return true;
            }
        }
    }
    false
}

/// The by-ref params of `fi` that compile as plain by-value params in v2+,
/// because their `@x` has no observable by-reference effect there. This holds
/// for a **lambda** or a **method**: in v2+ neither propagates a `@x`
/// reassignment to the caller (only a plain top-level *named function* does, via
/// `byref_cells_threadable`) — interpreter-confirmed (`o.m(@x){x=9}` leaves the
/// caller's argument unchanged). A named top-level function is NOT no-op (it
/// threads). The param must be pure-local ([`byref_param_pure_local`]) — an
/// in-place mutation still propagates through the shared `Rc`, and an escaping
/// one needs a real cell — and a lambda must not be a writeback-HOF callback
/// (the runtime writes those back). Empty for v1.
fn noop_byref_params(program: &MirProgram, fi: usize, version: u8) -> HashSet<LocalId> {
    if version < 2 {
        return HashSet::new();
    }
    let f = &program.functions[fi];
    let is_lambda = lambda_body_idxs(program).contains(&fi);
    let is_method = f.owning_class.is_some();
    // A named top-level function propagates `@x` (threaded), so it is not no-op.
    if !is_lambda && !is_method {
        return HashSet::new();
    }
    // A HOF-callback lambda's by-ref params are written back by the runtime.
    if is_lambda && lambda_passed_to_hof(program, fi) {
        return HashSet::new();
    }
    f.params
        .iter()
        .copied()
        .filter(|&p| f.locals[p.0 as usize].is_by_ref && byref_param_pure_local(f, p))
        .collect()
}

/// True if class `child_def` is `ancestor_def` or descends from it (cycle-safe
/// walk of `parent_def`). Free-function form of `Tx::class_descends_from`.
fn class_descends_from(program: &MirProgram, child_def: DefId, ancestor_def: DefId) -> bool {
    let mut cur = Some(child_def);
    let mut seen: HashSet<DefId> = HashSet::new();
    while let Some(d) = cur {
        if !seen.insert(d) {
            return false;
        }
        if d == ancestor_def {
            return true;
        }
        cur = program.class(d).and_then(|c| c.parent_def);
    }
    false
}

/// Virtually-dispatched method targets in `f`: for each `recv.m(args)` on a
/// known instance whose static class `C` has `m` AND a subclass *overrides* it
/// (so the receiver's runtime class decides which body runs), register
/// `(fn_idx, class, m)` for `C` and every descendant — seeding `METHOD_RESOLVE`,
/// the uniform-compile set, and reachability so the call can be lowered as a
/// runtime-class-keyed bound-method dispatch (`leek_value_index` + `call_value`).
fn virtual_method_targets(program: &MirProgram, f: &MirFunction) -> Vec<(usize, String, String)> {
    let new_classes = new_class_locals(f);
    let aliased = aliased_class_locals(f);
    let mut out = Vec::new();
    for b in &f.blocks {
        for s in &b.statements {
            let Statement::Call { call, .. } = s else {
                continue;
            };
            let Callee::Method { receiver, method } = &call.callee else {
                continue;
            };
            let Some(cls_name) = receiver_class(&f.locals, &new_classes, &aliased, *receiver) else {
                continue;
            };
            let Some(c) = program.class_by_name(cls_name) else {
                continue;
            };
            let arity = call.args.len();
            let overridden = program.classes.iter().any(|sc| {
                sc.def_id != c.def_id
                    && class_descends_from(program, sc.def_id, c.def_id)
                    && sc
                        .methods
                        .iter()
                        .any(|m| !m.is_static && m.name == *method && m.user_arity == arity)
            });
            if !overridden {
                continue;
            }
            for x in &program.classes {
                if (x.def_id == c.def_id || class_descends_from(program, x.def_id, c.def_id))
                    && let Some(vt) = program.resolve_method(x, method, Some(arity))
                {
                    out.push((vt.function_idx, x.name.clone(), method.clone()));
                }
            }
        }
    }
    out
}

/// Methods of `f` that are read as *values* via index syntax — `obj['m']`
/// (which the interpreter turns into a bound method). Each entry is
/// `(function_idx, class_name, method_name)`. A literal key naming a *field*
/// is a plain field read and excluded; a dynamic key could name any method,
/// so every method of the (statically known) class is included. These edges
/// make the method reachable (so it + its callees compile) and seed the
/// runtime method-resolution table.
fn index_method_targets(program: &MirProgram, f: &MirFunction) -> Vec<(usize, String, String)> {
    let mut out = Vec::new();
    let new_classes = new_class_locals(f);
    let aliased = aliased_class_locals(f);
    // A literal member name on an instance receiver (`obj['m']` or `obj.m`)
    // that resolves to a method (not a stored field) is a bound-method value.
    let named = |base: &LocalId, name: &str, out: &mut Vec<(usize, String, String)>| {
        if let Some(cls) = receiver_class(&f.locals, &new_classes, &aliased, *base)
            && let Some(c) = program.class_by_name(cls)
            && c.field_slot(name).is_none()
            && let Some(vt) = program.resolve_method(c, name, None)
        {
            out.push((vt.function_idx, cls.to_string(), name.to_string()));
        }
    };
    for b in &f.blocks {
        for s in &b.statements {
            match s {
                Statement::Assign(_, Rvalue::Index(base, Operand::Const(Const::String(name)))) => {
                    named(base, name, &mut out);
                }
                // `obj.m` field read of an instance method → bound method.
                Statement::Assign(_, Rvalue::Field(base, name)) => {
                    named(base, name, &mut out);
                }
                // A *dynamic* index key could name any method — register all.
                Statement::Assign(_, Rvalue::Index(base, _)) => {
                    if let Some(cls) = receiver_class(&f.locals, &new_classes, &aliased, *base)
                        && let Some(c) = program.class_by_name(cls)
                    {
                        for e in &c.vtable {
                            out.push((e.function_idx, cls.to_string(), e.name.clone()));
                        }
                    }
                }
                _ => {}
            }
        }
    }
    out
}

/// Method calls in `f` whose receiver's class is NOT statically known
/// (`receiver_class` returns `None`) and whose method name is a *user* method
/// (not a builtin) of some class — `(function_idx, class, method)` for every
/// class declaring that method, at the call's arity. These dispatch at runtime
/// on the receiver's actual class (`leek_call_method`), so every candidate
/// method is seeded into `METHOD_RESOLVE`, uniform-compiled, and made reachable.
/// Mirrors `dynamic_method_dispatch_candidate` in `try_user_method` (they MUST
/// agree). Builtin method names are excluded — those keep the existing
/// builtin-method path and would otherwise force-compile unrelated user methods.
fn dynamic_method_targets(program: &MirProgram, f: &MirFunction) -> Vec<(usize, String, String)> {
    let new_classes = new_class_locals(f);
    let aliased = aliased_class_locals(f);
    let classrefs = classref_locals(f);
    let mut out = Vec::new();
    for b in &f.blocks {
        for s in &b.statements {
            let Statement::Call { call, .. } = s else { continue };
            let Callee::Method { receiver, method } = &call.callee else { continue };
            // Known-class receivers (incl. class refs for static calls) use the
            // direct/virtual path; only *unknown* receivers dispatch dynamically.
            if receiver_class(&f.locals, &new_classes, &aliased, *receiver).is_some()
                || classrefs.contains_key(receiver)
                || leek_runtime::is_known_builtin(method)
            {
                continue;
            }
            for c in &program.classes {
                if let Some(vt) = program.resolve_method(c, method, Some(call.args.len())) {
                    out.push((vt.function_idx, c.name.clone(), method.clone()));
                }
            }
        }
    }
    out
}

/// Build the method-resolution table (`(class, method)` → function index) and
/// the set of method indices that must be compiled with the uniform ABI for
/// dynamic (bound-method) invocation, by scanning `reachable` function bodies
/// for `obj['m']` value reads, virtually-dispatched calls, and
/// unknown-receiver dynamic method calls.
pub fn method_value_info(
    program: &MirProgram,
    reachable_indices: &[usize],
) -> (HashMap<(String, String), usize>, HashSet<usize>) {
    let mut table = HashMap::new();
    let mut set = HashSet::new();
    for &fi in reachable_indices {
        for (fidx, cls, name) in index_method_targets(program, &program.functions[fi])
            .into_iter()
            .chain(virtual_method_targets(program, &program.functions[fi]))
            .chain(dynamic_method_targets(program, &program.functions[fi]))
        {
            table.insert((cls, name), fidx);
            set.insert(fidx);
        }
    }
    (table, set)
}

/// True when a reachable function needs `Value::Cell` sharing the handle
/// model doesn't implement — so the whole program must skip rather than
/// miscompile. Two shapes: an `@x` by-reference parameter (the caller and
/// callee must share a cell), or a closure that *writes* to a shared
/// captured variable (the write must propagate to the enclosing scope).
/// Read-only captures are fine — native's value-capture snapshot matches —
/// so this deliberately does NOT gate them.
/// True if `id` is ever *written*, passed to a call (where it could be
/// mutated in place or aliased onward), used as a method/super receiver, or
/// captured by a lambda — i.e. anything beyond a plain read of its value.
/// Used to tell a genuinely-aliased `@x` by-ref param from a read-only one.
/// Locals passed as arguments to an *indirect* (function-value) call. In v1
/// they become cells so a callee `@x` by-ref param can share the caller's
/// storage: the runtime `dispatch_call_value` threads the cell for by-ref
/// params and peels + (v1) deep-clones it for by-value params. v2+ lambda
/// by-ref is a no-op, so this is applied v1-only (callers gate on version).
fn indirect_arg_cell_locals(f: &MirFunction) -> HashSet<LocalId> {
    let mut out = HashSet::new();
    for b in &f.blocks {
        for s in &b.statements {
            if let Statement::Call { call, .. } = s
                && matches!(&call.callee, Callee::Indirect(_))
            {
                for a in &call.args {
                    if let Operand::Local(l) = a {
                        out.insert(*l);
                    }
                }
            }
        }
    }
    out
}

/// True if lambda by-ref param `id` *escapes* its body — captured by a nested
/// closure, returned, or passed onward to a user fn/method/indirect call. A
/// non-escaping reassigned/in-place by-ref param of a lambda is handled by
/// simple cell-threading at the (indirect) call site; an escaping one needs the
/// cell to survive the call (returns/captures), which this step doesn't do, so
/// it stays gated.
fn byref_lambda_param_escapes(f: &MirFunction, id: LocalId) -> bool {
    let op_is = |o: &Operand| matches!(o, Operand::Local(l) if *l == id);
    for b in &f.blocks {
        for s in &b.statements {
            match s {
                Statement::Assign(_, Rvalue::MakeLambda { captures, .. })
                    if captures.iter().any(op_is) =>
                {
                    return true;
                }
                Statement::Call { call, .. } => {
                    let onward = matches!(
                        &call.callee,
                        Callee::Function(_) | Callee::Indirect(_) | Callee::Method { .. }
                    );
                    if onward && call.args.iter().any(op_is) {
                        return true;
                    }
                    if let Callee::Method { receiver, .. } = &call.callee
                        && *receiver == id
                    {
                        return true;
                    }
                }
                _ => {}
            }
        }
        if let Terminator::Return(Some(op)) = &b.terminator
            && op_is(op)
        {
            return true;
        }
    }
    false
}

/// True if v1 by-ref param `id` needs true caller/callee `Value::Cell` aliasing
/// (so the program must skip), as opposed to propagating through the shared `Rc`
/// once the call site stops deep-cloning the by-ref arg. A cell is needed when
/// the binding is *reassigned* (`x = …` — a whole-local `Place::Local` write),
/// captured by a closure, passed onward to a user fn / method / indirect call
/// (clone-suppression only threads ONE level), used as a method/super receiver,
/// promoted, or returned. It is NOT needed for an in-place mutation
/// (`x[i] = …`, `push(x, …)` — a builtin call) or a plain read: those go through
/// the shared backing store the suppressed clone leaves intact.
fn byref_param_needs_cell_v1(f: &MirFunction, id: LocalId) -> bool {
    let op_is = |o: &Operand| matches!(o, Operand::Local(l) if *l == id);
    for b in &f.blocks {
        for s in &b.statements {
            match s {
                Statement::Assign(place, rv) => {
                    if let Place::Local(l) = place
                        && *l == id
                    {
                        return true;
                    }
                    if let Place::LambdaCapture { lambda, .. } = place
                        && *lambda == id
                    {
                        return true;
                    }
                    if let Rvalue::MakeLambda { captures, .. } = rv
                        && captures.iter().any(op_is)
                    {
                        return true;
                    }
                }
                Statement::Call { dest, call } => {
                    if let Some(Place::Local(l)) = dest
                        && *l == id
                    {
                        return true;
                    }
                    // Passed onward to a non-builtin call could be reassigned at
                    // the next level (clone-suppression only threads one hop).
                    let onward = matches!(
                        &call.callee,
                        Callee::Function(_) | Callee::Indirect(_) | Callee::Method { .. }
                    );
                    if onward && call.args.iter().any(op_is) {
                        return true;
                    }
                    match &call.callee {
                        Callee::Method { receiver, .. } if *receiver == id => return true,
                        Callee::SuperConstructor { this, .. } if *this == id => return true,
                        _ => {}
                    }
                }
                Statement::ApplyPromotion(l) if *l == id => return true,
                _ => {}
            }
        }
        if let Terminator::Return(Some(op)) = &b.terminator
            && op_is(op)
        {
            return true;
        }
    }
    false
}

/// The number of leading capture-slot parameters of lambda body `fi` — the
/// `captures.len()` of the `MakeLambda` that constructs it. `None` when `fi`
/// is never constructed as a lambda (a plain function). A lambda body's first
/// `n` params are the captured enclosing locals (prepended), the rest are the
/// user-visible params.
fn lambda_capture_count(program: &MirProgram, fi: usize) -> Option<usize> {
    for f in &program.functions {
        for s in f.blocks.iter().flat_map(|b| &b.statements) {
            if let Statement::Assign(_, Rvalue::MakeLambda { function_idx, captures }) = s
                && *function_idx == fi
            {
                return Some(captures.len());
            }
        }
    }
    None
}

/// True if by-ref capture-slot `id` of lambda body `f` is *only* read and then
/// handed back via `return @id`, with no other cell-needing use (no whole-local
/// reassignment, no nested-lambda capture, no onward pass to a user
/// fn/method/indirect call, no promotion). Returning such a slot is safe: it is
/// a shared `Value::Cell` originating in the enclosing frame, so handing the raw
/// cell back aliases live storage (the runtime peels the cell at every use
/// site). Anything more than read-and-return needs the heavier escaping-cell
/// machinery and stays gated.
fn byref_capture_only_returned(f: &MirFunction, id: LocalId) -> bool {
    let op_is = |o: &Operand| matches!(o, Operand::Local(l) if *l == id);
    let mut returned = false;
    for b in &f.blocks {
        for s in &b.statements {
            match s {
                Statement::Assign(Place::Local(l), _) if *l == id => return false,
                Statement::Assign(Place::LambdaCapture { lambda, .. }, _) if *lambda == id => {
                    return false;
                }
                Statement::Assign(_, Rvalue::MakeLambda { captures, .. })
                    if captures.iter().any(op_is) =>
                {
                    return false;
                }
                Statement::Call { dest, call } => {
                    if let Some(Place::Local(l)) = dest
                        && *l == id
                    {
                        return false;
                    }
                    let onward = matches!(
                        &call.callee,
                        Callee::Function(_) | Callee::Indirect(_) | Callee::Method { .. }
                    );
                    if onward && call.args.iter().any(op_is) {
                        return false;
                    }
                    match &call.callee {
                        Callee::Method { receiver, .. } if *receiver == id => return false,
                        Callee::SuperConstructor { this, .. } if *this == id => return false,
                        _ => {}
                    }
                }
                Statement::ApplyPromotion(l) if *l == id => return false,
                _ => {}
            }
        }
        if let Terminator::Return(Some(op)) = &b.terminator
            && op_is(op)
        {
            returned = true;
        }
    }
    returned
}

/// True if by-ref param `p` of `f` is captured by an inner lambda (`MakeLambda`
/// captures `p`). The param is backed by a shared `Value::Cell` (it is
/// `is_shared`); the inner lambda holds that same `Rc`, so when the lambda
/// escapes (is returned / stored) the cell survives. Mutations the lambda makes
/// propagate to the caller iff the caller's argument is threaded as that cell.
fn byref_param_captured_by_lambda(f: &MirFunction, p: LocalId) -> bool {
    f.blocks.iter().flat_map(|b| &b.statements).any(|s| {
        matches!(s, Statement::Assign(_, Rvalue::MakeLambda { captures, .. })
            if captures.iter().any(|o| matches!(o, Operand::Local(l) if *l == p)))
    })
}

/// True if by-ref param `p` is handed back via `return @p` (a `Return`
/// terminator whose operand is `p`, marked `is_by_ref`). The returned value is
/// the shared `Value::Cell`, so the caller's `var y = f(x)` aliases `p`'s
/// storage — propagating later in-place mutations of `y` back through the cell.
fn byref_param_returned(f: &MirFunction, p: LocalId) -> bool {
    f.blocks.iter().any(|b| {
        matches!(&b.terminator, Terminator::Return(Some(Operand::Local(l))) if *l == p)
    })
}

/// True if by-ref param `p` *escapes* `f` via being captured by an inner lambda
/// OR returned (`return @p`), and that escape is its *only* cell-need: it is not
/// reassigned in `f`'s own body, not aliased onward to another user fn / method
/// / indirect call, not used as a method/super receiver, and not promoted. Such
/// a param is cell-threaded end-to-end — the caller passes its shared
/// `Value::Cell` (directly via [`Tx::byref_cell_arg`], indirectly via the
/// runtime `thread_args`), the param reuses it (`leek_make_cell`), and the
/// escaped value (the capturing lambda, or the returned reference the caller
/// binds) shares it; a non-local argument simply gets a fresh cell (no aliasing
/// expected). `return @p` hands back the raw cell (see the `Return` terminator),
/// and `var y = f(x)` binds it without a v1-clone (a `Call` dest never clones),
/// so the caller aliases `p`'s storage. Only valid for a non-method function — a
/// `Callee::Method` call site isn't threaded.
fn byref_param_escape_threadable(f: &MirFunction, p: LocalId) -> bool {
    // Only a genuine `@x` by-ref param threads the caller's cell. A by-value
    // param captured by a closure (`function(array){ return function(e){
    // push(array, e) } }`) — or returned after a by-value clone — is
    // v1-deep-cloned at the call boundary, so the escaped value aliases the
    // *copy* and the caller's argument must NOT be cell-threaded.
    if f.owning_class.is_some()
        || !f.locals[p.0 as usize].is_by_ref
        || !(byref_param_captured_by_lambda(f, p) || byref_param_returned(f, p))
    {
        return false;
    }
    let op_is = |o: &Operand| matches!(o, Operand::Local(l) if *l == p);
    for s in f.blocks.iter().flat_map(|b| &b.statements) {
        match s {
            Statement::Assign(Place::Local(l), _) if *l == p => return false,
            Statement::Assign(Place::LambdaCapture { lambda, .. }, _) if *lambda == p => {
                return false;
            }
            Statement::Call { dest, call } => {
                if let Some(Place::Local(l)) = dest
                    && *l == p
                {
                    return false;
                }
                let onward = matches!(
                    &call.callee,
                    Callee::Function(_) | Callee::Indirect(_) | Callee::Method { .. }
                );
                if onward && call.args.iter().any(op_is) {
                    return false;
                }
                match &call.callee {
                    Callee::Method { receiver, .. } if *receiver == p => return false,
                    Callee::SuperConstructor { this, .. } if *this == p => return false,
                    _ => {}
                }
            }
            Statement::ApplyPromotion(l) if *l == p => return false,
            _ => {}
        }
    }
    true
}

/// The locals of `f` passed (by name) directly to a callee parameter that is a
/// captured-escaping cell param ([`byref_param_escape_threadable`]). Those
/// locals must become cells so the shared `Value::Cell` handle is what is
/// passed at the call site (the v1 analogue of [`byref_arg_cell_locals`], which
/// only handles reassigned/aliased-onward params).
fn byref_captured_arg_cell_locals(f: &MirFunction, program: &MirProgram) -> HashSet<LocalId> {
    let mut out = HashSet::new();
    for s in f.blocks.iter().flat_map(|b| &b.statements) {
        let Statement::Call { call, .. } = s else {
            continue;
        };
        let Callee::Function(def_id) = &call.callee else {
            continue;
        };
        let Some(g) = program.function(*def_id) else {
            continue;
        };
        for (i, arg) in call.args.iter().enumerate() {
            if let Some(&gp) = g.params.get(i)
                && byref_param_escape_threadable(g, gp)
                && let Operand::Local(l) = arg
            {
                out.insert(*l);
            }
        }
    }
    out
}

/// True if `f` reassigns the *whole* local `id` (a `Place::Local(id)` assignment
/// destination), as opposed to mutating its contents in place
/// (`Place::Index`/`Field`/`Slice`). A rebinding can't be carried by a plain
/// shared `Rc` — it needs a `Value::Cell`.
fn local_reassigned(f: &MirFunction, id: LocalId) -> bool {
    f.blocks
        .iter()
        .flat_map(|b| &b.statements)
        .any(|s| matches!(s, Statement::Assign(Place::Local(l), _) if *l == id))
}

/// True if by-ref param `p` of `f` *aliases onward*: it is passed (by name) to a
/// **by-ref** parameter of a user-function callee, so its alias chain must be
/// carried by a shared cell too. By-ref-ness is a static property, so this needs
/// no fixpoint — each level of an `f → g → h` chain detects the next locally.
fn byref_aliases_onward(f: &MirFunction, p: LocalId, program: &MirProgram) -> bool {
    for s in f.blocks.iter().flat_map(|b| &b.statements) {
        let Statement::Call { call, .. } = s else { continue };
        let Callee::Function(d) = &call.callee else { continue };
        let Some(g) = program.function(*d) else { continue };
        for (i, arg) in call.args.iter().enumerate() {
            if matches!(arg, Operand::Local(l) if *l == p)
                && let Some(&gp) = g.params.get(i)
                && g.locals[gp.0 as usize].is_by_ref
            {
                return true;
            }
        }
    }
    false
}

/// A function's by-reference parameters that need true caller/callee
/// `Value::Cell` aliasing: those *reassigned* in the body, or *aliased onward*
/// to another by-ref param. In-place-only by-ref params (the contents are
/// mutated through the shared `Rc` but the binding never changes and never
/// escapes) keep the cheaper shared-handle path and are excluded.
fn byref_cell_params(f: &MirFunction, program: &MirProgram) -> HashSet<LocalId> {
    f.params
        .iter()
        .copied()
        .filter(|&p| {
            f.locals[p.0 as usize].is_by_ref
                && (local_reassigned(f, p) || byref_aliases_onward(f, p, program))
        })
        .collect()
}

/// The locals of `f` passed (by name) to a callee parameter that needs a cell
/// ([`byref_cell_params`]). Those locals must themselves become cells in `f` so
/// the shared `Value::Cell` handle is what gets passed at the call site.
fn byref_arg_cell_locals(f: &MirFunction, program: &MirProgram) -> HashSet<LocalId> {
    let mut out = HashSet::new();
    for s in f.blocks.iter().flat_map(|b| &b.statements) {
        let Statement::Call { call, .. } = s else { continue };
        let Callee::Function(def_id) = &call.callee else { continue };
        let Some(g) = program.function(*def_id) else { continue };
        let cells = byref_cell_params(g, program);
        if cells.is_empty() {
            continue;
        }
        for (i, arg) in call.args.iter().enumerate() {
            if let Some(gp) = g.params.get(i)
                && cells.contains(gp)
                && let Operand::Local(l) = arg
            {
                out.insert(*l);
            }
        }
    }
    out
}

// ---- Exhaustive "does this MIR construct mention local `l`" predicates ----
// Used to prove a lambda value never escapes (so its `@`-by-ref params can be
// safely runtime-threaded by a writeback HOF builtin). The matches are
// exhaustive on purpose — the compiler then guarantees no operand position is
// silently missed, which would be a miscompile.

fn op_mentions(o: &Operand, l: LocalId) -> bool {
    matches!(o, Operand::Local(x) if *x == l)
}

fn slice_bounds_mentions(b: &leek_mir::ir::SliceBounds, l: LocalId) -> bool {
    [&b.start, &b.end, &b.step]
        .into_iter()
        .flatten()
        .any(|o| op_mentions(o, l))
}

fn place_mentions(p: &Place, l: LocalId) -> bool {
    match p {
        Place::Local(x) | Place::Field(x, _) => *x == l,
        Place::Index(x, idx) => *x == l || op_mentions(idx, l),
        Place::Slice(x, b) => *x == l || slice_bounds_mentions(b, l),
        Place::LambdaCapture { lambda, .. } => *lambda == l,
        Place::Global(..) => false,
    }
}

fn rvalue_mentions(r: &Rvalue, l: LocalId) -> bool {
    match r {
        Rvalue::Use(o)
        | Rvalue::UseFresh(o)
        | Rvalue::Unary(_, o)
        | Rvalue::Cast(_, o)
        | Rvalue::MakeForeachIter(o) => op_mentions(o, l),
        Rvalue::Binary(_, a, b) => op_mentions(a, l) || op_mentions(b, l),
        Rvalue::Field(x, _) => *x == l,
        Rvalue::Index(x, o) => *x == l || op_mentions(o, l),
        Rvalue::Slice(x, b) => *x == l || slice_bounds_mentions(b, l),
        Rvalue::Array(ops) | Rvalue::Set(ops) => ops.iter().any(|o| op_mentions(o, l)),
        Rvalue::New { args, .. } => args.iter().any(|o| op_mentions(o, l)),
        Rvalue::Map(pairs) => pairs
            .iter()
            .any(|(k, v)| op_mentions(k, l) || op_mentions(v, l)),
        Rvalue::Object(fields) => fields.iter().any(|(_, o)| op_mentions(o, l)),
        Rvalue::Interval(iv) => [&iv.start, &iv.end, &iv.step]
            .into_iter()
            .flatten()
            .any(|o| op_mentions(o, l)),
        Rvalue::MakeLambda { captures, .. } => captures.iter().any(|o| op_mentions(o, l)),
        Rvalue::MakeSuper { this, .. } => *this == l,
        Rvalue::FunctionRef(_)
        | Rvalue::GlobalRef(..)
        | Rvalue::BuiltinRef(_)
        | Rvalue::This
        | Rvalue::ClassSelf
        | Rvalue::Super
        | Rvalue::ClassRef(..)
        | Rvalue::Unsupported(_) => false,
    }
}

fn callee_mentions(c: &Callee, l: LocalId) -> bool {
    match c {
        Callee::Method { receiver, .. } => *receiver == l,
        Callee::Indirect(x) => *x == l,
        Callee::SuperConstructor { this, .. } => *this == l,
        Callee::Function(_) | Callee::Builtin(_) => false,
    }
}

fn stmt_mentions(s: &Statement, l: LocalId) -> bool {
    match s {
        Statement::Assign(p, r) => place_mentions(p, l) || rvalue_mentions(r, l),
        Statement::Call { dest, call } => {
            dest.as_ref().is_some_and(|p| place_mentions(p, l))
                || callee_mentions(&call.callee, l)
                || call.args.iter().any(|o| op_mentions(o, l))
        }
        Statement::Charge(_) => false,
        Statement::ApplyPromotion(x) => *x == l,
    }
}

fn term_mentions(t: &Terminator, l: LocalId) -> bool {
    match t {
        Terminator::Return(Some(o)) => op_mentions(o, l),
        Terminator::Branch { cond, .. } => op_mentions(cond, l),
        Terminator::Switch { discriminant, .. } => op_mentions(discriminant, l),
        Terminator::Return(None) | Terminator::Goto(_) | Terminator::Unreachable => false,
    }
}

/// Higher-order builtins that wrap a `@`-by-ref callback argument in a
/// `Value::Cell` and read the reassigned value back into the source element
/// (`builtins/array.rs`). A lambda used *only* as one of these calls' arguments
/// has its by-ref params fully handled by the runtime.
const WRITEBACK_HOFS: &[&str] = &["arrayMap", "arrayFilter", "arrayIter", "arrayPartition"];

/// True if every appearance of local `l` in `f` is either its defining
/// `MakeLambda` assignment or an *argument* to a writeback HOF builtin call.
/// Any other appearance means the lambda value escapes (assigned on, returned,
/// captured, indirectly called, …), so its by-ref params can't be safely
/// runtime-threaded.
fn lambda_local_hof_only(f: &MirFunction, l: LocalId) -> bool {
    for b in &f.blocks {
        for s in &b.statements {
            match s {
                // Defining assignment `l = MakeLambda{…}` — fine, unless the
                // lambda captures itself (not this shape).
                Statement::Assign(Place::Local(d), Rvalue::MakeLambda { captures, .. })
                    if *d == l =>
                {
                    if captures.iter().any(|o| op_mentions(o, l)) {
                        return false;
                    }
                }
                // `l` passed as an argument to a writeback HOF builtin — the
                // only allowed use. It must not also be the dest or the callee.
                Statement::Call { dest, call }
                    if matches!(&call.callee, Callee::Builtin(n) if WRITEBACK_HOFS.contains(&n.as_str())) =>
                {
                    if dest.as_ref().is_some_and(|p| place_mentions(p, l))
                        || callee_mentions(&call.callee, l)
                    {
                        return false;
                    }
                }
                // Anything else mentioning `l` → escape → not HOF-only.
                other => {
                    if stmt_mentions(other, l) {
                        return false;
                    }
                }
            }
        }
        if term_mentions(&b.terminator, l) {
            return false;
        }
    }
    true
}

/// True if lambda body `lambda_fi` is only ever constructed (`MakeLambda`) and
/// immediately handed to a writeback HOF builtin — so the runtime fully handles
/// its `@`-by-ref params. Conservative: if it is never matched as a plain
/// `local = MakeLambda` (or any construction escapes), returns false → skip.
fn lambda_is_hof_only(program: &MirProgram, reachable: &[usize], lambda_fi: usize) -> bool {
    let mut constructed = false;
    for &fi in reachable {
        let f = &program.functions[fi];
        for s in f.blocks.iter().flat_map(|b| &b.statements) {
            if let Statement::Assign(Place::Local(l), Rvalue::MakeLambda { function_idx, .. }) = s
                && *function_idx == lambda_fi
            {
                constructed = true;
                if !lambda_local_hof_only(f, *l) {
                    return false;
                }
            }
        }
    }
    constructed
}

/// Like [`lambda_local_hof_only`] but for a local that holds a *named-function*
/// reference (`l = FunctionRef(def)`): the defining assignment is the
/// `FunctionRef`, and every other appearance must be a writeback-HOF argument.
fn funcref_local_hof_only(f: &MirFunction, l: LocalId, def_id: DefId) -> bool {
    for b in &f.blocks {
        for s in &b.statements {
            match s {
                Statement::Assign(Place::Local(d), Rvalue::FunctionRef(dd))
                    if *d == l && *dd == def_id => {}
                Statement::Call { dest, call }
                    if matches!(&call.callee, Callee::Builtin(n) if WRITEBACK_HOFS.contains(&n.as_str())) =>
                {
                    if dest.as_ref().is_some_and(|p| place_mentions(p, l))
                        || callee_mentions(&call.callee, l)
                    {
                        return false;
                    }
                }
                other => {
                    if stmt_mentions(other, l) {
                        return false;
                    }
                }
            }
        }
        if term_mentions(&b.terminator, l) {
            return false;
        }
    }
    true
}

/// True if named function `def_id` is referenced as a value ONLY to be handed
/// to a writeback HOF builtin (`arrayMap(a, f)` where `function f(@x){…}`) — so
/// the runtime fully handles its `@`-by-ref params, exactly like a HOF-only
/// lambda. Conservative: requires at least one `FunctionRef`, every reference
/// to be a HOF argument, and NO direct (`Callee::Function`) call (a direct call
/// wouldn't be runtime-threaded, so a both-called-and-passed fn isn't purely
/// HOF-only).
fn named_fn_is_hof_only(program: &MirProgram, reachable: &[usize], def_id: DefId) -> bool {
    let mut referenced = false;
    for &fi in reachable {
        let f = &program.functions[fi];
        for s in f.blocks.iter().flat_map(|b| &b.statements) {
            match s {
                Statement::Assign(Place::Local(l), Rvalue::FunctionRef(d)) if *d == def_id => {
                    referenced = true;
                    if !funcref_local_hof_only(f, *l, def_id) {
                        return false;
                    }
                }
                Statement::Call { call, .. }
                    if matches!(&call.callee, Callee::Function(d) if *d == def_id) =>
                {
                    return false;
                }
                _ => {}
            }
        }
    }
    referenced
}

/// True when every reassigned by-ref parameter in the reachable set can be
/// safely cell-threaded: a plain (non-method) function only ever reached by
/// `Callee::Function` with a shareable *local* in each cell-param position, or a
/// lambda that is only ever passed to a writeback HOF builtin (runtime-handled).
/// When this fails for any such param, the program keeps skipping
/// (skip-don't-miscompile).
fn byref_cells_threadable(
    program: &MirProgram,
    reachable: &[usize],
    lambda_set: &HashSet<usize>,
) -> bool {
    let mut needs: HashMap<DefId, HashSet<LocalId>> = HashMap::new();
    for &fi in reachable {
        let f = &program.functions[fi];
        let cells = byref_cell_params(f, program);
        if cells.is_empty() {
            continue;
        }
        // A lambda's by-ref params are threaded by the runtime IFF the lambda is
        // only ever passed to a writeback HOF builtin (`arrayFilter`/…). Such a
        // lambda needs no `Callee::Function` call-site threading.
        if lambda_set.contains(&fi) {
            if !lambda_is_hof_only(program, reachable, fi) {
                return false;
            }
            continue;
        }
        match f.def_id {
            // Only a plain top-level function, dispatched by `Callee::Function`,
            // can be threaded here. A method/constructor (`owning_class`) with a
            // reassigned by-ref param goes through `Callee::Method` — not handled.
            Some(d) if f.owning_class.is_none() => {
                needs.insert(d, cells);
            }
            _ => return false,
        }
    }
    if needs.is_empty() {
        return true;
    }
    for &fi in reachable {
        let f = &program.functions[fi];
        for s in f.blocks.iter().flat_map(|b| &b.statements) {
            match s {
                Statement::Call { call, .. } => {
                    if let Callee::Function(d) = &call.callee
                        && let Some(cells) = needs.get(d)
                    {
                        let g = &program.functions[program
                            .functions
                            .iter()
                            .position(|x| x.def_id == Some(*d))
                            .unwrap()];
                        for (i, gp) in g.params.iter().enumerate() {
                            if cells.contains(gp)
                                && !matches!(call.args.get(i), Some(Operand::Local(_)))
                            {
                                return false;
                            }
                        }
                    }
                }
                // A cell function taken as a *value* (`var g = f`) is invoked
                // indirectly through `dispatch_call_value`, which threads the arg
                // cell against the function's registered `@`-by-ref mask exactly
                // like a direct call — and MIR flattens every indirect-call arg to
                // a local (cell-promoted by `indirect_arg_cell_locals`) or a const
                // (a harmless fresh cell, no caller storage to alias). So a
                // FunctionRef'd cell function is threadable too.
                _ => {}
            }
        }
    }
    true
}

pub fn needs_cell_semantics(
    program: &MirProgram,
    reachable_indices: &[usize],
    lambda_set: &HashSet<usize>,
    version: u8,
) -> bool {
    // A reassigned (or aliased-onward) `@x` by-ref parameter is handled via
    // cross-function `Value::Cell` threading when the program is threadable —
    // in **every** version. v1's deep-clone value semantics are preserved at the
    // cell-write site (the v1 deep-clone with a `@`-by-ref-source alias
    // exemption), so threading the caller's cell for a `@x` rebind is correct in
    // v1 too; an un-threadable program keeps skipping below.
    let byref_threadable = byref_cells_threadable(program, reachable_indices, lambda_set);
    // Lambdas whose by-ref params are *entirely* handled by a writeback HOF
    // builtin at runtime (`arrayFilter`/…). Their by-ref-param reassignments are
    // exempt from the by-ref gates **in every version** — the HOF machinery
    // lives in the shared runtime (`higher_order_array`), which the interpreter
    // already handles at v1, so native matches it. Captured (non-param) shared
    // reassignments stay gated (those are the genuine v1 deep-clone conflict).
    // Includes both lambdas constructed-and-passed to a writeback HOF and
    // *named* functions referenced only as a HOF callback (`arrayMap(a, f)`).
    let hof_only_lambdas: HashSet<usize> = reachable_indices
        .iter()
        .copied()
        .filter(|&fi| {
            let f = &program.functions[fi];
            if !f.params.iter().any(|p| f.locals[p.0 as usize].is_by_ref) {
                return false;
            }
            if lambda_set.contains(&fi) {
                lambda_is_hof_only(program, reachable_indices, fi)
            } else if let Some(d) = f.def_id {
                f.owning_class.is_none() && named_fn_is_hof_only(program, reachable_indices, d)
            } else {
                false
            }
        })
        .collect();
    for &fi in reachable_indices {
        let f = &program.functions[fi];
        let has_byref_param = f
            .params
            .iter()
            .any(|p| f.locals[p.0 as usize].is_by_ref);
        // In v1 composites are passed by *value* (deep clone). A `@x` by-ref
        // param whose binding is *reassigned* (`x = …`), captured by a closure,
        // passed onward to a user fn/method, or returned needs true caller/callee
        // cell aliasing the handle model can't express → skip. BUT an `@x` that
        // is only *read* OR mutated *in place* (`push(x, …)`, `x[i] = …`)
        // propagates correctly through the shared `Rc` once the call site stops
        // deep-cloning the by-ref arg (see `user_call`'s clone-suppression): the
        // callee shares the caller's backing store, so the in-place mutation is
        // visible. Gate only the v1 by-ref params that genuinely need a cell.
        //
        // The clone-suppression lives in `user_call` (DIRECT calls). If the
        // function is taken as a *value* (`FunctionRef`) it can be invoked
        // indirectly through `dispatch_call_value`, which does NOT apply the v1
        // arg-clone at all — so neither the by-ref suppression nor the by-value
        // clone of an *adjacent* function's arg is honoured. Keep gating a v1
        // by-ref function that's referenced as a value.
        // A non-escaping by-ref param of a *lambda* is handled by cell-threading:
        // the lambda is invoked indirectly with the arg passed as a shared cell,
        // so a reassignment/in-place mutation propagates. Only an *escaping* one
        // (captured by a nested closure, returned, passed onward) still needs the
        // cell to survive the call → stays gated.
        if version <= 1 && has_byref_param && !hof_only_lambdas.contains(&fi) {
            let is_lambda = lambda_set.contains(&fi);
            // Leading capture-slot params of this lambda (the `MakeLambda`'s
            // captures). A `return @x` of one of these hands back a shared cell
            // that originates in the enclosing frame — safe, handled by the
            // raw-cell return path + cell-peeling runtime ops.
            let cap_count = if is_lambda {
                lambda_capture_count(program, fi).unwrap_or(0)
            } else {
                0
            };
            if f.params.iter().enumerate().any(|(pi, p)| {
                let id = *p;
                if !f.locals[id.0 as usize].is_by_ref {
                    return false;
                }
                if is_lambda && !byref_lambda_param_escapes(f, id) {
                    return false;
                }
                // A by-ref capture slot that is only read and returned (`return
                // @x`) is compiled by handing back the raw shared cell — not gated.
                if is_lambda && pi < cap_count && byref_capture_only_returned(f, id) {
                    return false;
                }
                // A `@x` by-ref param captured by an (escaping) inner lambda,
                // threaded end-to-end as a shared cell: the caller passes its
                // cell (direct call → `byref_cell_arg`; indirect → runtime
                // `thread_args`), the param reuses it (`leek_make_cell`), and the
                // capturing lambda mutates it. `function f(@a){ return
                // function(){ a += 2 } }; f(x)()` makes `x` observe the change.
                if byref_param_escape_threadable(f, id) {
                    return false;
                }
                // A reassigned / aliased-onward `@x` param is cell-threaded when
                // the whole program is threadable — the caller passes its cell
                // (`byref_cell_arg`), the param reuses it, and the rebind/`x=…`
                // cell-write (v1 deep-clone preserved) propagates back. Same path
                // as v2+, just no longer version-gated.
                if byref_threadable && byref_cell_params(f, program).contains(&id) {
                    return false;
                }
                // A by-ref param referenced as a *value* (`var t = [f, g]`,
                // `arrayMap(a, f)`) is now threaded at the indirect/HOF call
                // boundary: `dispatch_call_value`/`thread_args` pass the arg's
                // shared cell raw for `@x` and v1-clone by-value args, and the
                // composite-mutation shims peel the cell. So only a param that
                // *genuinely needs a cell* (reassigned / captured / returned /
                // passed onward — none of which an indirect call can thread)
                // still gates.
                byref_param_needs_cell_v1(f, id)
            }) {
                return true;
            }
        }
        // (Formerly: a v1 gate on *any* reassignment of a captured shared local
        // from inside a lambda, on the theory that a composite assigned through
        // the cell would alias its source. That is now handled correctly at the
        // assignment site — the cell-write path applies the v1 deep-clone, with
        // a `@`-by-ref-source exemption to preserve genuine alias chains
        // (`t = @a`) — so the blanket gate is no longer needed. Cross-function
        // by-ref *parameter* rebinding is still gated below.)
        // `@x` by-reference *parameter* reassignment needs caller/callee
        // storage sharing the handle model can't express (also gated in
        // `function_sig`). Lambda `is_shared` captures are backed by real
        // `Value::Cell`s (see `cell_locals` + `local_value`), so their
        // reassignment propagates and is no longer gated in v2+.
        // When not threadable, a reassigned by-ref param still forces a skip —
        // except a HOF-only lambda's by-ref params (runtime-handled).
        if !byref_threadable && !hof_only_lambdas.contains(&fi) {
            // A v2+ by-ref param that's a no-op (a pure-local `@x` on a lambda
            // or a method, neither of which propagates in v2+) compiles as a
            // plain by-value param — its `@x` has no observable effect, so it
            // doesn't need a cell and isn't gated. (Empty for a named function,
            // which threads, and for v1.)
            let noop = noop_byref_params(program, fi, version);
            let is_lambda = lambda_set.contains(&fi);
            let shares = |id: LocalId| {
                let l = &f.locals[id.0 as usize];
                // A non-escaping v1 lambda by-ref param reassignment is handled by
                // cell-threading at the indirect call site (the arg is a shared
                // cell), so it isn't gated.
                let cell_threaded =
                    version <= 1 && is_lambda && !byref_lambda_param_escapes(f, id);
                l.is_by_ref && f.params.contains(&id) && !noop.contains(&id) && !cell_threaded
            };
            for b in &f.blocks {
                for s in &b.statements {
                    if let Statement::Assign(Place::Local(id), _) = s
                        && shares(*id)
                    {
                        return true;
                    }
                }
            }
        }
    }
    false
}

/// True if the program assigns to a file-level global named `name` — used
/// to detect a user definition shadowing a builtin (`cos = function(){…}`),
/// which is referenced via `Place::Global`/`Rvalue::GlobalRef` rather than
/// being listed in `MirProgram::globals`.
fn program_writes_global(program: &MirProgram, name: &str) -> bool {
    if program.globals.iter().any(|g| g.name == name) {
        return true;
    }
    program.functions.iter().any(|f| {
        f.blocks.iter().any(|b| {
            b.statements.iter().any(|s| {
                matches!(s, Statement::Assign(Place::Global(_, n), _) if n == name)
            })
        })
    })
}

/// The class a local statically holds an instance of, if any. An untyped
/// `var d = new Dog()` carries the class in `inferred_ty` (its declared
/// `ty` stays `Any`), so both are consulted.
fn instance_class(decl: &LocalDecl) -> Option<&str> {
    if let Type::ClassInstance(n, _) = &decl.ty {
        return Some(n);
    }
    if let Some(Type::ClassInstance(n, _)) = &decl.inferred_ty {
        return Some(n);
    }
    None
}

/// Locals every one of whose assignments is `new C(...)` for the *same*
/// class `C`. Captures the unnamed temp in `new C().m()` (whose declared
/// type is `Any`), so its method calls can dispatch. A local with any
/// other (or conflicting `new`) assignment is dropped (ambiguous).
/// Locals proven to hold an object literal (`{f: v}`) — every assignment is a
/// `Rvalue::Object` or a `Use`/`UseFresh` of an object local (covers `var o =
/// {…}`, lowered via a temp). Lets `o.field(args)` dispatch as an
/// object-field-call (read the field, invoke its value) like the interpreter's
/// `dispatch_method_call` `Object` arm.
fn object_locals(f: &MirFunction) -> HashSet<LocalId> {
    let mut set: HashSet<LocalId> = HashSet::new();
    loop {
        let mut acc: HashMap<LocalId, bool> = HashMap::new();
        for b in &f.blocks {
            for s in &b.statements {
                let Statement::Assign(Place::Local(id), rv) = s else {
                    continue;
                };
                let is_obj = match rv {
                    Rvalue::Object(_) => true,
                    Rvalue::Use(Operand::Local(src)) | Rvalue::UseFresh(Operand::Local(src)) => {
                        set.contains(src)
                    }
                    _ => false,
                };
                acc.entry(*id).and_modify(|cur| *cur &= is_obj).or_insert(is_obj);
            }
        }
        let next: HashSet<LocalId> =
            acc.into_iter().filter_map(|(k, v)| v.then_some(k)).collect();
        if next == set {
            return set;
        }
        set = next;
    }
}

/// For each object-literal local, the field-name → value-operand map from its
/// `Object` rvalue (propagated through `Use` chains). Lets an object-field-call
/// inspect what a field holds — e.g. to skip a field holding a *user* class
/// reference (calling it would construct the class, which the runtime
/// `call_value` can't do).
fn object_field_srcs(f: &MirFunction) -> HashMap<LocalId, HashMap<String, Operand>> {
    let mut map: HashMap<LocalId, HashMap<String, Operand>> = HashMap::new();
    loop {
        let mut changed = false;
        for b in &f.blocks {
            for s in &b.statements {
                let Statement::Assign(Place::Local(id), rv) = s else {
                    continue;
                };
                let fields = match rv {
                    Rvalue::Object(fs) => Some(fs.iter().cloned().collect::<HashMap<_, _>>()),
                    Rvalue::Use(Operand::Local(src)) | Rvalue::UseFresh(Operand::Local(src)) => {
                        map.get(src).cloned()
                    }
                    _ => None,
                };
                if let Some(fields) = fields
                    && map.get(id) != Some(&fields)
                {
                    map.insert(*id, fields);
                    changed = true;
                }
            }
        }
        if !changed {
            return map;
        }
    }
}

fn new_class_locals(f: &MirFunction) -> HashMap<LocalId, String> {
    // Fixpoint: a local is an exact instance of `C` if *every* assignment to
    // it is either a direct `New { class: C }` or a `Use`/`UseFresh` of a
    // local already proven to be an exact `C` (covers `var a = A()`, which
    // lowers to `t = New{A}; a = Use(t)`). A `Use` of an exact-`C` local stays
    // exact `C` — even under v1's deep-clone-on-assign, a clone of a `C`
    // instance is still a `C`. Any other (or conflicting) assignment drops it.
    let mut map: HashMap<LocalId, String> = HashMap::new();
    loop {
        let mut acc: HashMap<LocalId, Option<String>> = HashMap::new();
        for b in &f.blocks {
            for s in &b.statements {
                let Statement::Assign(Place::Local(id), rv) = s else {
                    continue;
                };
                let this = match rv {
                    Rvalue::New { class, .. } => Some(class.clone()),
                    Rvalue::Use(Operand::Local(src)) | Rvalue::UseFresh(Operand::Local(src)) => {
                        map.get(src).cloned()
                    }
                    _ => None,
                };
                acc.entry(*id)
                    .and_modify(|cur| {
                        let keep = matches!((cur.as_deref(), this.as_deref()), (Some(a), Some(b)) if a == b);
                        if !keep {
                            *cur = None;
                        }
                    })
                    .or_insert(this);
            }
        }
        let next: HashMap<LocalId, String> =
            acc.into_iter().filter_map(|(k, v)| v.map(|c| (k, c))).collect();
        if next == map {
            return map;
        }
        map = next;
    }
}

/// Locals whose runtime class is known to be `C` but NOT necessarily *exact*
/// (it may be a subclass): every assignment is a `Use`/`UseFresh`/`Cast(User)`
/// of a value whose class is `C` — a declared `ClassInstance` (`this`, a typed
/// param), an exact `new C()` local, or another such aliased local. Covers
/// `var x = this; x.m()` and `(obj as C).m()`. Used (alongside `new_class_locals`)
/// to resolve a method receiver's class for dispatch; because these aren't
/// exact, dispatch on them is VIRTUAL when the method is overridden. Excludes
/// locals already exact (`new_class_locals`).
fn aliased_class_locals(f: &MirFunction) -> HashMap<LocalId, String> {
    let exact = new_class_locals(f);
    let mut map: HashMap<LocalId, String> = HashMap::new();
    loop {
        let mut acc: HashMap<LocalId, Option<String>> = HashMap::new();
        for b in &f.blocks {
            for s in &b.statements {
                let Statement::Assign(Place::Local(id), rv) = s else {
                    continue;
                };
                let src = match rv {
                    Rvalue::Use(Operand::Local(src))
                    | Rvalue::UseFresh(Operand::Local(src))
                    | Rvalue::Cast(CastKind::User, Operand::Local(src)) => Some(*src),
                    _ => None,
                };
                let this = src.and_then(|src| {
                    instance_class(&f.locals[src.0 as usize])
                        .map(str::to_string)
                        .or_else(|| exact.get(&src).cloned())
                        .or_else(|| map.get(&src).cloned())
                });
                acc.entry(*id)
                    .and_modify(|cur| {
                        let keep = matches!((cur.as_deref(), this.as_deref()), (Some(a), Some(b)) if a == b);
                        if !keep {
                            *cur = None;
                        }
                    })
                    .or_insert(this);
            }
        }
        let next: HashMap<LocalId, String> = acc
            .into_iter()
            // An exact local stays exact; don't shadow it here.
            .filter_map(|(k, v)| v.filter(|_| !exact.contains_key(&k)).map(|c| (k, c)))
            .collect();
        if next == map {
            return map;
        }
        map = next;
    }
}

/// Locals every one of whose assignments is `ClassRef(_, C)` for the *same*
/// class `C` — covering `var c = C` and the inline `C.staticMethod()` temp.
/// A local with any other (or conflicting) assignment is dropped.
fn classref_locals(f: &MirFunction) -> HashMap<LocalId, String> {
    // Fixpoint: a local is `ClassRef(C)` if every assignment to it is either
    // a direct `ClassRef(_, C)` or a `Use`/`UseFresh` of a local already
    // proven to hold `ClassRef(C)` (covers `Class c = A` lowered via a temp).
    let mut map: HashMap<LocalId, String> = HashMap::new();
    loop {
        let mut acc: HashMap<LocalId, Option<String>> = HashMap::new();
        for b in &f.blocks {
            for s in &b.statements {
                let Statement::Assign(Place::Local(id), rv) = s else {
                    continue;
                };
                let this = match rv {
                    Rvalue::ClassRef(_, name) => Some(name.clone()),
                    Rvalue::Use(Operand::Local(src)) | Rvalue::UseFresh(Operand::Local(src)) => {
                        map.get(src).cloned()
                    }
                    _ => None,
                };
                acc.entry(*id)
                    .and_modify(|cur| {
                        let keep = matches!((cur.as_deref(), this.as_deref()), (Some(a), Some(b)) if a == b);
                        if !keep {
                            *cur = None;
                        }
                    })
                    .or_insert(this);
            }
        }
        let next: HashMap<LocalId, String> =
            acc.into_iter().filter_map(|(k, v)| v.map(|c| (k, c))).collect();
        if next == map {
            return map;
        }
        map = next;
    }
}

/// Locals holding a `MakeSuper { this, parent_class }` value — `super.m()` /
/// `super.field`. Maps the super-local to `(this instance local, parent class
/// name)` so a method call on it dispatches statically against the parent
/// (super is never virtual) with the real `this` as receiver.
fn super_locals(f: &MirFunction) -> HashMap<LocalId, (LocalId, String)> {
    let mut map = HashMap::new();
    for b in &f.blocks {
        for s in &b.statements {
            if let Statement::Assign(
                Place::Local(id),
                Rvalue::MakeSuper { this, parent_class },
            ) = s
            {
                map.insert(*id, (*this, parent_class.clone()));
            }
        }
    }
    map
}

/// Resolve a *static* method by name (arity-preferring) walking `class_name`
/// and its parents, mirroring the interpreter's `find_static_method`.
/// Returns the method's `program.functions` index.
fn resolve_static_method(
    program: &MirProgram,
    class_name: &str,
    method: &str,
    argc: usize,
) -> Option<usize> {
    let mut cursor;
    let mut seen: HashSet<String> = HashSet::new();
    // Prefer an exact-arity overload across the whole chain, then any.
    for want_arity in [true, false] {
        cursor = Some(class_name.to_string());
        seen.clear();
        while let Some(name) = cursor {
            if !seen.insert(name.clone()) {
                break;
            }
            let c = program.class_by_name(&name)?;
            if let Some(m) = c.methods.iter().find(|m| {
                m.is_static && m.name == method && (!want_arity || m.user_arity == argc)
            }) {
                return Some(m.function_idx);
            }
            cursor = c.parent.clone();
        }
    }
    None
}

/// Build the value of a class reflective member (`C.fields`, `C.methods`,
/// `C.static_fields`, `C.static_methods`, `C.constructors`) — all known at
/// compile time. Walks the class chain child→parent like the interpreter.
/// Returns `None` for non-reflective members.
fn class_reflect(
    program: &MirProgram,
    class_name: &str,
    member: &str,
) -> Option<leek_runtime::Value> {
    use leek_runtime::Value as RtValue;
    use std::cell::RefCell;
    use std::rc::Rc;
    let str_arr = |names: Vec<String>| {
        RtValue::Array(Rc::new(RefCell::new(
            names.into_iter().map(|n| RtValue::String(Rc::new(n))).collect(),
        )))
    };
    // Walk the class + ancestors, collecting names selected by `pick`.
    let walk = |pick: &dyn Fn(&leek_mir::ir::MirClass) -> Vec<String>| -> Vec<String> {
        let mut out = Vec::new();
        let mut seen: HashSet<String> = HashSet::new();
        let mut cursor = Some(class_name.to_string());
        while let Some(cn) = cursor {
            if !seen.insert(cn.clone()) {
                break;
            }
            let Some(c) = program.class_by_name(&cn) else {
                break;
            };
            out.extend(pick(c));
            cursor = c.parent.clone();
        }
        out
    };
    match member {
        "fields" => Some(str_arr(walk(&|c| {
            c.instance_fields.iter().map(|f| f.name.clone()).collect()
        }))),
        "static_fields" | "staticFields" => Some(str_arr(walk(&|c| {
            c.static_fields.iter().map(|f| f.name.clone()).collect()
        }))),
        "methods" => Some(str_arr(walk(&|c| {
            c.methods.iter().filter(|m| !m.is_static).map(|m| m.name.clone()).collect()
        }))),
        "static_methods" | "staticMethods" => Some(str_arr(walk(&|c| {
            c.methods.iter().filter(|m| m.is_static).map(|m| m.name.clone()).collect()
        }))),
        "constructors" => {
            let n = program.class_by_name(class_name).map_or(0, |c| c.constructors.len());
            Some(RtValue::Array(Rc::new(RefCell::new(vec![RtValue::Int(0); n]))))
        }
        _ => None,
    }
}

/// Resolve a *static* field by name, walking `class_name` and its parents.
/// Returns the declaring class's `DefId` (the storage key) and the field.
fn resolve_static_field<'a>(
    program: &'a MirProgram,
    class_name: &str,
    field: &str,
) -> Option<(DefId, &'a leek_mir::ir::MirField)> {
    let mut cursor = Some(class_name.to_string());
    let mut seen: HashSet<String> = HashSet::new();
    while let Some(name) = cursor {
        if !seen.insert(name.clone()) {
            break;
        }
        let c = program.class_by_name(&name)?;
        if let Some(f) = c.static_fields.iter().find(|f| f.name == field) {
            return Some((c.def_id, f));
        }
        cursor = c.parent.clone();
    }
    None
}

/// Names of static fields read/written on a class reference in `f`, as
/// `(declaring-class DefId, field name, init function index or None)`.
/// Used to make the init functions reachable and to seed the runtime
/// static-field-init table.
fn static_field_accesses(
    program: &MirProgram,
    f: &MirFunction,
) -> Vec<(DefId, String, Option<usize>)> {
    let classrefs = classref_locals(f);
    let mut out = Vec::new();
    let consider = |base: &LocalId, name: &str, out: &mut Vec<_>| {
        if let Some(cls) = classrefs.get(base)
            && let Some((owner, field)) = resolve_static_field(program, cls, name)
        {
            out.push((owner, name.to_string(), field.init_fn));
        }
    };
    for b in &f.blocks {
        for s in &b.statements {
            match s {
                Statement::Assign(Place::Field(base, name), _) => consider(base, name, &mut out),
                Statement::Assign(Place::Index(base, Operand::Const(Const::String(name))), _) => {
                    consider(base, name, &mut out);
                }
                Statement::Assign(_, Rvalue::Field(base, name)) => consider(base, name, &mut out),
                Statement::Assign(_, Rvalue::Index(base, Operand::Const(Const::String(name)))) => {
                    consider(base, name, &mut out);
                }
                // `A.a()` where `a` is a *static field* holding a callable
                // (`static a = -> 12`): the call reads the field, so its
                // initializer (and the lambda it builds) must be reachable.
                // `consider` only adds it when `a` actually resolves to a
                // static field (a static *method* `a` resolves elsewhere).
                Statement::Call { call, .. } => {
                    if let Callee::Method { receiver, method } = &call.callee {
                        consider(receiver, method, &mut out);
                    }
                }
                _ => {}
            }
        }
    }
    out
}

/// Named functions referenced as values (`var f = foo`) in the reachable
/// program: `DefId` raw → `program.functions` index. The functions are
/// uniform-compiled so a `Function::User` value can be invoked indirectly.
pub fn function_ref_info(
    program: &MirProgram,
    reachable_indices: &[usize],
) -> HashMap<u32, usize> {
    let mut table = HashMap::new();
    for &fi in reachable_indices {
        for b in &program.functions[fi].blocks {
            for s in &b.statements {
                if let Statement::Assign(_, Rvalue::FunctionRef(d)) = s
                    && let Some(idx) = program.functions.iter().position(|g| g.def_id == Some(*d))
                {
                    table.insert(d.0, idx);
                }
            }
        }
    }
    table
}

/// Resolve a *static* method by name only (any arity), walking `class_name`
/// and its parents — for `C.staticMethod` read as a *value*. Returns the
/// method's `program.functions` index (first match, most-derived first).
fn resolve_static_method_value(
    program: &MirProgram,
    class_name: &str,
    method: &str,
) -> Option<usize> {
    let mut seen: HashSet<String> = HashSet::new();
    let mut cursor = Some(class_name.to_string());
    while let Some(name) = cursor {
        if !seen.insert(name.clone()) {
            break;
        }
        let c = program.class_by_name(&name)?;
        if let Some(m) = c.methods.iter().find(|m| m.is_static && m.name == method) {
            return Some(m.function_idx);
        }
        cursor = c.parent.clone();
    }
    None
}

/// Resolve an *instance* method by name only (any arity), walking
/// `class_name` and its parents — for `C.instanceMethod` read as an *unbound*
/// value (`var f = A.m; f(receiver, …)`). Returns the method's
/// `program.functions` index (first match, most-derived first).
fn resolve_instance_method_value(
    program: &MirProgram,
    class_name: &str,
    method: &str,
) -> Option<usize> {
    let mut seen: HashSet<String> = HashSet::new();
    let mut cursor = Some(class_name.to_string());
    while let Some(name) = cursor {
        if !seen.insert(name.clone()) {
            break;
        }
        let c = program.class_by_name(&name)?;
        if let Some(m) = c.methods.iter().find(|m| !m.is_static && m.name == method) {
            return Some(m.function_idx);
        }
        cursor = c.parent.clone();
    }
    None
}

/// Methods referenced as *values* in `f` — a `Field`/string-`Index` read of a
/// class-ref member that resolves to a *method* (and not a static field,
/// reflective member, or `name`). Covers both static methods (`C.staticM`) and
/// instance methods read unbound (`C.m`, invoked as `f(receiver, …)`). Yields
/// `(method DefId, idx)`. Used for reachability (`fn_edges`), the
/// `Function::User` dispatch table (`static_method_value_info`), and the
/// `field()` boxing.
fn static_method_value_refs(program: &MirProgram, f: &MirFunction) -> Vec<(DefId, usize)> {
    let classrefs = classref_locals(f);
    let mut out = Vec::new();
    let consider = |base: &LocalId, name: &str, out: &mut Vec<(DefId, usize)>| {
        if let Some(cls) = classrefs.get(base)
            && name != "name"
            && resolve_static_field(program, cls, name).is_none()
            && class_reflect(program, cls, name).is_none()
            && let Some(idx) = resolve_static_method_value(program, cls, name)
                .or_else(|| resolve_instance_method_value(program, cls, name))
            && let Some(def) = program.functions[idx].def_id
        {
            out.push((def, idx));
        }
    };
    for b in &f.blocks {
        for s in &b.statements {
            match s {
                Statement::Assign(_, Rvalue::Field(base, name)) => consider(base, name, &mut out),
                Statement::Assign(_, Rvalue::Index(base, Operand::Const(Const::String(name)))) => {
                    consider(base, name, &mut out);
                }
                _ => {}
            }
        }
    }
    out
}

/// `DefId` raw → `program.functions` index for every static method referenced
/// as a value across the reachable program. Merged into the `Function::User`
/// dispatch table so `var f = C.staticMethod; f(...)` works.
pub fn static_method_value_info(
    program: &MirProgram,
    reachable_indices: &[usize],
) -> HashMap<u32, usize> {
    let mut table = HashMap::new();
    for &fi in reachable_indices {
        for (def, idx) in static_method_value_refs(program, &program.functions[fi]) {
            table.insert(def.0, idx);
        }
    }
    table
}

/// The runtime static-field-init table for the reachable program:
/// `(class def_id raw, field name)` → init function index (only fields with
/// an initialiser). Its values are the init functions to uniform-compile.
pub fn static_field_info(
    program: &MirProgram,
    reachable_indices: &[usize],
) -> HashMap<(u32, String), usize> {
    let mut table = HashMap::new();
    for &fi in reachable_indices {
        for (owner, name, init) in static_field_accesses(program, &program.functions[fi]) {
            if let Some(idx) = init {
                table.insert((owner.0, name), idx);
            }
        }
    }
    table
}

/// The class a method-call receiver statically holds an instance of —
/// from the local's declared/inferred type, or from a `new C(...)`
/// assignment (covering inline `new C().m()` temps).
fn receiver_class<'a>(
    locals: &'a [LocalDecl],
    new_classes: &'a HashMap<LocalId, String>,
    aliased: &'a HashMap<LocalId, String>,
    id: LocalId,
) -> Option<&'a str> {
    if let Some(n) = instance_class(&locals[id.0 as usize]) {
        return Some(n);
    }
    new_classes
        .get(&id)
        .or_else(|| aliased.get(&id))
        .map(String::as_str)
}

/// True if `c` (or an ancestor) extends something that isn't another user
/// class — i.e. a builtin like `Array`/`Map`. The interpreter collapses
/// `class A extends Array {}` to a real Array; the native backend can't
/// model that as a plain instance, so such classes skip.
/// The builtin class name a user class (transitively) extends — `Array` for
/// `class A extends Array {}` — or `None` if its whole parent chain is user
/// classes. The builtin name is the first ancestor `parent` that doesn't
/// resolve to a user class.
fn builtin_ancestor(program: &MirProgram, c: &leek_mir::ir::MirClass) -> Option<String> {
    let mut cur = Some(c);
    let mut seen: HashSet<DefId> = HashSet::new();
    while let Some(cc) = cur {
        if !seen.insert(cc.def_id) {
            break;
        }
        if let Some(p) = &cc.parent
            && program.class_by_name(p).is_none()
        {
            return Some(p.clone());
        }
        cur = cc.parent_def.and_then(|d| program.class(d));
    }
    None
}

fn class_extends_builtin(program: &MirProgram, c: &leek_mir::ir::MirClass) -> bool {
    let mut cur = Some(c);
    let mut seen: HashSet<DefId> = HashSet::new();
    while let Some(cc) = cur {
        if !seen.insert(cc.def_id) {
            break;
        }
        if let Some(p) = &cc.parent
            && program.class_by_name(p).is_none()
        {
            return true;
        }
        cur = cc.parent_def.and_then(|d| program.class(d));
    }
    false
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
type DbgFrame = Option<(i64, Option<cranelift::codegen::ir::StackSlot>, Vec<(usize, u8)>)>;

/// Emit a `leek_dbg_safepoint(offset, desc, values)` call, spilling the
/// frame's named locals into the value slot first. Used before each
/// statement and before each `return` terminator.
fn emit_dbg_safepoint(tx: &mut Tx<'_, '_>, frame: &DbgFrame, offset: u32) -> Result<(), NativeError> {
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
                tx.b.ins().stack_store(v, *slot, i32::try_from(idx * 8).unwrap_or(0));
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
        if let Some(fi) = program.functions.iter().position(|g| std::ptr::eq(g, mir_fn)) {
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
    let argv = if uniform_abi { Some(entry_params[0]) } else { None };
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
    let dbg_frame: DbgFrame =
        if debug_hooks {
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
                descs.push(crate::debug::VarDesc { name: name.clone(), kind });
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
            let cond = builder.ins().icmp(IntCC::SignedLessThanOrEqual, argc, i_val);
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
    *FLAG.get_or_init(|| {
        std::env::var_os("LEEK_NATIVE_NO_COALESCE").is_some_and(|v| v == "1")
    })
}

impl Tx<'_, '_> {
    /// Accumulate `n` operations into the current block's pending charge, at
    /// the same MIR sites the interpreter's `charge_ops` fires. The charge is
    /// *coalesced*: nothing is emitted here — [`flush_charge`](Self::flush_charge)
    /// emits a single `leek_charge_ops(pending)` at the block boundary. Since a
    /// MIR block is straight-line, the summed-then-charged total equals the
    /// per-op total for any completing program, so the two backends still report
    /// identical op counts (`.ops(N)` corpus cases); a budget-hitting program
    /// errors under both, just possibly one block-charge later.
    // Returns `Result` for consistency with the fallible emit helpers it sits
    // beside (and is called with `?`); accumulation is infallible today.
    #[allow(clippy::unnecessary_wraps)]
    fn charge(&mut self, n: u64) -> Result<(), NativeError> {
        self.pending_charge = self.pending_charge.saturating_add(n);
        // A/B escape hatch: with `LEEK_NATIVE_NO_COALESCE=1` set, flush every
        // charge immediately, reproducing the old per-op charging so the two
        // modes can be benchmarked back-to-back on the same machine.
        if no_coalesce() {
            self.flush_charge()?;
        }
        Ok(())
    }

    /// Emit the coalesced `leek_charge_ops(pending)` call for the current block
    /// and reset the accumulator. Called at every terminator (block boundary)
    /// and, on a back-edge `Branch`, *before* the budget check so the check
    /// observes the block's ops. A zero pending charge emits nothing.
    #[allow(clippy::unnecessary_wraps)]
    fn flush_charge(&mut self) -> Result<(), NativeError> {
        let n = self.pending_charge;
        if n == 0 {
            return Ok(());
        }
        self.pending_charge = 0;
        // Text-dump (CLIF inspection) mode declares no imports — there's no op
        // counter to charge, so skip silently rather than fail the dump.
        let Ok(f) = self.imports.rt("leek_charge_ops") else {
            return Ok(());
        };
        let nv = self.b.ins().iconst(types::I64, n as i64);
        self.b.ins().call(f, &[nv]);
        Ok(())
    }

    /// Emit an op-budget back-edge check before a branch: if the budget is
    /// exhausted, jump to a trap block (which returns the function's default)
    /// instead of continuing — so an unbounded loop stops promptly. The runtime
    /// already recorded `TOO_MUCH_OPERATIONS`, which `run()` surfaces. Only
    /// emitted when a finite budget is in force (see
    /// [`crate::runtime::enforce_budget`]).
    ///
    /// The order matters for Cranelift: we emit the `brif` *first* (which fills
    /// the current block) and only then `switch_to_block` to fill the trap and
    /// continuation blocks. Filling the trap before terminating the current
    /// block — as a shared lazily-built trap would require — trips Cranelift's
    /// "you have to fill your block before switching" invariant, since the
    /// current loop block already holds the back-edge's charge instruction.
    fn emit_budget_check(&mut self) -> Result<(), NativeError> {
        if !crate::runtime::enforce_budget() {
            return Ok(());
        }
        let f = self.imports.rt("leek_op_budget_exceeded")?;
        let inst = self.b.ins().call(f, &[]);
        let over = self.b.inst_results(inst)[0];
        let trap = self.b.create_block();
        let cont = self.b.create_block();
        // Terminate the current block — both switches below now leave a filled
        // block, satisfying Cranelift's invariant.
        self.b.ins().brif(over, trap, &[], cont, &[]);

        // Trap path: the budget is spent; return the function's default value
        // (the runtime has already recorded `TOO_MUCH_OPERATIONS`).
        self.b.switch_to_block(trap);
        let z = match self.ret_ty {
            ValTy::Ref => {
                let null = self.imports.rt("leek_box_null")?;
                let inst = self.b.ins().call(null, &[]);
                self.b.inst_results(inst)[0]
            }
            ValTy::Real => self.b.ins().f64const(0.0),
            _ => self.b.ins().iconst(types::I64, 0),
        };
        self.b.ins().return_(&[z]);

        // Continue emitting the branch into the budget-OK fall-through block.
        self.b.switch_to_block(cont);
        Ok(())
    }

    /// True when an rvalue reads a `@`-by-ref source local directly
    /// (`t = @a` lowers to `Use`/`UseFresh` of a local whose `is_by_ref` is
    /// set). Assigning such a value into a cell must *alias* (no v1 clone) so
    /// the by-ref alias chain is preserved — `function(@a){ t = @a }` makes
    /// `t` alias the caller's argument.
    fn rvalue_reads_byref_source(&self, rv: &Rvalue) -> bool {
        match rv {
            Rvalue::Use(Operand::Local(l)) | Rvalue::UseFresh(Operand::Local(l)) => {
                self.mir_locals[l.0 as usize].is_by_ref
            }
            _ => false,
        }
    }

    fn stmt(&mut self, s: &Statement) -> Result<(), NativeError> {
        match s {
            Statement::Assign(Place::Local(id), rv) => {
                // A cell local's var holds the (stable) shared cell handle;
                // a write stores the new value *into* the cell so closures
                // sharing the cell see the reassignment — the var itself is
                // never rebound.
                if self.cell_locals.contains(id) {
                    let (v, ty) = self.rvalue(rv)?;
                    let mut v = self.coerce(v, ty, ValTy::Ref)?;
                    // v1 value semantics also apply to a captured cell local: a
                    // composite reassigned *into* the cell is copied (deep-clone)
                    // so it doesn't alias its source — UNLESS the value is freshly
                    // produced (`UseFresh`), the cell is itself a `@`-by-ref alias,
                    // or the RHS reads a `@`-by-ref source local (the alias chain
                    // continues, `t = @a`). Mirrors the plain-local path below.
                    if self.lang.version <= 1
                        && !matches!(rv, Rvalue::UseFresh(_))
                        && self.mir_locals[id.0 as usize].kind == LocalKind::UserLocal
                        && !self.mir_locals[id.0 as usize].is_by_ref
                        && !self.rvalue_reads_byref_source(rv)
                    {
                        let f = self.imports.rt("leek_clone_v1")?;
                        let inst = self.b.ins().call(f, &[v]);
                        v = self.b.inst_results(inst)[0];
                    }
                    let cell = self.b.use_var(self.vars[id.0 as usize]);
                    let f = self.imports.rt("leek_cell_set")?;
                    self.b.ins().call(f, &[cell, v]);
                    return Ok(());
                }
                let target = self.var_tys[id.0 as usize];
                // A direct array read into a scalar-typed slot reads straight
                // into an unboxed `i64`/`f64` (`integer x = arr[i]`), skipping
                // the box-then-`to_long`/`to_real`-unbox round-trip. Gated on a
                // statically-integer index and a non-class-ref base (the only
                // base kind `index` intercepts before the general read path).
                let (v, ty) = match rv {
                    Rvalue::Index(base, idx)
                        if matches!(target, ValTy::Int | ValTy::Real)
                            && self.operand_int_kind(idx)
                            && !self.classref_locals.contains_key(base) =>
                    {
                        (self.index_unboxed(*base, idx, target)?, target)
                    }
                    // A direct field read into a scalar-typed slot
                    // (`integer x = obj.f`) reads straight into an unboxed
                    // scalar. Gated to the generic member path `field` would take
                    // (a `Ref` base that isn't a class-ref; not the `super`/
                    // `class` meta-properties), so the special cases still route
                    // through `field`.
                    Rvalue::Field(base, name)
                        if matches!(target, ValTy::Int | ValTy::Real)
                            && name != "super"
                            && name != "class"
                            && self.var_tys[base.0 as usize] == ValTy::Ref
                            && !self.classref_locals.contains_key(base) =>
                    {
                        (self.field_unboxed(*base, name, target)?, target)
                    }
                    _ => self.rvalue(rv)?,
                };
                // Coerce to the local's declared cranelift kind (e.g.
                // `real x = 42` stores 42 as 42.0).
                let mut v = self.coerce(v, ty, target)?;
                // v1 value semantics: assigning a composite to a *user*
                // local copies it (deep-clone), unless the value is freshly
                // produced (`UseFresh` — e.g. a builtin result) or the slot
                // is a `@`-by-ref alias.
                if self.lang.version <= 1
                    && target == ValTy::Ref
                    && !matches!(rv, Rvalue::UseFresh(_))
                    && self.mir_locals[id.0 as usize].kind == LocalKind::UserLocal
                    && !self.mir_locals[id.0 as usize].is_by_ref
                {
                    let f = self.imports.rt("leek_clone_v1")?;
                    let inst = self.b.ins().call(f, &[v]);
                    v = self.b.inst_results(inst)[0];
                }
                self.b.def_var(self.vars[id.0 as usize], v);
                Ok(())
            }
            Statement::Assign(Place::Index(base, idx), rv) => self.set_index(*base, idx, rv),
            Statement::Assign(Place::Field(base, name), rv) => self.set_field(*base, name, rv),
            Statement::Assign(Place::Global(_, name), rv) => self.set_global(name, rv),
            // Self-recursive `var f = function(){ f(...) }`: the lowering patches
            // the lambda's own capture slot after the init assign. In the cell
            // model this is a no-op — the binding `f` is `is_shared` (captured
            // by its own lambda), so it's a `Value::Cell` the lambda captured by
            // raw handle; the preceding `cell_set` of the lambda value into that
            // shared cell already makes recursive calls see the right binding.
            Statement::Assign(Place::LambdaCapture { lambda, .. }, _)
                if self.cell_locals.contains(lambda) =>
            {
                Ok(())
            }
            Statement::Assign(p, _) => Err(unsupported(format!("assign to {p:?}"))),
            // Static op charge inserted by the `leek-charge` HIR pass (statement
            // costs — assignments, returns, … — that aren't charged dynamically
            // by the binary/branch/builtin sites). The interpreter executes
            // these; native must too, or its op count comes up short.
            Statement::Charge(n) => self.charge(*n),
            // v1-v3 LegacyArray promotion: a mutating builtin (`push` in v1;
            // `assocSort`/`keySort`/`assocReverse`/`removeElement` in v1-v3)
            // may morph a dense array into a sparse map and stash the new
            // value; write it back to the local. A no-op in v4 (nothing is
            // stashed) and whenever the runtime stash is empty.
            Statement::ApplyPromotion(local) => {
                if self.lang.version <= 3 && self.var_tys[local.0 as usize] == ValTy::Ref {
                    let (cur, _) = self.local_value(*local)?;
                    let f = self.imports.rt("leek_apply_promotion")?;
                    let inst = self.b.ins().call(f, &[cur]);
                    let v = self.b.inst_results(inst)[0];
                    if self.cell_locals.contains(local) {
                        let cell = self.b.use_var(self.vars[local.0 as usize]);
                        let set = self.imports.rt("leek_cell_set")?;
                        self.b.ins().call(set, &[cell, v]);
                    } else {
                        self.b.def_var(self.vars[local.0 as usize], v);
                    }
                }
                Ok(())
            }
            Statement::Call { dest, call } => self.call(dest.as_ref(), call),
        }
    }

    /// `base[idx] = value` (v4 semantics: in-range writes land, otherwise
    /// no-op). `base` is an array handle.
    fn set_index(
        &mut self,
        base: LocalId,
        idx: &Operand,
        rv: &Rvalue,
    ) -> Result<(), NativeError> {
        // `C['staticField'] = v` — write to per-class static storage.
        if let Some(cls) = self.classref_locals.get(&base).cloned() {
            if let Operand::Const(Const::String(name)) = idx
                && let Some((owner, field)) = resolve_static_field(self.program, &cls, name)
            {
                if field.is_final || !self.method_visible(owner, field.visibility) {
                    return Ok(());
                }
                let (v, vt) = self.rvalue(rv)?;
                let v = self.coerce(v, vt, ValTy::Ref)?;
                return self.static_field_set(owner, name, v);
            }
            return Err(unsupported("class reference index assignment"));
        }
        if self.var_tys[base.0 as usize] != ValTy::Ref {
            return Err(unsupported("index assign to non-composite"));
        }
        // Honor `final` fields when the base is a known class instance: a
        // constant field name no-ops if final (and the write is external);
        // a dynamic key that could hit a final field can't be checked
        // statically, so bail.
        if let Some(cls) = receiver_class(self.mir_locals, self.new_classes, self.aliased_classes, base)
            && let Some(c) = self.program.class_by_name(cls)
        {
            match idx {
                Operand::Const(Const::String(s)) => {
                    if self.is_final_field(base, s) {
                        return Ok(());
                    }
                }
                _ if self.owning_class != Some(c.def_id)
                    && c.field_layout.iter().any(|fs| fs.is_final) =>
                {
                    return Err(unsupported("dynamic index-write on instance with final field"));
                }
                _ => {}
            }
        }
        let set = self.imports.rt("leek_value_set_index")?;
        let (arr, _) = self.local_value(base)?;
        let (i, it) = self.operand(idx)?;
        // The index is a boxed value (so map keys of any kind work).
        let idx_h = self.coerce(i, it, ValTy::Ref)?;
        let (mut v, mut vt) = self.rvalue(rv)?;
        // A typed numeric array coerces the written element to its element
        // kind (`Array<real>`'s `a[0] = 5` stores `5.0`); a constant-keyed
        // write to a typed instance field coerces to the field type.
        if vt != ValTy::Ref {
            if let Some(et) = self.elem_tys[base.0 as usize] {
                v = self.coerce(v, vt, et)?;
                vt = et;
            } else if let Some(mvt) = map_value_valty(&self.mir_locals[base.0 as usize].ty) {
                // A typed map coerces the written value to its value type
                // (`Map<integer, real>`'s `m[k] = 5` stores `5.0`).
                v = self.coerce(v, vt, mvt)?;
                vt = mvt;
            } else if let Operand::Const(Const::String(s)) = idx
                && let Some(ft) = self.field_coerce_ty(base, s)
            {
                v = self.coerce(v, vt, ft)?;
                vt = ft;
            }
        }
        let elem = self.coerce(v, vt, ValTy::Ref)?;
        let ver = self.b.ins().iconst(types::I64, i64::from(self.lang.version));
        self.b.ins().call(set, &[arr, idx_h, elem, ver]);
        Ok(())
    }

    /// `base.field = value` — a string-keyed index-set (`set_index` routes
    /// objects/instances to `set_field`).
    fn set_field(&mut self, base: LocalId, name: &str, rv: &Rvalue) -> Result<(), NativeError> {
        // `C.staticField = …` — write to per-class static storage.
        if let Some(cls) = self.classref_locals.get(&base).cloned() {
            let Some((owner, field)) = resolve_static_field(self.program, &cls, name) else {
                return Err(unsupported("class reference member assignment"));
            };
            // A `final` static field, or one inaccessible from here, ignores
            // the write (matching the interpreter).
            if field.is_final || !self.method_visible(owner, field.visibility) {
                return Ok(());
            }
            let (v, vt) = self.rvalue(rv)?;
            let v = self.coerce(v, vt, ValTy::Ref)?;
            return self.static_field_set(owner, name, v);
        }
        if self.var_tys[base.0 as usize] != ValTy::Ref {
            return Err(unsupported("field assign to non-object"));
        }
        // A `final` field ignores writes (its initializer already set it).
        if self.is_final_field(base, name) {
            return Ok(());
        }
        let set = self.imports.rt("leek_field_set")?;
        let (base_h, _) = self.local_value(base)?;
        let (ptr, lenv) = self.const_str_bytes(name);
        let (mut v, mut vt) = self.rvalue(rv)?;
        // Coerce a scalar write to the declared field type (`real? x = 5`
        // stores `5.0`), matching the interpreter's `coerce_to_type`.
        if vt != ValTy::Ref
            && let Some(ft) = self.field_coerce_ty(base, name)
        {
            v = self.coerce(v, vt, ft)?;
            vt = ft;
        }
        let val = self.coerce(v, vt, ValTy::Ref)?;
        let ver = self.b.ins().iconst(types::I64, i64::from(self.lang.version));
        // The field name is passed unboxed (`ptr`,`len`) — `leek_field_set`
        // writes an instance/object field via `set_field(&str, …)` with no
        // `Value::String` key allocation.
        self.b.ins().call(set, &[base_h, ptr, lenv, val, ver]);
        Ok(())
    }

    /// `global name = value`. A typed global (`global real x`) coerces the
    /// written value to its declared kind.
    fn set_global(&mut self, name: &str, rv: &Rvalue) -> Result<(), NativeError> {
        let set = self.imports.rt("leek_global_set")?;
        let key = self.const_string(name)?;
        let (mut v, mut vt) = self.rvalue(rv)?;
        if let Some(&gt) = self.global_tys.get(name)
            && vt != ValTy::Ref
        {
            v = self.coerce(v, vt, gt)?;
            vt = gt;
        }
        let val = self.coerce(v, vt, ValTy::Ref)?;
        self.b.ins().call(set, &[key, val]);
        Ok(())
    }

    /// Lower a `Statement::Call`. Supports scalar math builtins: the
    /// shared runtime functions (`sqrt`/`floor`/`pow`/`atan2`/…, called
    /// via the resolved import) plus the type-polymorphic ones handled
    /// inline (`abs`/`signum`/`min`/`max`). Everything else skips.
    fn call(
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
                    let ver = self.b.ins().iconst(types::I64, i64::from(self.lang.version));
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
                                return Err(unsupported("super constructor: omitted param without default"));
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
                        cl_args.push(self.b.ins().iconst(types::I64, (call.args.len() + 1) as i64));
                        self.b.ins().call(fref, &cl_args);
                    } else {
                        let mut defaults: Vec<Const> = Vec::new();
                        for i in call.args.len()..user_params {
                            match const_default(ctor_fn, ctor_fn.params[i + 1]) {
                                Some(c) => defaults.push(c),
                                None => {
                                    return Err(unsupported("super constructor non-constant default arg"));
                                }
                            }
                        }
                        let mut cl_args = Vec::with_capacity(sig.params.len());
                        cl_args.push(self.coerce(this_v, this_t, sig.params[0])?);
                        for (i, &pty) in sig.params[1..].iter().enumerate() {
                            let (v, t) = if i < call.args.len() {
                                self.operand(&call.args[i])?
                            } else {
                                self.operand(&Operand::Const(defaults[i - call.args.len()].clone()))?
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
                let ver = self.b.ins().iconst(types::I64, i64::from(self.lang.version));
                let inst = self.b.ins().call(f, &[callee, ptr, nc, ver]);
                (self.b.inst_results(inst)[0], ValTy::Ref)
            }
        };
        if let Some(Place::Local(id)) = dest {
            let target = self.var_tys[id.0 as usize];
            let v = self.coerce(res, res_ty, target)?;
            self.b.def_var(self.vars[id.0 as usize], v);
        }
        Ok(())
    }

    /// Dispatch a builtin by name on already-lowered operands (shared by
    /// the free-function and method-call forms).
    fn dispatch_builtin(&mut self, name: &str, args: &[Operand]) -> Result<(Value, ValTy), NativeError> {
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
    fn generic_builtin(&mut self, name: &str, args: &[Operand]) -> Result<(Value, ValTy), NativeError> {
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
        let ver = self.b.ins().iconst(types::I64, i64::from(self.lang.version));
        cl_args.push(ver);
        let inst = self.b.ins().call(f, &cl_args);
        Ok((self.b.inst_results(inst)[0], ValTy::Ref))
    }

    /// Resolve a single parameter's default to a `DefaultArg`, if it's
    /// self-contained (a constant or a constant composite literal). `None`
    /// when the default references earlier params / calls a function / has no
    /// default — the caller then bails (skips or falls back to builtin sugar).
    fn param_default(&self, callee: &MirFunction, param: LocalId) -> Option<DefaultArg> {
        if let Some(c) = const_default(callee, param) {
            Some(DefaultArg::Const(c))
        } else {
            const_eval_default(callee, param, self.lang.version).map(DefaultArg::Composite)
        }
    }

    /// Materialise a resolved default into a Cranelift value at the call site.
    fn default_arg_value(&mut self, d: &DefaultArg) -> Result<(Value, ValTy), NativeError> {
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
    fn byref_cell_arg(&mut self, def_id: DefId, i: usize, args: &[Operand]) -> Option<Value> {
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
    fn user_call(
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
    fn fresh_composite_default(
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
    fn named_builtin(
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
    fn unary_poly(
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
                return if self.lang.version <= 1 {
                    Ok((self.b.ins().iconst(types::I64, 0), ValTy::Int))
                } else {
                    Ok((self.b.ins().f64const(0.0), ValTy::Real))
                };
            }
            if matches!(args.first(), Some(Operand::Const(Const::Null))) {
                return Err(unsupported(format!("{name}(null) — real/int result")));
            }
            return self.generic_builtin(name, args);
        }
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
    fn min_max(&mut self, want_min: bool, args: &[Operand]) -> Result<(Value, ValTy), NativeError> {
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
    fn count_call(&mut self, args: &[Operand]) -> Result<(Value, ValTy), NativeError> {
        let count = self.imports.rt("leek_count")?;
        let (v, t) = match args.first() {
            Some(op) => self.operand(op)?,
            None => return Err(unsupported("count: missing argument")),
        };
        let h = self.coerce(v, t, ValTy::Ref)?;
        let ver = self.b.ins().iconst(types::I64, i64::from(self.lang.version));
        let inst = self.b.ins().call(count, &[h, ver]);
        Ok((self.b.inst_results(inst)[0], ValTy::Int))
    }

    /// `push(arr, elem)` — append in place. Returns `null` (a handle), per
    /// the interpreter.
    fn push_call(&mut self, args: &[Operand]) -> Result<(Value, ValTy), NativeError> {
        if args.len() != 2 {
            return Err(unsupported("push: expected 2 args"));
        }
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
        self.b.ins().call(push, &[arr, elem]);
        let inst = self.b.ins().call(null, &[]);
        Ok((self.b.inst_results(inst)[0], ValTy::Ref))
    }


    fn terminator(&mut self, block_id: BlockId, t: &Terminator) -> Result<(), NativeError> {
        // A `default_init` block used as an entry-time param filler: its
        // `Return(op)` stores `op` into the param var (peeling/boxing for a
        // cell) and jumps to the continuation rather than returning.
        if let Some(&(param, cont)) = self.default_fill.get(&block_id) {
            let Terminator::Return(Some(op)) = t else {
                return Err(unsupported("default-init block: non-return terminator"));
            };
            let (v, vt) = self.operand(op)?;
            let target = self.var_tys[param.0 as usize];
            let v = self.coerce(v, vt, target)?;
            if self.cell_locals.contains(&param) {
                let cell = self.b.use_var(self.vars[param.0 as usize]);
                let set = self.imports.rt("leek_cell_set")?;
                self.b.ins().call(set, &[cell, v]);
            } else {
                self.b.def_var(self.vars[param.0 as usize], v);
            }
            self.flush_charge()?;
            self.b.ins().jump(cont, &[]);
            return Ok(());
        }
        match t {
            Terminator::Goto(b) => {
                self.flush_charge()?;
                self.b.ins().jump(self.blocks[b], &[]);
            }
            Terminator::Branch {
                cond,
                then_block,
                else_block,
            } => {
                // A conditional branch costs 1 op (interp `exec.rs` `If` step —
                // the if/while/and/or flow-control cost), charged before the
                // condition is evaluated.
                self.charge(1)?;
                // Flush the block's coalesced charge (including this branch op)
                // before the budget check so the check observes the full count.
                self.flush_charge()?;
                // Back-edge budget check: a branch is the only way to re-enter a
                // block, so checking here bounds every loop. Stops an unbounded
                // loop once the op budget is spent (when a finite one is set).
                self.emit_budget_check()?;
                let (c, ty) = self.operand(cond)?;
                // brif tests an integer for non-zero; a real condition
                // becomes `c != 0.0`; a dynamic value goes through the
                // shared `is_truthy` runtime shim.
                let c = if ty == ValTy::Ref {
                    let f = self.imports.rt("leek_truthy")?;
                    let inst = self.b.ins().call(f, &[c]);
                    self.b.inst_results(inst)[0]
                } else {
                    self.truthy(c, ty)
                };
                self.b
                    .ins()
                    .brif(c, self.blocks[then_block], &[], self.blocks[else_block], &[]);
            }
            Terminator::Return(Some(op)) => {
                // `return @x` (v1): hand back the *raw* shared cell so the
                // caller aliases the same storage (matches the interpreter).
                // The `@` marks the returned cell local `is_by_ref`; a plain
                // `return x` of a cell local peels as usual. Guarded to v1 + a
                // boxed (`Ref`) return type — a scalar `@x` return has no
                // aliasing to preserve and would mis-coerce a cell handle to an
                // int, so it peels instead.
                let (v, ty) = match op {
                    Operand::Local(id)
                        if self.lang.version <= 1
                            && self.ret_ty == ValTy::Ref
                            && self.cell_locals.contains(id)
                            && self.mir_locals[id.0 as usize].is_by_ref =>
                    {
                        (self.b.use_var(self.vars[id.0 as usize]), ValTy::Ref)
                    }
                    _ => self.operand(op)?,
                };
                let v = self.coerce(v, ty, self.ret_ty)?;
                self.flush_charge()?;
                self.b.ins().return_(&[v]);
            }
            // A void function returns null. With a `Ref` result that's a
            // boxed null; for the (rejected-for-main) scalar case it's a
            // dead dummy zero.
            Terminator::Return(None) => {
                self.flush_charge()?;
                let z = match self.ret_ty {
                    ValTy::Ref => {
                        let null = self.imports.rt("leek_box_null")?;
                        let inst = self.b.ins().call(null, &[]);
                        self.b.inst_results(inst)[0]
                    }
                    ValTy::Real => self.b.ins().f64const(0.0),
                    _ => self.b.ins().iconst(types::I64, 0),
                };
                self.b.ins().return_(&[z]);
            }
            Terminator::Switch {
                discriminant,
                arms,
                default,
            } => {
                self.flush_charge()?;
                let (disc, dty) = self.operand(discriminant)?;
                if dty == ValTy::Real {
                    return Err(unsupported("switch on real"));
                }
                for (k, target) in arms {
                    let key = match k {
                        Const::Int(n) => *n,
                        Const::Bool(x) => *x as i64,
                        other => return Err(unsupported(format!("switch on {other:?}"))),
                    };
                    let kv = self.b.ins().iconst(types::I64, key);
                    let eq = self.b.ins().icmp(IntCC::Equal, disc, kv);
                    let next = self.b.create_block();
                    self.b.ins().brif(eq, self.blocks[target], &[], next, &[]);
                    self.b.switch_to_block(next);
                }
                self.b.ins().jump(self.blocks[default], &[]);
            }
            Terminator::Unreachable => {
                self.flush_charge()?;
                self.b.ins().trap(TrapCode::user(1).unwrap());
            }
        }
        Ok(())
    }

    fn rvalue(&mut self, rv: &Rvalue) -> Result<(Value, ValTy), NativeError> {
        match rv {
            Rvalue::Use(op) | Rvalue::UseFresh(op) => self.operand(op),
            Rvalue::Binary(op, l, r) => self.binary(*op, l, r),
            Rvalue::Unary(op, x) => self.unary(*op, x),
            Rvalue::Array(elems) => self.array_literal(elems),
            Rvalue::Index(base, idx) => self.index(*base, idx),
            Rvalue::Slice(base, bounds) => self.slice(*base, bounds),
            Rvalue::MakeForeachIter(op) => self.foreach_iter(op),
            Rvalue::Map(pairs) => self.map_literal(pairs),
            Rvalue::Set(items) => self.set_literal(items),
            Rvalue::Interval(iv) => self.interval(iv),
            Rvalue::Object(fields) => self.object_literal(fields),
            Rvalue::Field(base, name) => self.field(*base, name),
            Rvalue::GlobalRef(_, name) => self.global_get(name),
            Rvalue::New { class, args } => self.new_instance(class, args),
            // `x -> …` / `function(){}`: build a closure value capturing the
            // current capture values (value-capture snapshot).
            Rvalue::MakeLambda {
                function_idx,
                captures,
            } => {
                let (ptr, n) = self.build_ref_array_opt(captures, true)?;
                let f = self.imports.rt("leek_make_lambda")?;
                let fidx = self.b.ins().iconst(types::I64, *function_idx as i64);
                let nc = self.b.ins().iconst(types::I64, n as i64);
                let inst = self.b.ins().call(f, &[fidx, ptr, nc]);
                Ok((self.b.inst_results(inst)[0], ValTy::Ref))
            }
            // A bare builtin name used as a value. A *constant* (`PI`,
            // `SORT_ASC`, …) boxes its value directly; a builtin *function*
            // (`arrayMap(arr, abs)`) boxes a `Function::Builtin` handle the
            // runtime's HOF dispatch (`dispatch_call_value`) invokes by
            // name. Unknown names skip.
            Rvalue::BuiltinRef(name) => {
                if let Some(v) = leek_runtime::lookup_constant(name) {
                    let ptr = crate::runtime::box_value(v) as i64;
                    Ok((self.b.ins().iconst(types::I64, ptr), ValTy::Ref))
                } else if let Some(cls) = leek_runtime::builtin_class_name(name) {
                    // A builtin class used as a value (`var c = Array`,
                    // `x instanceof Map`, `Integer.MAX_VALUE`). Box a
                    // `BuiltinClass` handle — the shared `value_instanceof`,
                    // `read_field` (statics) and `construct_builtin_class`
                    // all dispatch on it.
                    let v = leek_runtime::Value::BuiltinClass(cls);
                    let ptr = crate::runtime::box_value(v) as i64;
                    Ok((self.b.ins().iconst(types::I64, ptr), ValTy::Ref))
                } else if leek_runtime::is_known_builtin(name) {
                    // A user global with this name shadows the builtin
                    // (`abs = 2; return abs`, or `var _c = count; count = …`):
                    // the reference resolves dynamically — the global's value if
                    // one has been assigned, else the builtin handle. Defer to
                    // the `leek_ref_or_builtin` shim.
                    if program_writes_global(self.program, name) {
                        let name_h = self.const_string(name)?;
                        let f = self.imports.rt("leek_ref_or_builtin")?;
                        let inst = self.b.ins().call(f, &[name_h]);
                        return Ok((self.b.inst_results(inst)[0], ValTy::Ref));
                    }
                    let v = leek_runtime::Value::Function(leek_runtime::Function::Builtin(
                        name.clone(),
                    ));
                    let ptr = crate::runtime::box_value(v) as i64;
                    Ok((self.b.ins().iconst(types::I64, ptr), ValTy::Ref))
                } else {
                    // An unresolved bare name (not a constant/class/builtin) —
                    // a global declared elsewhere (`global x`) or an undefined
                    // reference. The interpreter reads the name-keyed global
                    // store, falling back to null; `leek_global_get` does
                    // exactly that.
                    let name_h = self.const_string(name)?;
                    let f = self.imports.rt("leek_global_get")?;
                    let inst = self.b.ins().call(f, &[name_h]);
                    Ok((self.b.inst_results(inst)[0], ValTy::Ref))
                }
            }
            // A class used as a value (`var c = C`, or the `C.staticMethod()`
            // receiver). The class identity is known at compile time, so box
            // it once into a leaked handle (like a string literal).
            Rvalue::ClassRef(def_id, name) => {
                let v = leek_runtime::Value::ClassRef(*def_id, std::rc::Rc::new(name.clone()));
                let ptr = crate::runtime::box_value(v) as i64;
                Ok((self.b.ins().iconst(types::I64, ptr), ValTy::Ref))
            }
            // A named function used as a value (`var f = foo`). Boxed as a
            // `Function::User` handle; invoked via the indirect-call path
            // (`USER_FN_IDX` → uniform body), so the function is uniform-
            // compiled + registered in `define_program`.
            Rvalue::FunctionRef(def_id) => {
                let v = leek_runtime::Value::Function(leek_runtime::Function::User(*def_id));
                let ptr = crate::runtime::box_value(v) as i64;
                Ok((self.b.ins().iconst(types::I64, ptr), ValTy::Ref))
            }
            // `super` evaluates to the same instance as `this`; the parent-class
            // dispatch happens at the method-call site (via `super_locals`).
            // Field access `super.field` then reads `this`'s field storage.
            Rvalue::MakeSuper { this, .. } => {
                let (v, t) = self.local_value(*this)?;
                let boxed = self.coerce(v, t, ValTy::Ref)?;
                Ok((boxed, ValTy::Ref))
            }
            // `expr as T` (and implicit numeric/bool/string coercions). Box the
            // operand and run the shared `apply_cast` (matching the interp);
            // the boxed result is coerced to the destination kind by the
            // consuming local's declared type.
            Rvalue::Cast(kind, x) => {
                let (v, t) = self.operand(x)?;
                let boxed = self.coerce(v, t, ValTy::Ref)?;
                let code: i64 = match kind {
                    leek_mir::ir::CastKind::IntToReal => 0,
                    leek_mir::ir::CastKind::RealToInt => 1,
                    leek_mir::ir::CastKind::ToBool => 2,
                    leek_mir::ir::CastKind::ToString => 3,
                    leek_mir::ir::CastKind::User => 4,
                };
                let f = self.imports.rt("leek_apply_cast")?;
                let codev = self.b.ins().iconst(types::I64, code);
                let inst = self.b.ins().call(f, &[codev, boxed]);
                Ok((self.b.inst_results(inst)[0], ValTy::Ref))
            }
            other => Err(unsupported(format!("rvalue {}", rvalue_name(other)))),
        }
    }

    /// `new C(args)`: allocate the instance, run each field's initializer
    /// (coercing to the declared field type and storing it), then run the
    /// selected constructor. The instance handle is the result.
    fn new_instance(
        &mut self,
        class: &str,
        args: &[Operand],
    ) -> Result<(Value, ValTy), NativeError> {
        if self.lang.version < 2 {
            return Err(unsupported("new (v1-3 value semantics)"));
        }
        // `new Array`/`new Map`/`new Set`/`new Object`/`new Integer(x)` etc.
        // — a builtin class, constructed via the shared
        // `construct_builtin_class`.
        if leek_runtime::builtin_class_name(class).is_some() {
            let name = self.const_string(class)?;
            let (ptr, n) = self.build_ref_array(args)?;
            let f = self.imports.rt("leek_construct_builtin")?;
            let nc = self.b.ins().iconst(types::I64, n as i64);
            let inst = self.b.ins().call(f, &[name, ptr, nc]);
            return Ok((self.b.inst_results(inst)[0], ValTy::Ref));
        }
        // `self.program` is a shared `&MirProgram` (independent of `&mut
        // self`), so the resolved class data outlives the builder calls.
        let prog = self.program;
        let Some(c) = prog.class_by_name(class) else {
            return Err(unsupported("new: unknown class"));
        };
        // `class A extends Array {}` (or Map/Set/Object) — upstream (and the
        // interpreter's `construct_user_class`) collapses the user class to the
        // underlying builtin constructor, so `new A()` is a plain `[]`,
        // `push(new A(), 12)` works, and `instanceof A` is false in BOTH
        // backends. Mirror that exactly. A non-collection builtin ancestor still
        // skips (native doesn't model those).
        if let Some(builtin) = builtin_ancestor(prog, c) {
            if matches!(builtin.as_str(), "Array" | "Map" | "Set" | "Object") {
                let name = self.const_string(&builtin)?;
                let (ptr, n) = self.build_ref_array(args)?;
                let f = self.imports.rt("leek_construct_builtin")?;
                let nc = self.b.ins().iconst(types::I64, n as i64);
                let inst = self.b.ins().call(f, &[name, ptr, nc]);
                return Ok((self.b.inst_results(inst)[0], ValTy::Ref));
            }
            return Err(unsupported("new: class extends a non-collection builtin"));
        }
        // A user `string()` method is a `Display`/`toString` override applied to
        // the *top-level* program result (mirroring the interpreter's
        // `invoke_instance_string_method`). The instance constructs normally;
        // `define_program` force-compiles + registers `string()` for every
        // constructed class so the post-run transform can invoke it. Nested
        // instances render as `Class {…}` in both backends, so construction is
        // always safe.
        let new = self.imports.rt("leek_instance_new")?;
        let set = self.imports.rt("leek_value_set_index")?;
        let class_def = self.b.ins().iconst(types::I64, i64::from(c.def_id.0));
        let name_box = self.const_string(&c.name)?;
        let inst = self.b.ins().call(new, &[class_def, name_box]);
        let this = self.b.inst_results(inst)[0];
        let ver = self.b.ins().iconst(types::I64, i64::from(self.lang.version));

        // Initialize every declared field (in flattened slot order, matching
        // the interpreter's parent-first order), so even initializer-less
        // fields exist (as null) — `Display` and `keys()` see them all.
        for fs in &c.field_layout {
            let key = self.const_string(&fs.name)?;
            let boxed = match fs.init_fn {
                None => {
                    let null = self.imports.rt("leek_box_null")?;
                    let inst = self.b.ins().call(null, &[]);
                    self.b.inst_results(inst)[0]
                }
                Some(fi) => {
                    let Some((fref, sig)) = self.imports.field_init_fns.get(&fi) else {
                        return Err(unsupported("field-init not compiled"));
                    };
                    let (fref, sig) = (*fref, sig.clone());
                    if sig.params.len() != 1 {
                        return Err(unsupported("field-init arity"));
                    }
                    let call_inst = self.b.ins().call(fref, &[this]);
                    let mut v = self.b.inst_results(call_inst)[0];
                    let mut vt = sig.ret;
                    // Coerce a scalar init to the declared field type
                    // (`real x = 12` / `real? x = 12` → 12.0).
                    if let Some(ft) = coerce_target_ty(&fs.ty)
                        && vt != ValTy::Ref
                    {
                        v = self.coerce(v, vt, ft)?;
                        vt = ft;
                    }
                    self.coerce(v, vt, ValTy::Ref)?
                }
            };
            self.b.ins().call(set, &[this, key, boxed, ver]);
        }

        // Constructor (if the class or an ancestor declares one).
        if let Some(ctor_idx) = prog.select_constructor(c, args.len())
            && let Some(def) = prog.functions[ctor_idx].def_id
        {
            let Some((fref, sig)) = self.imports.user_fns.get(&def) else {
                return Err(unsupported("constructor not compiled"));
            };
            let (fref, sig) = (*fref, sig.clone());
            // params = `this` (Ref) + user params. Too many args (variadic)
            // isn't modeled.
            let user_params = sig.params.len() - 1;
            if args.len() > user_params {
                return Err(unsupported("constructor variadic args"));
            }
            let ctor_fn = &prog.functions[ctor_idx];
            if sig.has_defaults {
                // The constructor fills omitted defaulted params itself via the
                // hidden `argc` (= `this` + provided args).
                for i in args.len()..user_params {
                    if fillable_default(ctor_fn, ctor_fn.params[i + 1]).is_none() {
                        return Err(unsupported("constructor: omitted param without default"));
                    }
                }
                let mut cl_args = Vec::with_capacity(sig.params.len() + 1);
                cl_args.push(self.coerce(this, ValTy::Ref, sig.params[0])?);
                for (i, &pty) in sig.params[1..].iter().enumerate() {
                    if i < args.len() {
                        let (v, t) = self.operand(&args[i])?;
                        cl_args.push(self.coerce(v, t, pty)?);
                    } else {
                        cl_args.push(self.placeholder(pty));
                    }
                }
                cl_args.push(self.b.ins().iconst(types::I64, (args.len() + 1) as i64));
                self.b.ins().call(fref, &cl_args);
            } else {
                // An omitted trailing user param is filled from its
                // self-contained constant default (`constructor(x = 2)`); a
                // param with no default binds to null — matching the
                // interpreter — provided the slot is a boxed `Ref` (a declared
                // scalar param can't hold null, so that still skips).
                let mut cl_args = Vec::with_capacity(sig.params.len());
                cl_args.push(self.coerce(this, ValTy::Ref, sig.params[0])?);
                for (i, &pty) in sig.params[1..].iter().enumerate() {
                    let (v, t) = if i < args.len() {
                        self.operand(&args[i])?
                    } else if let Some(c) = const_default(ctor_fn, ctor_fn.params[i + 1]) {
                        self.operand(&Operand::Const(c))?
                    } else if pty == ValTy::Ref {
                        self.operand(&Operand::Const(Const::Null))?
                    } else {
                        return Err(unsupported("constructor: omitted scalar param without default"));
                    };
                    cl_args.push(self.coerce(v, t, pty)?);
                }
                self.b.ins().call(fref, &cl_args);
            }
        }

        Ok((this, ValTy::Ref))
    }

    /// `true` if the class-reference local `id` names a class that has a
    /// constructor thunk — so its `Value::ClassRef` can flow as a callable
    /// value (constructing via `dispatch_call_value`).
    fn classref_has_thunk(&self, id: LocalId) -> bool {
        self.classref_locals
            .get(&id)
            .and_then(|cls| self.program.class_by_name(cls))
            .is_some_and(|c| self.ctor_thunk_classes.contains(&c.def_id.0))
    }

    /// `true` if a write to field `name` on `base` must silently no-op:
    /// the field is `final` AND the write comes from outside the receiver's
    /// class. Inside the class (its constructor/methods) `final` fields are
    /// writable — matching the interpreter's `caller_class_def() != class`.
    fn is_final_field(&self, base: LocalId, name: &str) -> bool {
        let Some(cls) = receiver_class(self.mir_locals, self.new_classes, self.aliased_classes, base) else {
            return false;
        };
        let Some(c) = self.program.class_by_name(cls) else {
            return false;
        };
        c.field_slot(name)
            .is_some_and(|fs| fs.is_final && self.owning_class != Some(c.def_id))
    }

    /// The scalar kind a write to field `name` on `base` (a known class
    /// instance) coerces to (`real? x = 5` stores `5.0`), if any.
    fn field_coerce_ty(&self, base: LocalId, name: &str) -> Option<ValTy> {
        receiver_class(self.mir_locals, self.new_classes, self.aliased_classes, base)
            .and_then(|cls| self.program.class_by_name(cls))
            .and_then(|c| c.field_slot(name))
            .and_then(|fs| coerce_target_ty(&fs.ty))
    }

    /// `true` if some strict descendant of `base` overrides the instance
    /// method `(name, arity)` — so a receiver of static type `base` whose
    /// runtime class might be that subclass needs virtual dispatch, which
    /// the static dispatch here can't provide.
    fn overridden_below(&self, base: &leek_mir::ir::MirClass, name: &str, arity: usize) -> bool {
        self.program.classes.iter().any(|c| {
            c.def_id != base.def_id
                && self.class_descends_from(Some(c.def_id), base.def_id)
                && c.methods
                    .iter()
                    .any(|m| !m.is_static && m.name == name && m.user_arity == arity)
        })
    }

    /// Whether a member owned by `owner` with visibility `vis` is reachable
    /// from the function being compiled. Mirrors the interpreter's
    /// `member_visible`: public always; private only from the same class;
    /// protected from the class or a descendant.
    fn method_visible(&self, owner: DefId, vis: Visibility) -> bool {
        match vis {
            Visibility::Public => true,
            Visibility::Private => self.owning_class == Some(owner),
            Visibility::Protected => self.class_descends_from(self.owning_class, owner),
        }
    }

    /// True if `caller` is `owner` or a subclass of it (cycle-safe), via the
    /// resolved `parent_def` chain.
    fn class_descends_from(&self, caller: Option<DefId>, owner: DefId) -> bool {
        let Some(start) = caller else { return false };
        let mut cur = Some(start);
        let mut seen: HashSet<DefId> = HashSet::new();
        while let Some(d) = cur {
            if !seen.insert(d) {
                return false;
            }
            if d == owner {
                return true;
            }
            cur = self.program.class(d).and_then(|c| c.parent_def);
        }
        false
    }

    /// Try to statically dispatch `receiver.method(args)` to a user-class
    /// method. Returns `Ok(None)` when the receiver isn't a known
    /// `ClassInstance` or the class has no such method (so the caller can
    /// fall back to builtin-method sugar).
    /// `C.staticMethod(args)` — the receiver is a class reference. Resolve
    /// the static method and call it as a free function (no `this`). Mirrors
    /// the interpreter's `dispatch_method_call` on a `Value::ClassRef`.
    fn try_static_method(
        &mut self,
        receiver: LocalId,
        method: &str,
        args: &[Operand],
    ) -> Result<Option<(Value, ValTy)>, NativeError> {
        let Some(cls) = self.classref_locals.get(&receiver).cloned() else {
            return Ok(None);
        };
        let Some(idx) = resolve_static_method(self.program, &cls, method, args.len()) else {
            // Not a static method — but it may be a static *field* holding a
            // callable (`static a = -> 12` then `A.a()`): read the field and
            // invoke the value indirectly (matching the interpreter, which
            // reads the static member then `call_value`s it). The field's
            // initializer (+ lambda) is made reachable by
            // `static_field_accesses` recognizing this call shape.
            if let Some((owner, fld)) = resolve_static_field(self.program, &cls, method) {
                if !self.method_visible(owner, fld.visibility) {
                    return Err(unsupported("static field (callable) not visible"));
                }
                let coerce = coerce_target_ty(&fld.ty);
                let callee = self.static_field_get(owner, method, coerce)?;
                let (ptr, n) = self.build_ref_array(args)?;
                let f = self.imports.rt("leek_call_value")?;
                let nc = self.b.ins().iconst(types::I64, n as i64);
                let ver = self.b.ins().iconst(types::I64, i64::from(self.lang.version));
                let inst = self.b.ins().call(f, &[callee, ptr, nc, ver]);
                return Ok(Some((self.b.inst_results(inst)[0], ValTy::Ref)));
            }
            // Otherwise an instance method used as a free function, etc. —
            // skip rather than miscompile.
            return Err(unsupported("static method not found on class reference"));
        };
        // An inaccessible static method yields null (matching the interp).
        if let Some(owner) = self.program.functions[idx].owning_class
            && let Some(c) = self.program.class(owner)
            && let Some(m) = c.methods.iter().find(|m| m.function_idx == idx)
            && !self.method_visible(owner, m.visibility)
        {
            let null = self.imports.rt("leek_box_null")?;
            let inst = self.b.ins().call(null, &[]);
            return Ok(Some((self.b.inst_results(inst)[0], ValTy::Ref)));
        }
        let Some(def) = self.program.functions[idx].def_id else {
            return Ok(None);
        };
        if !self.imports.user_fns.contains_key(&def) {
            return Err(unsupported("static method target not compiled"));
        }
        Ok(Some(self.user_call(def, args)?))
    }

    /// `super.m(args)` — the receiver is a `MakeSuper` local. Resolve `m`
    /// against the *parent* class and dispatch statically (super is never
    /// virtual), passing the real `this` instance as the receiver.
    fn try_super_method(
        &mut self,
        receiver: LocalId,
        method: &str,
        args: &[Operand],
    ) -> Result<Option<(Value, ValTy)>, NativeError> {
        if self.lang.version < 2 {
            return Ok(None);
        }
        let Some((this_local, parent)) = self.super_locals.get(&receiver).cloned() else {
            return Ok(None);
        };
        let prog = self.program;
        let Some(c) = prog.class_by_name(&parent) else {
            return Ok(None);
        };
        let Some(vt) = prog.resolve_method(c, method, Some(args.len())) else {
            return Err(unsupported("super method not found"));
        };
        // An inaccessible parent method through `super` is ambiguous between
        // the interp's null-dispatch and an actual call — skip rather than
        // risk a wrong result.
        if !self.method_visible(vt.owner, vt.visibility) {
            return Err(unsupported("super method not visible"));
        }
        let Some(def) = prog.functions[vt.function_idx].def_id else {
            return Ok(None);
        };
        let Some((fref, sig)) = self.imports.user_fns.get(&def) else {
            return Err(unsupported("super method target not compiled"));
        };
        let (fref, sig) = (*fref, sig.clone());
        if args.len() + 1 != sig.params.len() {
            return Ok(None);
        }
        let (recv, recv_ty) = self.local_value(this_local)?;
        let mut cl_args = Vec::with_capacity(args.len() + 1);
        cl_args.push(self.coerce(recv, recv_ty, sig.params[0])?);
        for (op, &pty) in args.iter().zip(&sig.params[1..]) {
            let (v, t) = self.operand(op)?;
            cl_args.push(self.coerce(v, t, pty)?);
        }
        let inst = self.b.ins().call(fref, &cl_args);
        Ok(Some((self.b.inst_results(inst)[0], sig.ret)))
    }

    fn try_user_method(
        &mut self,
        receiver: LocalId,
        method: &str,
        args: &[Operand],
    ) -> Result<Option<(Value, ValTy)>, NativeError> {
        if self.lang.version < 2 {
            return Ok(None);
        }
        let prog = self.program;
        let Some(name) = receiver_class(self.mir_locals, self.new_classes, self.aliased_classes, receiver) else {
            // Unknown receiver class. If `method` is a (non-builtin) user method
            // of some class, dispatch dynamically on the receiver's RUNTIME class
            // via `leek_call_method` (instance method else builtin) — mirroring
            // the interpreter, and matching `dynamic_method_targets`' seeding.
            if !leek_runtime::is_known_builtin(method)
                && prog
                    .classes
                    .iter()
                    .any(|c| prog.resolve_method(c, method, Some(args.len())).is_some())
            {
                let (recv, recv_ty) = self.local_value(receiver)?;
                let recv = self.coerce(recv, recv_ty, ValTy::Ref)?;
                let key = self.const_string(method)?;
                let (ptr, n) = self.build_ref_array(args)?;
                let nc = self.b.ins().iconst(types::I64, n as i64);
                let ver = self.b.ins().iconst(types::I64, i64::from(self.lang.version));
                let f = self.imports.rt("leek_call_method")?;
                let inst = self.b.ins().call(f, &[recv, key, ptr, nc, ver]);
                return Ok(Some((self.b.inst_results(inst)[0], ValTy::Ref)));
            }
            return Ok(None);
        };
        let Some(c) = prog.class_by_name(name) else {
            return Ok(None);
        };
        let Some(vt) = prog.resolve_method(c, method, Some(args.len())) else {
            // No method by that name. If the class has a data *field* with
            // that name, `obj.field(args)` reads the field and invokes its
            // value (a stored function/lambda/bound method) — matching the
            // interpreter's `dispatch_method_call`, which falls back to a field
            // read + `call_value`.
            if c.field_slot(method).is_some() {
                let (callee, ct) = self.field(receiver, method)?;
                let callee = self.coerce(callee, ct, ValTy::Ref)?;
                let (ptr, n) = self.build_ref_array(args)?;
                let f = self.imports.rt("leek_call_value")?;
                let nc = self.b.ins().iconst(types::I64, n as i64);
                let ver = self.b.ins().iconst(types::I64, i64::from(self.lang.version));
                let inst = self.b.ins().call(f, &[callee, ptr, nc, ver]);
                return Ok(Some((self.b.inst_results(inst)[0], ValTy::Ref)));
            }
            // No method and no field — the interpreter falls back to
            // `run_method` = `run_builtin(method, [receiver, …args])` (a builtin
            // method on the value; an unknown name or a math builtin on a
            // non-number yields null). The generic `call_builtin` shim
            // reproduces this EXACTLY (same `call_builtin`, null on Err), so
            // route through it rather than the type-coercing math path.
            let mut combined = Vec::with_capacity(args.len() + 1);
            combined.push(Operand::Local(receiver));
            combined.extend(args.iter().cloned());
            return Ok(Some(self.generic_builtin(method, &combined)?));
        };
        // Static dispatch is only sound when the receiver's runtime class is
        // its static class. A `new C()` temp is exact; a `this`/typed local
        // could be a subclass. If a subclass overrides this method, dispatch
        // VIRTUALLY: build a bound method keyed on the receiver's RUNTIME class
        // (via `leek_value_index` → `METHOD_RESOLVE`, seeded for the whole
        // hierarchy by `virtual_method_targets`) and invoke it. Gated to public
        // methods — an override's visibility depends on the runtime class,
        // which the static check can't model.
        let exact = self.new_classes.contains_key(&receiver);
        if !exact && self.overridden_below(c, method, args.len()) {
            if !matches!(vt.visibility, Visibility::Public) {
                return Err(unsupported("virtual dispatch (non-public method)"));
            }
            let (recv, recv_ty) = self.local_value(receiver)?;
            let recv = self.coerce(recv, recv_ty, ValTy::Ref)?;
            let key = self.const_string(method)?;
            let ver = self.b.ins().iconst(types::I64, i64::from(self.lang.version));
            let idx = self.imports.rt("leek_value_index")?;
            let inst = self.b.ins().call(idx, &[recv, key, ver]);
            let bound = self.b.inst_results(inst)[0];
            let (ptr, n) = self.build_ref_array(args)?;
            let f = self.imports.rt("leek_call_value")?;
            let nc = self.b.ins().iconst(types::I64, n as i64);
            let inst = self.b.ins().call(f, &[bound, ptr, nc, ver]);
            return Ok(Some((self.b.inst_results(inst)[0], ValTy::Ref)));
        }
        // An inaccessible method call yields null (matching the interpreter's
        // `dispatch_method_call`), rather than invoking the method.
        if !self.method_visible(vt.owner, vt.visibility) {
            let null = self.imports.rt("leek_box_null")?;
            let inst = self.b.ins().call(null, &[]);
            return Ok(Some((self.b.inst_results(inst)[0], ValTy::Ref)));
        }
        let Some(def) = prog.functions[vt.function_idx].def_id else {
            return Ok(None);
        };
        let Some((fref, sig)) = self.imports.user_fns.get(&def) else {
            return Err(unsupported("method target not compiled"));
        };
        let (fref, sig) = (*fref, sig.clone());
        // params = `this` (Ref) + user params. Too many args (variadic) →
        // fall back. Too few → pad omitted trailing params that carry a
        // self-contained default (`m(x = 2)`); otherwise fall back.
        let n_user_params = sig.params.len() - 1;
        if args.len() > n_user_params {
            return Ok(None);
        }
        // `has_defaults`: the method fills omitted defaulted params itself via
        // the hidden `argc` (= `this` + provided user args). Each omitted user
        // param must carry a default; pass placeholders + the count.
        if sig.has_defaults {
            let Some(callee) = prog.function(def) else {
                return Ok(None);
            };
            for i in args.len()..n_user_params {
                if fillable_default(callee, callee.params[i + 1]).is_none() {
                    return Ok(None);
                }
            }
            let (recv, recv_ty) = self.local_value(receiver)?;
            let mut cl_args = Vec::with_capacity(sig.params.len() + 1);
            cl_args.push(self.coerce(recv, recv_ty, sig.params[0])?);
            for idx in 0..n_user_params {
                let pty = sig.params[idx + 1];
                if idx < args.len() {
                    let (v, t) = self.operand(&args[idx])?;
                    cl_args.push(self.coerce(v, t, pty)?);
                } else {
                    cl_args.push(self.placeholder(pty));
                }
            }
            // argc counts `this` + the provided user args.
            cl_args.push(self.b.ins().iconst(types::I64, (args.len() + 1) as i64));
            let inst = self.b.ins().call(fref, &cl_args);
            return Ok(Some((self.b.inst_results(inst)[0], sig.ret)));
        }
        let mut defaults: Vec<DefaultArg> = Vec::new();
        if args.len() < n_user_params {
            let Some(callee) = prog.function(def) else {
                return Ok(None);
            };
            for i in args.len()..n_user_params {
                match self.param_default(callee, callee.params[i + 1]) {
                    Some(d) => defaults.push(d),
                    None => return Ok(None),
                }
            }
        }
        let (recv, recv_ty) = self.local_value(receiver)?;
        let mut cl_args = Vec::with_capacity(sig.params.len());
        cl_args.push(self.coerce(recv, recv_ty, sig.params[0])?);
        for idx in 0..n_user_params {
            let pty = sig.params[idx + 1];
            let (v, t) = if idx < args.len() {
                self.operand(&args[idx])?
            } else {
                self.default_arg_value(&defaults[idx - args.len()])?
            };
            cl_args.push(self.coerce(v, t, pty)?);
        }
        let inst = self.b.ins().call(fref, &cl_args);
        Ok(Some((self.b.inst_results(inst)[0], sig.ret)))
    }

    /// `o.field(args)` where `o` is an object literal — read the field and
    /// invoke its value (`leek_call_value`: a callable runs, a non-callable /
    /// absent field yields null, matching the interpreter's `Object` arm of
    /// `dispatch_method_call`). Returns `Ok(None)` when `o` isn't a known
    /// object local (so the caller falls back to builtin-method sugar).
    fn try_object_method(
        &mut self,
        receiver: LocalId,
        field: &str,
        args: &[Operand],
    ) -> Result<Option<(Value, ValTy)>, NativeError> {
        if !self.object_locals.contains(&receiver) {
            return Ok(None);
        }
        // Only a field the object literal actually DEFINES is an
        // object-field-call. A *missing* field name is a builtin method on the
        // object (`{}.keys()`, `{a:1}.values()`) — fall through to builtin
        // sugar (the interpreter's `Object` arm does the same: field-then-
        // `run_method`).
        let Some(field_op) = self.object_field_srcs.get(&receiver).and_then(|m| m.get(field))
        else {
            return Ok(None);
        };
        // A field holding a *user* class reference CONSTRUCTS the class when
        // invoked. That works when the class has a constructor thunk (the field
        // holds a `Value::ClassRef`, and `dispatch_call_value` constructs via
        // the thunk). Without one (un-constructible class / v1) the runtime
        // couldn't build it, so skip. (Builtin classes always construct via the
        // `BuiltinClass` arm, so those are fine.)
        if let Operand::Local(l) = field_op
            && self.classref_locals.contains_key(l)
            && !self.classref_has_thunk(*l)
        {
            return Err(unsupported("object field holds a class reference"));
        }
        let (callee, ct) = self.field(receiver, field)?;
        let callee = self.coerce(callee, ct, ValTy::Ref)?;
        let (ptr, n) = self.build_ref_array(args)?;
        let f = self.imports.rt("leek_call_value")?;
        let nc = self.b.ins().iconst(types::I64, n as i64);
        let ver = self.b.ins().iconst(types::I64, i64::from(self.lang.version));
        let inst = self.b.ins().call(f, &[callee, ptr, nc, ver]);
        Ok(Some((self.b.inst_results(inst)[0], ValTy::Ref)))
    }

    /// Build an object literal `{f: v, …}`: allocate, then set each field
    /// (reusing the generic index-set, which routes objects to `set_field`).
    fn object_literal(&mut self, fields: &[(String, Operand)]) -> Result<(Value, ValTy), NativeError> {
        let new = self.imports.rt("leek_object_new")?;
        let set = self.imports.rt("leek_value_set_index")?;
        let inst = self.b.ins().call(new, &[]);
        let obj = self.b.inst_results(inst)[0];
        let ver = self.b.ins().iconst(types::I64, i64::from(self.lang.version));
        for (name, op) in fields {
            let key = self.const_string(name)?;
            let (v, t) = self.operand(op)?;
            let val = self.coerce(v, t, ValTy::Ref)?;
            self.b.ins().call(set, &[obj, key, val, ver]);
        }
        Ok((obj, ValTy::Ref))
    }

    /// `base.field` read — a string-keyed index (`read_index` handles
    /// objects/instances via `read_field`).
    /// Emit a boxed-null handle.
    fn boxed_null(&mut self) -> Result<Value, NativeError> {
        let null = self.imports.rt("leek_box_null")?;
        let inst = self.b.ins().call(null, &[]);
        Ok(self.b.inst_results(inst)[0])
    }

    /// Emit a `leek_static_get(owner_def, name)` — lazily-initialised
    /// per-class static-field storage. `decl_ty` is the field's declared
    /// type; a scalar one coerces the stored value (so `real? a = 12` reads
    /// back `12.0`).
    fn static_field_get(
        &mut self,
        owner: DefId,
        name: &str,
        coerce: Option<ValTy>,
    ) -> Result<Value, NativeError> {
        let get = self.imports.rt("leek_static_get")?;
        let cd = self.b.ins().iconst(types::I64, owner.0 as i64);
        let key = self.const_string(name)?;
        let inst = self.b.ins().call(get, &[cd, key]);
        let mut v = self.b.inst_results(inst)[0];
        if let Some(kind) = coerce {
            let coerce = self.imports.rt("leek_coerce_scalar")?;
            let k = self.b.ins().iconst(
                types::I64,
                match kind {
                    ValTy::Int => 0,
                    ValTy::Real => 1,
                    _ => 2,
                },
            );
            let inst = self.b.ins().call(coerce, &[v, k]);
            v = self.b.inst_results(inst)[0];
        }
        Ok(v)
    }

    /// Emit a `leek_static_set(owner_def, name, val)`.
    fn static_field_set(&mut self, owner: DefId, name: &str, val: Value) -> Result<(), NativeError> {
        let set = self.imports.rt("leek_static_set")?;
        let cd = self.b.ins().iconst(types::I64, owner.0 as i64);
        let key = self.const_string(name)?;
        self.b.ins().call(set, &[cd, key, val]);
        Ok(())
    }

    fn field(&mut self, base: LocalId, name: &str) -> Result<(Value, ValTy), NativeError> {
        // `C.super` on a class reference is C's parent class. When the base is
        // a known class-ref with an explicit user parent (`parent_def`), box it
        // at compile time. Otherwise (`x.class.super`, or a class with no
        // explicit parent → implicit builtin `Value` base) resolve at runtime
        // via `leek_class_super` (backed by the `CLASS_PARENT` table).
        if name == "super" {
            if let Some(cls) = self.classref_locals.get(&base).cloned()
                && let Some(c) = self.program.class_by_name(&cls)
                && let Some(pdef) = c.parent_def
                && let Some(pc) = self.program.class(pdef)
            {
                let v =
                    leek_runtime::Value::ClassRef(pdef, std::rc::Rc::new(pc.name.clone()));
                let ptr = crate::runtime::box_value(v) as i64;
                return Ok((self.b.ins().iconst(types::I64, ptr), ValTy::Ref));
            }
            let (v, t) = self.local_value(base)?;
            let boxed = self.coerce(v, t, ValTy::Ref)?;
            let f = self.imports.rt("leek_class_super")?;
            let inst = self.b.ins().call(f, &[boxed]);
            return Ok((self.b.inst_results(inst)[0], ValTy::Ref));
        }
        // `.class` is a meta-property (the runtime class) available on ANY
        // value, including scalars — box the base and ask the shared
        // `class_of`.
        if name == "class" {
            let (v, t) = self.local_value(base)?;
            let boxed = self.coerce(v, t, ValTy::Ref)?;
            let f = self.imports.rt("leek_class_of")?;
            let inst = self.b.ins().call(f, &[boxed]);
            return Ok((self.b.inst_results(inst)[0], ValTy::Ref));
        }
        // `C.member` — a class reference's member.
        if let Some(cls) = self.classref_locals.get(&base).cloned() {
            // `C.name` is the class name (a compile-time-known string).
            if name == "name" {
                let v = leek_runtime::Value::String(std::rc::Rc::new(cls));
                let ptr = crate::runtime::box_value(v) as i64;
                return Ok((self.b.ins().iconst(types::I64, ptr), ValTy::Ref));
            }
            // A static field reads from per-class storage (lazily initialised).
            if let Some((owner, fld)) = resolve_static_field(self.program, &cls, name) {
                if !self.method_visible(owner, fld.visibility) {
                    return Ok((self.boxed_null()?, ValTy::Ref));
                }
                let coerce = coerce_target_ty(&fld.ty);
                let v = self.static_field_get(owner, name, coerce)?;
                return Ok((v, ValTy::Ref));
            }
            // Reflective members (`C.fields`, `C.methods`, …) are known at
            // compile time — materialise the array as a leaked handle.
            if let Some(v) = class_reflect(self.program, &cls, name) {
                let ptr = crate::runtime::box_value(v) as i64;
                return Ok((self.b.ins().iconst(types::I64, ptr), ValTy::Ref));
            }
            // A static method read as a *value* (`var f = C.staticMethod`):
            // box a `Function::User` handle (the method is uniform-compiled +
            // registered in `USER_FN_IDX` via `static_method_value_info`), so
            // it can be invoked indirectly through `leek_call_value`.
            if let Some(idx) = resolve_static_method_value(self.program, &cls, name)
                && let Some(def) = self.program.functions[idx].def_id
            {
                let v = leek_runtime::Value::Function(leek_runtime::Function::User(def));
                let ptr = crate::runtime::box_value(v) as i64;
                return Ok((self.b.ins().iconst(types::I64, ptr), ValTy::Ref));
            }
            // An *instance* method read via the class ref (`var f = A.m`) is an
            // unbound method value — invoked as `f(receiver, …)`, the receiver
            // becomes the method's first (`this`) parameter. Box the same
            // `Function::User` handle (the method is uniform-compiled +
            // registered via `static_method_value_refs`).
            if let Some(idx) = resolve_instance_method_value(self.program, &cls, name)
                && let Some(def) = self.program.functions[idx].def_id
            {
                let v = leek_runtime::Value::Function(leek_runtime::Function::User(def));
                let ptr = crate::runtime::box_value(v) as i64;
                return Ok((self.b.ins().iconst(types::I64, ptr), ValTy::Ref));
            }
            // Other class members aren't representable here — skip.
            return Err(unsupported("class reference member access"));
        }
        if self.var_tys[base.0 as usize] != ValTy::Ref {
            return Err(unsupported("field of non-object"));
        }
        // `obj.m` where `m` is a method (not a stored field) is a bound-method
        // value. It falls through to `leek_value_index`, which builds the
        // `BoundMethod` from `METHOD_RESOLVE` — seeded for `obj.m` field reads
        // by `index_method_targets` (so the method is reachable + uniform-
        // compiled). The receiver is captured, so calling it prepends `obj`.
        let (base_h, _) = self.local_value(base)?;
        let v = self.field_get_boxed(base_h, name)?;
        Ok((v, ValTy::Ref))
    }

    /// Read a file-level global by name.
    fn global_get(&mut self, name: &str) -> Result<(Value, ValTy), NativeError> {
        let get = self.imports.rt("leek_global_get")?;
        let key = self.const_string(name)?;
        let inst = self.b.ins().call(get, &[key]);
        Ok((self.b.inst_results(inst)[0], ValTy::Ref))
    }

    /// Build an interval `[start..end]`. Endpoints are boxed (a null
    /// handle marks an unbounded end); inclusivity / forces-real bits are
    /// packed into `flags`. `step` is ignored (as in the interpreter).
    fn interval(&mut self, iv: &leek_mir::ir::IntervalRvalue) -> Result<(Value, ValTy), NativeError> {
        // Interval-literal construction costs 2 ops (interp `exec.rs`).
        self.charge(2)?;
        let f = self.imports.rt("leek_interval")?;
        let bound = |this: &mut Self, op: &Option<Operand>| -> Result<Value, NativeError> {
            match op {
                Some(o) => {
                    let (v, t) = this.operand(o)?;
                    this.coerce(v, t, ValTy::Ref)
                }
                None => Ok(this.b.ins().iconst(types::I64, 0)), // null handle
            }
        };
        let start = bound(self, &iv.start)?;
        let end = bound(self, &iv.end)?;
        let flags = (iv.start_inclusive as i64)
            | ((iv.end_inclusive as i64) << 1)
            | ((iv.start_forces_real as i64) << 2)
            | ((iv.end_forces_real as i64) << 3);
        let flags = self.b.ins().iconst(types::I64, flags);
        let inst = self.b.ins().call(f, &[start, end, flags]);
        Ok((self.b.inst_results(inst)[0], ValTy::Ref))
    }

    /// Build a map literal: allocate, then box-and-insert each key/value.
    fn map_literal(&mut self, pairs: &[(Operand, Operand)]) -> Result<(Value, ValTy), NativeError> {
        let new = self.imports.rt("leek_map_new")?;
        let put = self.imports.rt("leek_map_put")?;
        let inst = self.b.ins().call(new, &[]);
        let map = self.b.inst_results(inst)[0];
        for (k, v) in pairs {
            let (kv, kt) = self.operand(k)?;
            let key = self.coerce(kv, kt, ValTy::Ref)?;
            let (vv, vt) = self.operand(v)?;
            let val = self.coerce(vv, vt, ValTy::Ref)?;
            self.b.ins().call(put, &[map, key, val]);
        }
        Ok((map, ValTy::Ref))
    }

    /// Build a set literal: allocate, then box-and-add each element.
    fn set_literal(&mut self, items: &[Operand]) -> Result<(Value, ValTy), NativeError> {
        // Set-literal construction costs 2 ops per element (interp `exec.rs`).
        self.charge(2 * items.len() as u64)?;
        let new = self.imports.rt("leek_set_new")?;
        let add = self.imports.rt("leek_set_add")?;
        let inst = self.b.ins().call(new, &[]);
        let set = self.b.inst_results(inst)[0];
        for o in items {
            let (v, t) = self.operand(o)?;
            let elem = self.coerce(v, t, ValTy::Ref)?;
            self.b.ins().call(add, &[set, elem]);
        }
        Ok((set, ValTy::Ref))
    }

    /// Build a `foreach` iterator handle (`[key, value]` pairs) from an
    /// iterable. The loop body then indexes it: `iter[i]` is a pair,
    /// `iter[i][1]` the value — both lower through `index`.
    fn foreach_iter(&mut self, op: &Operand) -> Result<(Value, ValTy), NativeError> {
        let f = self.imports.rt("leek_foreach_iter")?;
        let (v, t) = self.operand(op)?;
        let h = self.coerce(v, t, ValTy::Ref)?;
        let inst = self.b.ins().call(f, &[h]);
        Ok((self.b.inst_results(inst)[0], ValTy::Ref))
    }

    /// Build an array literal: allocate, then box-and-push each element.
    /// Composite support is v4-only (v1–v3 arrays are value-typed, which
    /// this aliasing handle model doesn't reproduce).
    fn array_literal(&mut self, elems: &[Operand]) -> Result<(Value, ValTy), NativeError> {
        let new = self.imports.rt("leek_array_new")?;
        let push = self.imports.rt("leek_array_push")?;
        let inst = self.b.ins().call(new, &[]);
        let arr = self.b.inst_results(inst)[0];
        for op in elems {
            let (v, t) = self.operand(op)?;
            let boxed = self.coerce(v, t, ValTy::Ref)?;
            self.b.ins().call(push, &[arr, boxed]);
        }
        Ok((arr, ValTy::Ref))
    }

    /// `base[idx]` read for any indexable handle (array / string / map /
    /// …). The result is a handle to the element (out-of-range → `null`).
    /// `base[start:end:step]` — slice an array / string / interval. Each
    /// bound is boxed (a null handle marks an absent bound).
    fn slice(
        &mut self,
        base: LocalId,
        bounds: &leek_mir::ir::SliceBounds,
    ) -> Result<(Value, ValTy), NativeError> {
        if self.var_tys[base.0 as usize] != ValTy::Ref {
            return Err(unsupported("slice of non-composite"));
        }
        let f = self.imports.rt("leek_slice")?;
        let (base_h, _) = self.local_value(base)?;
        let bound = |this: &mut Self, op: &Option<Operand>| -> Result<Value, NativeError> {
            match op {
                Some(o) => {
                    let (v, t) = this.operand(o)?;
                    this.coerce(v, t, ValTy::Ref)
                }
                None => Ok(this.boxed_null()?),
            }
        };
        let start = bound(self, &bounds.start)?;
        let end = bound(self, &bounds.end)?;
        let step = bound(self, &bounds.step)?;
        let inst = self.b.ins().call(f, &[base_h, start, end, step]);
        Ok((self.b.inst_results(inst)[0], ValTy::Ref))
    }

    fn index(&mut self, base: LocalId, idx: &Operand) -> Result<(Value, ValTy), NativeError> {
        // `C['member']` indexes a class reference — resolved like `C.member`
        // (the interpreter treats index and field member access identically).
        if let Some(cls) = self.classref_locals.get(&base).cloned() {
            if let Operand::Const(Const::String(name)) = idx {
                // `C['name']` — the class name.
                if name.as_str() == "name" {
                    let v = leek_runtime::Value::String(std::rc::Rc::new(cls.clone()));
                    let ptr = crate::runtime::box_value(v) as i64;
                    return Ok((self.b.ins().iconst(types::I64, ptr), ValTy::Ref));
                }
                // A static field.
                if let Some((owner, fld)) = resolve_static_field(self.program, &cls, name) {
                    if !self.method_visible(owner, fld.visibility) {
                        return Ok((self.boxed_null()?, ValTy::Ref));
                    }
                    let coerce = coerce_target_ty(&fld.ty);
                    let v = self.static_field_get(owner, name, coerce)?;
                    return Ok((v, ValTy::Ref));
                }
                // A reflective member (`C['fields']`, …).
                if let Some(v) = class_reflect(self.program, &cls, name) {
                    let ptr = crate::runtime::box_value(v) as i64;
                    return Ok((self.b.ins().iconst(types::I64, ptr), ValTy::Ref));
                }
                // A static or instance method read as a value (`A['m']`) — box
                // a `Function::User` handle (uniform-compiled + registered via
                // `static_method_value_refs`, which scans string-`Index` reads).
                if let Some(idx) = resolve_static_method_value(self.program, &cls, name)
                    .or_else(|| resolve_instance_method_value(self.program, &cls, name))
                    && let Some(def) = self.program.functions[idx].def_id
                {
                    let v = leek_runtime::Value::Function(leek_runtime::Function::User(def));
                    let ptr = crate::runtime::box_value(v) as i64;
                    return Ok((self.b.ins().iconst(types::I64, ptr), ValTy::Ref));
                }
            }
            return Err(unsupported("class reference index (non-static-field)"));
        }
        // Indexing a non-composite (`(5)[0]`) yields null — box the scalar
        // and let the shared `read_index` return null, matching the interp.
        let arr = {
            let (v, base_ty) = self.local_value(base)?;
            self.coerce(v, base_ty, ValTy::Ref)?
        };
        let (i, it) = self.operand(idx)?;
        let ver = self.b.ins().iconst(types::I64, i64::from(self.lang.version));
        // A statically-integer index needs no heap box: `leek_index_int` takes
        // it as a raw `i64`. The result is still a handle, identical to the
        // boxed-index `leek_value_index` (whose class-ref / instance-method
        // special cases only apply to a string key, so an integer index falls
        // through to the same `read_index_versioned`).
        if it == ValTy::Int {
            let get = self.imports.rt("leek_index_int")?;
            let inst = self.b.ins().call(get, &[arr, i, ver]);
            return Ok((self.b.inst_results(inst)[0], ValTy::Ref));
        }
        let get = self.imports.rt("leek_value_index")?;
        // The index is itself a boxed value (so map string keys work too).
        let idx_h = self.coerce(i, it, ValTy::Ref)?;
        let inst = self.b.ins().call(get, &[arr, idx_h, ver]);
        Ok((self.b.inst_results(inst)[0], ValTy::Ref))
    }

    /// Box each operand to a `Ref` handle into a fresh stack array, returning
    /// `(ptr, count)`. Used for lambda captures and indirect-call args; the
    /// callee shim copies the handles out, so the array's frame lifetime is
    /// sufficient.
    fn build_ref_array(&mut self, ops: &[Operand]) -> Result<(Value, usize), NativeError> {
        self.build_ref_array_opt(ops, false)
    }

    /// Build a stack array of boxed `Ref` handles from `ops`. When `raw`,
    /// a cell local contributes its *cell handle* directly (not the peeled
    /// value) — used for `MakeLambda` captures so the closure shares the
    /// same `Value::Cell` `Rc` and observes the enclosing scope's writes.
    fn build_ref_array_opt(
        &mut self,
        ops: &[Operand],
        raw: bool,
    ) -> Result<(Value, usize), NativeError> {
        let n = ops.len();
        if n == 0 {
            return Ok((self.b.ins().iconst(types::I64, 0), 0));
        }
        let slot = self.b.create_sized_stack_slot(StackSlotData::new(
            StackSlotKind::ExplicitSlot,
            (n * 8) as u32,
            3,
        ));
        for (i, op) in ops.iter().enumerate() {
            let h = match op {
                Operand::Local(id) if raw && self.cell_locals.contains(id) => {
                    self.b.use_var(self.vars[id.0 as usize])
                }
                _ => {
                    let (v, t) = self.operand(op)?;
                    self.coerce(v, t, ValTy::Ref)?
                }
            };
            self.b.ins().stack_store(h, slot, (i * 8) as i32);
        }
        let ptr = self.b.ins().stack_addr(types::I64, slot, 0);
        Ok((ptr, n))
    }

    /// The runtime *value* of a local as a handle/scalar. For a cell local
    /// (lambda-shared storage) this peels the cell (`cell_get` → boxed
    /// `Ref`); any other local is its raw variable. Use this wherever a
    /// local's value is needed as a base / receiver / callee — NOT for the
    /// cell-write target or a raw capture, which want the cell handle.
    fn local_value(&mut self, id: LocalId) -> Result<(Value, ValTy), NativeError> {
        let v = self.b.use_var(self.vars[id.0 as usize]);
        if self.cell_locals.contains(&id) {
            let f = self.imports.rt("leek_cell_get")?;
            let inst = self.b.ins().call(f, &[v]);
            return Ok((self.b.inst_results(inst)[0], ValTy::Ref));
        }
        Ok((v, self.var_tys[id.0 as usize]))
    }

    /// Materialize a string literal's bytes *in-binary* (immediate byte stores
    /// to a stack slot) and box them at runtime via `leek_const_string`. Unlike
    /// baking a `box_string` handle as an absolute immediate, this is fully
    /// relocatable — the AOT binary builds the string in its own process.
    /// Materialise a compile-time string's bytes onto a stack slot, returning
    /// `(ptr, len)` as i64 values. Used both to box a string literal
    /// (`const_string`) and to pass a field name unboxed to `leek_field_get*`.
    fn const_str_bytes(&mut self, s: &str) -> (Value, Value) {
        let bytes = s.as_bytes();
        let len = bytes.len();
        let ptr = if len == 0 {
            self.b.ins().iconst(types::I64, 0)
        } else {
            let slot = self.b.create_sized_stack_slot(StackSlotData::new(
                StackSlotKind::ExplicitSlot,
                u32::try_from(len).unwrap_or(u32::MAX),
                0,
            ));
            for (i, &byte) in bytes.iter().enumerate() {
                let bv = self.b.ins().iconst(types::I8, i64::from(byte));
                self.b.ins().stack_store(bv, slot, i32::try_from(i).unwrap_or(0));
            }
            self.b.ins().stack_addr(types::I64, slot, 0)
        };
        let lenv = self.b.ins().iconst(types::I64, i64::try_from(len).unwrap_or(i64::MAX));
        (ptr, lenv)
    }

    fn const_string(&mut self, s: &str) -> Result<Value, NativeError> {
        let (ptr, lenv) = self.const_str_bytes(s);
        let f = self.imports.rt("leek_const_string")?;
        let inst = self.b.ins().call(f, &[ptr, lenv]);
        Ok(self.b.inst_results(inst)[0])
    }

    /// Read instance field / member `name` of `base`, returning a boxed handle,
    /// via `leek_field_get` — the field name is passed unboxed (`ptr`,`len`), so
    /// no `Value::String` key is allocated per read. Semantically identical to
    /// `leek_value_index(base, const_string(name))`.
    fn field_get_boxed(&mut self, base_h: Value, name: &str) -> Result<Value, NativeError> {
        let (ptr, lenv) = self.const_str_bytes(name);
        let ver = self.b.ins().iconst(types::I64, i64::from(self.lang.version));
        let f = self.imports.rt("leek_field_get")?;
        let inst = self.b.ins().call(f, &[base_h, ptr, lenv, ver]);
        Ok(self.b.inst_results(inst)[0])
    }

    /// Read instance field `name` of `base` directly into an unboxed scalar
    /// (`target` is `Int`/`Real`), via `leek_field_get_int`/`_real`. The value is
    /// `read_member(..).to_long()/.to_real()` — byte-identical to what the boxed
    /// read coerced to the scalar slot would produce, with neither key nor result
    /// boxed. The caller guarantees `base` is a known class instance.
    fn field_unboxed(&mut self, base: LocalId, name: &str, target: ValTy) -> Result<Value, NativeError> {
        let (bv, bt) = self.local_value(base)?;
        let base_h = self.coerce(bv, bt, ValTy::Ref)?;
        let (ptr, lenv) = self.const_str_bytes(name);
        let ver = self.b.ins().iconst(types::I64, i64::from(self.lang.version));
        let sym = if target == ValTy::Real {
            "leek_field_get_real"
        } else {
            "leek_field_get_int"
        };
        let f = self.imports.rt(sym)?;
        let inst = self.b.ins().call(f, &[base_h, ptr, lenv, ver]);
        Ok(self.b.inst_results(inst)[0])
    }

    fn operand(&mut self, op: &Operand) -> Result<(Value, ValTy), NativeError> {
        match op {
            Operand::Local(id) => {
                let ty = self.var_tys[id.0 as usize];
                let v = self.b.use_var(self.vars[id.0 as usize]);
                // A cell local's var holds the shared cell handle; a normal
                // read peels the current value out of it.
                if self.cell_locals.contains(id) {
                    let f = self.imports.rt("leek_cell_get")?;
                    let inst = self.b.ins().call(f, &[v]);
                    return Ok((self.b.inst_results(inst)[0], ValTy::Ref));
                }
                Ok((v, ty))
            }
            Operand::Const(Const::Int(n)) => Ok((self.b.ins().iconst(types::I64, *n), ValTy::Int)),
            Operand::Const(Const::Bool(x)) => {
                Ok((self.b.ins().iconst(types::I64, *x as i64), ValTy::Bool))
            }
            Operand::Const(Const::Real(bits)) => {
                Ok((self.b.ins().f64const(f64::from_bits(*bits)), ValTy::Real))
            }
            // A string literal: materialize its bytes in-binary and box them at
            // runtime (relocatable — see `const_string`).
            Operand::Const(Const::String(s)) => Ok((self.const_string(s)?, ValTy::Ref)),
            // A null literal: build a fresh `Null` handle at runtime rather than
            // baking a pointer to the compiler process's null singleton.
            Operand::Const(Const::Null) => {
                let f = self.imports.rt("leek_box_null")?;
                let inst = self.b.ins().call(f, &[]);
                Ok((self.b.inst_results(inst)[0], ValTy::Ref))
            }
        }
    }

    /// Convert `v` (kind `from`) to kind `to`, matching Leekscript's
    /// `coerce_to_type`: int/bool widen to real, and real narrows to int
    /// by truncation toward zero (`(long) r` / Rust `r as i64`, which
    /// saturates — `fcvt_to_sint_sat`). Real → bool isn't a real
    /// coercion in Leekscript (it would keep the real), so it bails.
    fn coerce(&mut self, v: Value, from: ValTy, to: ValTy) -> Result<Value, NativeError> {
        match (from, to) {
            (a, b) if a == b => Ok(v),
            // Box a scalar into a handle.
            (_, ValTy::Ref) => {
                let sym = match from {
                    ValTy::Int => "leek_box_int",
                    ValTy::Bool => "leek_box_bool",
                    ValTy::Real => "leek_box_real",
                    ValTy::Ref => unreachable!(),
                };
                let fref = self.imports.rt(sym)?;
                let inst = self.b.ins().call(fref, &[v]);
                Ok(self.b.inst_results(inst)[0])
            }
            // Unbox a handle into a scalar.
            (ValTy::Ref, _) => {
                let sym = match to {
                    ValTy::Int => "leek_unbox_int",
                    ValTy::Bool => "leek_unbox_bool",
                    ValTy::Real => "leek_unbox_real",
                    ValTy::Ref => unreachable!(),
                };
                let fref = self.imports.rt(sym)?;
                let inst = self.b.ins().call(fref, &[v]);
                Ok(self.b.inst_results(inst)[0])
            }
            (ValTy::Int | ValTy::Bool, ValTy::Real) => Ok(self.b.ins().fcvt_from_sint(types::F64, v)),
            // int↔bool share the i64 repr.
            (ValTy::Int, ValTy::Bool) | (ValTy::Bool, ValTy::Int) => Ok(v),
            (ValTy::Real, ValTy::Int) => Ok(self.b.ins().fcvt_to_sint_sat(types::I64, v)),
            (ValTy::Real, ValTy::Bool) => Err(unsupported("real → bool coercion")),
            _ => Ok(v),
        }
    }

    /// True if an operand's static kind is an unboxed `integer` — an `Int`
    /// local or an integer literal. Used to gate the unboxed-index array reads
    /// (`leek_index_int` / `leek_array_get_*`), which take the index as a raw
    /// `i64`.
    fn operand_int_kind(&self, op: &Operand) -> bool {
        match op {
            Operand::Local(id) => self.var_tys[id.0 as usize] == ValTy::Int,
            Operand::Const(Const::Int(_)) => true,
            _ => false,
        }
    }

    /// Read `base[idx]` directly into an unboxed scalar (`target` is `Int` or
    /// `Real`), for an indexing whose result flows into a scalar-typed slot.
    /// `read_index_versioned(..).to_long()/.to_real()` is exactly what the
    /// destination's `coerce(Ref → Int/Real)` (i.e. `leek_unbox_int/real`)
    /// would compute on the boxed read, so the value is byte-identical — but
    /// neither the index nor the result is ever boxed. The caller guarantees
    /// `idx` is a statically-integer operand and `base` is not a class-ref.
    fn index_unboxed(
        &mut self,
        base: LocalId,
        idx: &Operand,
        target: ValTy,
    ) -> Result<Value, NativeError> {
        let arr = {
            let (v, base_ty) = self.local_value(base)?;
            self.coerce(v, base_ty, ValTy::Ref)?
        };
        let (i, _it) = self.operand(idx)?;
        let ver = self.b.ins().iconst(types::I64, i64::from(self.lang.version));
        let sym = if target == ValTy::Real {
            "leek_array_get_real"
        } else {
            "leek_array_get_int"
        };
        let f = self.imports.rt(sym)?;
        let inst = self.b.ins().call(f, &[arr, i, ver]);
        Ok(self.b.inst_results(inst)[0])
    }

    /// True if an operand's static kind is a boxed `Ref` (a dynamic value) —
    /// a `Ref`/cell local, or a null/string constant.
    fn operand_is_ref(&self, op: &Operand) -> bool {
        match op {
            Operand::Local(id) => self.var_tys[id.0 as usize] == ValTy::Ref,
            Operand::Const(Const::Null | Const::String(_)) => true,
            _ => false,
        }
    }

    /// A dummy value of kind `ty` for an omitted defaulted call argument — the
    /// callee overwrites it (running the param's `default_init`) before the
    /// body runs, so the value is never observed. A `Ref` uses a valid boxed
    /// null handle (not a 0 pointer) in case of a stray read.
    fn placeholder(&mut self, ty: ValTy) -> Value {
        match ty {
            ValTy::Real => self.b.ins().f64const(0.0),
            ValTy::Ref => {
                // `leek_box_null` is always declared (a composite shim).
                let f = self
                    .imports
                    .rt("leek_box_null")
                    .expect("leek_box_null shim declared");
                let inst = self.b.ins().call(f, &[]);
                self.b.inst_results(inst)[0]
            }
            _ => self.b.ins().iconst(types::I64, 0),
        }
    }

    /// `value != 0` as an i64 condition for `brif`.
    fn truthy(&mut self, v: Value, ty: ValTy) -> Value {
        match ty {
            ValTy::Real => {
                let zero = self.b.ins().f64const(0.0);
                self.b.ins().fcmp(FloatCC::NotEqual, v, zero)
            }
            _ => v,
        }
    }

    /// Truthiness of `v` as a 1-bit value (`value != 0`).
    fn emit_i1(&mut self, v: Value, ty: ValTy) -> Value {
        if ty == ValTy::Real {
            let zero = self.b.ins().f64const(0.0);
            self.b.ins().fcmp(FloatCC::NotEqual, v, zero)
        } else {
            let zero = self.b.ins().iconst(types::I64, 0);
            self.b.ins().icmp(IntCC::NotEqual, v, zero)
        }
    }

    fn binary(&mut self, op: BinOp, l: &Operand, r: &Operand) -> Result<(Value, ValTy), NativeError> {
        // Charge the binary op's cost (interp charges this for every `Binary`
        // rvalue, before evaluating — so it applies even to the v1 div-by-zero
        // early return below). String-concat's per-char surcharge is handled in
        // the boxed `leek_value_binop` path, which measures the result at
        // runtime; no `.ops` corpus case exercises string concat.
        self.charge(op.op_cost())?;
        // v1 real division by a statically-zero divisor yields `null`, not
        // `±∞` — produce a boxed-null `Ref` (so `8 / 0 === null` is true and
        // `0 / 0 === NaN` is false), rather than emitting an infinity v1
        // never produces.
        if self.lang.version == 1 && matches!(op, BinOp::Div) && is_const_zero(r) {
            // Use the compile-time leaked null handle (an `iconst`), not the
            // `leek_box_null` shim — so this works even in an otherwise
            // composite-free function.
            let f = self.imports.rt("leek_box_null")?;
            let inst = self.b.ins().call(f, &[]);
            return Ok((self.b.inst_results(inst)[0], ValTy::Ref));
        }
        let (a, lt) = self.operand(l)?;
        let (b, rt) = self.operand(r)?;

        // Arithmetic / comparison involving a dynamically-typed (boxed)
        // operand — e.g. an array element — dispatches at runtime through
        // the shared `apply_binary`, matching the interpreter exactly.
        if lt == ValTy::Ref || rt == ValTy::Ref {
            let code = self.b.ins().iconst(types::I64, crate::runtime::binop_code(op));
            let ver = self.b.ins().iconst(types::I64, i64::from(self.lang.version));
            // Fast path: when exactly one operand is dynamic (`Ref`) and the
            // other is a statically-`integer`/`real` value (literal OR a typed
            // local), pass that scalar by value to a `_ci{r,l}` / `_cr{r,l}`
            // shim instead of heap-boxing it on every evaluation (`arr[i] - 1`,
            // `boxed + x`, …). The shim rebuilds the identical `Value::Int` /
            // `Value::Real` on the stack, so the result is unchanged — only the
            // scalar's per-op allocation is removed. `Bool` keeps the boxed
            // path (its `Value::Bool` dispatch differs from an int rebuild).
            let res = if lt == ValTy::Ref && rt == ValTy::Int {
                let a = self.coerce(a, lt, ValTy::Ref)?;
                let cir = self.imports.rt("leek_value_binop_cir")?;
                let inst = self.b.ins().call(cir, &[code, a, b, ver]);
                self.b.inst_results(inst)[0]
            } else if lt == ValTy::Ref && rt == ValTy::Real {
                let a = self.coerce(a, lt, ValTy::Ref)?;
                let crr = self.imports.rt("leek_value_binop_crr")?;
                let inst = self.b.ins().call(crr, &[code, a, b, ver]);
                self.b.inst_results(inst)[0]
            } else if rt == ValTy::Ref && lt == ValTy::Int {
                let b = self.coerce(b, rt, ValTy::Ref)?;
                let cil = self.imports.rt("leek_value_binop_cil")?;
                let inst = self.b.ins().call(cil, &[code, a, b, ver]);
                self.b.inst_results(inst)[0]
            } else if rt == ValTy::Ref && lt == ValTy::Real {
                let b = self.coerce(b, rt, ValTy::Ref)?;
                let crl = self.imports.rt("leek_value_binop_crl")?;
                let inst = self.b.ins().call(crl, &[code, a, b, ver]);
                self.b.inst_results(inst)[0]
            } else {
                let binop = self.imports.rt("leek_value_binop")?;
                let a = self.coerce(a, lt, ValTy::Ref)?;
                let b = self.coerce(b, rt, ValTy::Ref)?;
                let inst = self.b.ins().call(binop, &[code, a, b, ver]);
                self.b.inst_results(inst)[0]
            };
            // String concat charges 1 op per result char on top of the `Add`
            // node cost (interp `exec.rs`). The shim no-ops for non-string
            // results (e.g. `array + array`).
            if matches!(op, BinOp::Add) {
                let concat = self.imports.rt("leek_charge_concat")?;
                self.b.ins().call(concat, &[res]);
            }
            return Ok((res, ValTy::Ref));
        }

        // `xor` is logical xor of truthiness in every version — works on
        // any scalar mix.
        if matches!(op, BinOp::Xor) {
            let ba = self.emit_i1(a, lt);
            let bb = self.emit_i1(b, rt);
            let x = self.b.ins().bxor(ba, bb);
            return Ok((self.b.ins().uextend(types::I64, x), ValTy::Bool));
        }

        // Equality across an int and a bool (the only non-equal,
        // non-real scalar pairing). Two behaviors:
        //   * `===` / `!==` (and `==` / `!=` in v4) are type-sensitive:
        //     differing types ⇒ structurally false / true.
        //   * `==` / `!=` in v1–v3 coerce by *truthiness*
        //     (`true == 12` is true, `false == 0` is true).
        if matches!(
            op,
            BinOp::Eq | BinOp::Ne | BinOp::IdentityEq | BinOp::IdentityNe
        ) && lt != rt
            && lt != ValTy::Real
            && rt != ValTy::Real
        {
            let identity = matches!(op, BinOp::IdentityEq | BinOp::IdentityNe);
            let want_eq = matches!(op, BinOp::Eq | BinOp::IdentityEq);
            if identity || self.lang.version >= 4 {
                let c = self.b.ins().iconst(types::I64, i64::from(!want_eq));
                return Ok((c, ValTy::Bool));
            }
            // v1–v3 truthiness comparison.
            let ba = self.emit_i1(a, lt);
            let bb = self.emit_i1(b, rt);
            let cc = if want_eq {
                IntCC::Equal
            } else {
                IntCC::NotEqual
            };
            let bit = self.b.ins().icmp(cc, ba, bb);
            return Ok((self.b.ins().uextend(types::I64, bit), ValTy::Bool));
        }

        // `^=` compound assignment is POWER-assign in v1 (`x ^= n` ≡
        // `x = x ** n`) and XOR-assign in v2+ (`x = x ^ n`). The v1 form
        // shares the `**` path below; the v2+ form is integer bitwise xor
        // (handled in the integer match arm). Standalone `^` is `BitXor`.
        let pow_like = matches!(op, BinOp::Pow)
            || (matches!(op, BinOp::CompoundXor) && self.lang.version <= 1);

        // `**` power. Real if either side is real; otherwise integer
        // power, but only when the exponent is a constant in `[0, 64)`
        // (the interp's int-pow range — outside it the result kind
        // becomes data-dependent, so we skip).
        if pow_like {
            if lt == ValTy::Real || rt == ValTy::Real {
                let fref = self
                    .imports
                    .pow_real
                    .ok_or_else(|| unsupported("pow import not declared"))?;
                let a = self.coerce(a, lt, ValTy::Real)?;
                let b = self.coerce(b, rt, ValTy::Real)?;
                let inst = self.b.ins().call(fref, &[a, b]);
                return Ok((self.b.inst_results(inst)[0], ValTy::Real));
            }
            match const_pow_exp(r) {
                Some(e) if (0..64).contains(&e) => {
                    let fref = self
                        .imports
                        .pow_int
                        .ok_or_else(|| unsupported("ipow import not declared"))?;
                    let inst = self.b.ins().call(fref, &[a, b]);
                    return Ok((self.b.inst_results(inst)[0], ValTy::Int));
                }
                _ => return Err(unsupported("integer ** with non-constant/large exponent")),
            }
        }

        // Bitwise / shift ops are integer-only: a real operand truncates to
        // an integer (matching the interpreter), so they never take the real
        // path even when an operand is typed `real`.
        let bitwise = matches!(
            op,
            BinOp::BitAnd
                | BinOp::BitOr
                | BinOp::BitXor
                | BinOp::CompoundXor
                | BinOp::ShiftL
                | BinOp::ShiftR
                | BinOp::UShiftR
        );
        let real = !bitwise && (lt == ValTy::Real || rt == ValTy::Real || matches!(op, BinOp::Div));

        if real {
            let a = self.coerce(a, lt, ValTy::Real)?;
            let b = self.coerce(b, rt, ValTy::Real)?;
            let ins = self.b.ins();
            let (v, ty) = match op {
                BinOp::Add => (ins.fadd(a, b), ValTy::Real),
                BinOp::Sub => (ins.fsub(a, b), ValTy::Real),
                BinOp::Mul => (ins.fmul(a, b), ValTy::Real),
                BinOp::Div => (ins.fdiv(a, b), ValTy::Real),
                // `===` / `!==` on numbers compares numerically, like
                // `==` / `!=` (`1 === 1.0` is true).
                BinOp::Eq | BinOp::IdentityEq => return Ok(self.fcmp(FloatCC::Equal, a, b)),
                BinOp::Ne | BinOp::IdentityNe => return Ok(self.fcmp(FloatCC::NotEqual, a, b)),
                BinOp::Lt => return Ok(self.fcmp(FloatCC::LessThan, a, b)),
                BinOp::Le => return Ok(self.fcmp(FloatCC::LessThanOrEqual, a, b)),
                BinOp::Gt => return Ok(self.fcmp(FloatCC::GreaterThan, a, b)),
                BinOp::Ge => return Ok(self.fcmp(FloatCC::GreaterThanOrEqual, a, b)),
                other => return Err(unsupported(format!("real binary op {other:?}"))),
            };
            return Ok((v, ty));
        }

        // Integer path: coerce both operands to `Int` (a no-op when already
        // integer/bool; truncates a `real` operand for the bitwise ops routed
        // here).
        let a = self.coerce(a, lt, ValTy::Int)?;
        let b = self.coerce(b, rt, ValTy::Int)?;
        let ins = self.b.ins();
        let (v, ty) = match op {
            BinOp::Add => (ins.iadd(a, b), ValTy::Int),
            BinOp::Sub => (ins.isub(a, b), ValTy::Int),
            BinOp::Mul => (ins.imul(a, b), ValTy::Int),
            BinOp::IntDiv => (ins.sdiv(a, b), ValTy::Int),
            BinOp::Mod => (ins.srem(a, b), ValTy::Int),
            BinOp::BitAnd => (ins.band(a, b), ValTy::Int),
            BinOp::BitOr => (ins.bor(a, b), ValTy::Int),
            BinOp::BitXor => (ins.bxor(a, b), ValTy::Int),
            // v2+ `^=` xor-assign (v1 power-assign took the `**` path above).
            BinOp::CompoundXor => (ins.bxor(a, b), ValTy::Int),
            BinOp::ShiftL => (ins.ishl(a, b), ValTy::Int),
            BinOp::ShiftR => (ins.sshr(a, b), ValTy::Int),
            BinOp::UShiftR => (ins.ushr(a, b), ValTy::Int),
            BinOp::Eq | BinOp::IdentityEq => return Ok(self.icmp(IntCC::Equal, a, b)),
            BinOp::Ne | BinOp::IdentityNe => return Ok(self.icmp(IntCC::NotEqual, a, b)),
            BinOp::Lt => return Ok(self.icmp(IntCC::SignedLessThan, a, b)),
            BinOp::Le => return Ok(self.icmp(IntCC::SignedLessThanOrEqual, a, b)),
            BinOp::Gt => return Ok(self.icmp(IntCC::SignedGreaterThan, a, b)),
            BinOp::Ge => return Ok(self.icmp(IntCC::SignedGreaterThanOrEqual, a, b)),
            other => return Err(unsupported(format!("binary op {other:?}"))),
        };
        Ok((v, ty))
    }

    fn icmp(&mut self, cc: IntCC, a: Value, b: Value) -> (Value, ValTy) {
        let bit = self.b.ins().icmp(cc, a, b);
        (self.b.ins().uextend(types::I64, bit), ValTy::Bool)
    }

    fn fcmp(&mut self, cc: FloatCC, a: Value, b: Value) -> (Value, ValTy) {
        let bit = self.b.ins().fcmp(cc, a, b);
        (self.b.ins().uextend(types::I64, bit), ValTy::Bool)
    }

    fn unary(&mut self, op: UnOp, x: &Operand) -> Result<(Value, ValTy), NativeError> {
        let (v, ty) = self.operand(x)?;
        if ty == ValTy::Ref {
            match op {
                // `!dynamic` uses the shared truthiness shim.
                UnOp::Not => {
                    let f = self.imports.rt("leek_truthy")?;
                    let inst = self.b.ins().call(f, &[v]);
                    let t = self.b.inst_results(inst)[0];
                    let zero = self.b.ins().iconst(types::I64, 0);
                    let bit = self.b.ins().icmp(IntCC::Equal, t, zero);
                    return Ok((self.b.ins().uextend(types::I64, bit), ValTy::Bool));
                }
                // `-dynamic` / `~dynamic` go through the shared unary shim,
                // returning a new boxed value.
                UnOp::Neg | UnOp::BitNot => {
                    let code = i64::from(op != UnOp::Neg);
                    let f = self.imports.rt("leek_value_unary")?;
                    let code_v = self.b.ins().iconst(types::I64, code);
                    let inst = self.b.ins().call(f, &[code_v, v]);
                    return Ok((self.b.inst_results(inst)[0], ValTy::Ref));
                }
                _ => return Err(unsupported("unary operator on dynamic (boxed) value")),
            }
        }
        match op {
            UnOp::Neg if ty == ValTy::Real => Ok((self.b.ins().fneg(v), ValTy::Real)),
            UnOp::Neg => Ok((self.b.ins().ineg(v), ValTy::Int)),
            UnOp::BitNot if ty == ValTy::Real => Err(unsupported("bitnot on real")),
            UnOp::BitNot => Ok((self.b.ins().bnot(v), ValTy::Int)),
            UnOp::Pos | UnOp::Ref => Ok((v, ty)),
            UnOp::Not => {
                let bit = if ty == ValTy::Real {
                    let zero = self.b.ins().f64const(0.0);
                    self.b.ins().fcmp(FloatCC::Equal, v, zero)
                } else {
                    let zero = self.b.ins().iconst(types::I64, 0);
                    self.b.ins().icmp(IntCC::Equal, v, zero)
                };
                Ok((self.b.ins().uextend(types::I64, bit), ValTy::Bool))
            }
        }
    }
}

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
                && !program.classes.iter().any(|o| o.parent_def == Some(c.def_id));
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
        // `abs` otherwise keeps the argument kind (real → real, else int).
        "abs" => match call.args.first().and_then(|op| operand_ty(op, tys)) {
            Some(ValTy::Real) => Some(ValTy::Real),
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
            && (call.args.iter().any(|op| operand_ty(op, tys) == Some(ValTy::Ref))
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
    // A string OR null literal boxes into a handle and pulls in the composite
    // shims (box/unbox, and `leek_truthy` for `!null` / dynamic branches).
    let is_str = |o: &Operand| {
        matches!(o, Operand::Const(Const::String(_) | Const::Null))
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
    assigned.extend(main.blocks.iter().flat_map(|b| &b.statements).filter_map(|s| {
        match s {
            Statement::Assign(Place::Local(id), _) => Some(*id),
            Statement::Call {
                dest: Some(Place::Local(id)),
                ..
            } => Some(*id),
            _ => None,
        }
    }));

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
        Operand::Const(Const::String(_) | Const::Null) => Some(ValTy::Ref),
    }
}

/// The pinned value-kind for an explicit numeric declared type, if any.
/// `integer`/`real` (and their `?`-nullable forms) pin; everything else
/// (including `boolean`) is left to assignment inference.
fn pinned_valty(t: &Type) -> Option<ValTy> {
    match t {
        Type::Integer => Some(ValTy::Int),
        Type::Real => Some(ValTy::Real),
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
                Statement::Assign(Place::Local(id),
Rvalue::Use(Operand::Const(c)) | Rvalue::UseFresh(Operand::Const(c)))
                    if id == t =>
                {
                    Some(c.clone())
                }
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
fn const_eval_default(
    f: &MirFunction,
    param: LocalId,
    version: u8,
) -> Option<leek_runtime::Value> {
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
            for o in items {
                s.insert(const_eval_operand(o, scratch, version)?);
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
