//! MIR analysis passes that classify functions, lambdas, and by-reference
//! parameters ahead of translation: lambda-body detection, by-ref/cell
//! semantics, higher-order-function flow, and virtual/static method dispatch
//! resolution. These are pure queries over the `MirProgram`; the actual
//! Cranelift emission lives in the parent module's `Tx`.

use super::classes::{aliased_class_locals, classref_locals, new_class_locals, receiver_class};
use super::{
    Callee, Const, DefId, HashMap, HashSet, LocalId, MirFunction, MirProgram, Operand, Place,
    Rvalue, SetElem, Statement, Terminator,
};

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
pub(super) fn byref_param_pure_local(f: &MirFunction, id: LocalId) -> bool {
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
pub(super) fn lambda_passed_to_hof(program: &MirProgram, lambda_fi: usize) -> bool {
    for f in &program.functions {
        // Locals holding this lambda value (its `MakeLambda` dest + `Use` copies).
        let mut holders: HashSet<LocalId> = HashSet::new();
        loop {
            let before = holders.len();
            for s in f.blocks.iter().flat_map(|b| &b.statements) {
                if let Statement::Assign(Place::Local(d), rv) = s {
                    let holds = match rv {
                        Rvalue::MakeLambda { function_idx, .. } => *function_idx == lambda_fi,
                        Rvalue::Use(Operand::Local(src))
                        | Rvalue::UseFresh(Operand::Local(src)) => holders.contains(src),
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
                && call
                    .args
                    .iter()
                    .any(|a| matches!(a, Operand::Local(l) if holders.contains(l)))
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
pub(super) fn noop_byref_params(program: &MirProgram, fi: usize, version: u8) -> HashSet<LocalId> {
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
pub(super) fn class_descends_from(
    program: &MirProgram,
    child_def: DefId,
    ancestor_def: DefId,
) -> bool {
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
pub(super) fn virtual_method_targets(
    program: &MirProgram,
    f: &MirFunction,
) -> Vec<(usize, String, String)> {
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
            let Some(cls_name) = receiver_class(&f.locals, &new_classes, &aliased, *receiver)
            else {
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
pub(super) fn index_method_targets(
    program: &MirProgram,
    f: &MirFunction,
) -> Vec<(usize, String, String)> {
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
pub(super) fn dynamic_method_targets(
    program: &MirProgram,
    f: &MirFunction,
) -> Vec<(usize, String, String)> {
    let new_classes = new_class_locals(f);
    let aliased = aliased_class_locals(f);
    let classrefs = classref_locals(f);
    let mut out = Vec::new();
    for b in &f.blocks {
        for s in &b.statements {
            let Statement::Call { call, .. } = s else {
                continue;
            };
            let Callee::Method { receiver, method } = &call.callee else {
                continue;
            };
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
) -> (HashMap<u32, HashMap<String, usize>>, HashSet<usize>) {
    // Keyed by the class's `DefId` (a `u32`, matching the runtime instance's
    // `class`) → method name → function index, so the per-call resolve in
    // `leek_call_method` is a `(u32, &str)` lookup with no string clones/hashes.
    let mut table: HashMap<u32, HashMap<String, usize>> = HashMap::new();
    let mut set = HashSet::new();
    for &fi in reachable_indices {
        for (fidx, cls, name) in index_method_targets(program, &program.functions[fi])
            .into_iter()
            .chain(virtual_method_targets(program, &program.functions[fi]))
            .chain(dynamic_method_targets(program, &program.functions[fi]))
        {
            if let Some(c) = program.class_by_name(&cls) {
                table.entry(c.def_id.0).or_default().insert(name, fidx);
            }
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
pub(super) fn indirect_arg_cell_locals(f: &MirFunction) -> HashSet<LocalId> {
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
pub(super) fn byref_lambda_param_escapes(f: &MirFunction, id: LocalId) -> bool {
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
pub(super) fn byref_param_needs_cell_v1(f: &MirFunction, id: LocalId) -> bool {
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
pub(super) fn lambda_capture_count(program: &MirProgram, fi: usize) -> Option<usize> {
    for f in &program.functions {
        for s in f.blocks.iter().flat_map(|b| &b.statements) {
            if let Statement::Assign(
                _,
                Rvalue::MakeLambda {
                    function_idx,
                    captures,
                },
            ) = s
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
pub(super) fn byref_capture_only_returned(f: &MirFunction, id: LocalId) -> bool {
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
pub(super) fn byref_param_captured_by_lambda(f: &MirFunction, p: LocalId) -> bool {
    f.blocks.iter().flat_map(|b| &b.statements).any(|s| {
        matches!(s, Statement::Assign(_, Rvalue::MakeLambda { captures, .. })
            if captures.iter().any(|o| matches!(o, Operand::Local(l) if *l == p)))
    })
}

/// True if by-ref param `p` is handed back via `return @p` (a `Return`
/// terminator whose operand is `p`, marked `is_by_ref`). The returned value is
/// the shared `Value::Cell`, so the caller's `var y = f(x)` aliases `p`'s
/// storage — propagating later in-place mutations of `y` back through the cell.
pub(super) fn byref_param_returned(f: &MirFunction, p: LocalId) -> bool {
    f.blocks
        .iter()
        .any(|b| matches!(&b.terminator, Terminator::Return(Some(Operand::Local(l))) if *l == p))
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
pub(super) fn byref_param_escape_threadable(f: &MirFunction, p: LocalId) -> bool {
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
pub(super) fn byref_captured_arg_cell_locals(
    f: &MirFunction,
    program: &MirProgram,
) -> HashSet<LocalId> {
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
pub(super) fn local_reassigned(f: &MirFunction, id: LocalId) -> bool {
    f.blocks
        .iter()
        .flat_map(|b| &b.statements)
        .any(|s| matches!(s, Statement::Assign(Place::Local(l), _) if *l == id))
}

/// True if by-ref param `p` of `f` *aliases onward*: it is passed (by name) to a
/// **by-ref** parameter of a user-function callee, so its alias chain must be
/// carried by a shared cell too. By-ref-ness is a static property, so this needs
/// no fixpoint — each level of an `f → g → h` chain detects the next locally.
pub(super) fn byref_aliases_onward(f: &MirFunction, p: LocalId, program: &MirProgram) -> bool {
    for s in f.blocks.iter().flat_map(|b| &b.statements) {
        let Statement::Call { call, .. } = s else {
            continue;
        };
        let Callee::Function(d) = &call.callee else {
            continue;
        };
        let Some(g) = program.function(*d) else {
            continue;
        };
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
pub(super) fn byref_cell_params(f: &MirFunction, program: &MirProgram) -> HashSet<LocalId> {
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
pub(super) fn byref_arg_cell_locals(f: &MirFunction, program: &MirProgram) -> HashSet<LocalId> {
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

pub(super) fn op_mentions(o: &Operand, l: LocalId) -> bool {
    matches!(o, Operand::Local(x) if *x == l)
}

pub(super) fn slice_bounds_mentions(b: &leek_mir::ir::SliceBounds, l: LocalId) -> bool {
    [&b.start, &b.end, &b.step]
        .into_iter()
        .flatten()
        .any(|o| op_mentions(o, l))
}

pub(super) fn place_mentions(p: &Place, l: LocalId) -> bool {
    match p {
        Place::Local(x) | Place::Field(x, _) => *x == l,
        Place::Index(x, idx) => *x == l || op_mentions(idx, l),
        Place::Slice(x, b) => *x == l || slice_bounds_mentions(b, l),
        Place::LambdaCapture { lambda, .. } => *lambda == l,
        Place::Global(..) => false,
    }
}

pub(super) fn rvalue_mentions(r: &Rvalue, l: LocalId) -> bool {
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
        Rvalue::Array(ops) => ops.iter().any(|o| op_mentions(o, l)),
        Rvalue::Set(es) => es
            .iter()
            .flat_map(SetElem::operands)
            .any(|o| op_mentions(o, l)),
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

pub(super) fn callee_mentions(c: &Callee, l: LocalId) -> bool {
    match c {
        Callee::Method { receiver, .. } => *receiver == l,
        Callee::Indirect(x) => *x == l,
        Callee::SuperConstructor { this, .. } => *this == l,
        Callee::Function(_) | Callee::Builtin(_) => false,
    }
}

pub(super) fn stmt_mentions(s: &Statement, l: LocalId) -> bool {
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

pub(super) fn term_mentions(t: &Terminator, l: LocalId) -> bool {
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
pub(super) fn lambda_local_hof_only(f: &MirFunction, l: LocalId) -> bool {
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
                Statement::Call { dest, call } if matches!(&call.callee, Callee::Builtin(n) if WRITEBACK_HOFS.contains(&n.as_str())) => {
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
pub(super) fn lambda_is_hof_only(
    program: &MirProgram,
    reachable: &[usize],
    lambda_fi: usize,
) -> bool {
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
pub(super) fn funcref_local_hof_only(f: &MirFunction, l: LocalId, def_id: DefId) -> bool {
    for b in &f.blocks {
        for s in &b.statements {
            match s {
                Statement::Assign(Place::Local(d), Rvalue::FunctionRef(dd))
                    if *d == l && *dd == def_id => {}
                Statement::Call { dest, call } if matches!(&call.callee, Callee::Builtin(n) if WRITEBACK_HOFS.contains(&n.as_str())) => {
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
pub(super) fn named_fn_is_hof_only(
    program: &MirProgram,
    reachable: &[usize],
    def_id: DefId,
) -> bool {
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
                Statement::Call { call, .. } if matches!(&call.callee, Callee::Function(d) if *d == def_id) =>
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
pub(super) fn byref_cells_threadable(
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
        let has_byref_param = f.params.iter().any(|p| f.locals[p.0 as usize].is_by_ref);
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
                let cell_threaded = version <= 1 && is_lambda && !byref_lambda_param_escapes(f, id);
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
