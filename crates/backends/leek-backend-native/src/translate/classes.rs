//! Class/object resolution passes: statically classifying which locals hold
//! instances, class references, or `super` bindings, and resolving static and
//! instance method/field accesses to their defining class at compile time.
//! Like `analysis`, these are pure queries over the `MirProgram`; the
//! Cranelift emission lives in the parent module's `Tx`.

use super::{
    Callee, CastKind, Const, DefId, HashMap, HashSet, LocalDecl, LocalId, MirFunction, MirProgram,
    Operand, Place, Rvalue, Statement, Type,
};

/// True if the program assigns to a file-level global named `name` — used
/// to detect a user definition shadowing a builtin (`cos = function(){…}`),
/// which is referenced via `Place::Global`/`Rvalue::GlobalRef` rather than
/// being listed in `MirProgram::globals`.
pub(super) fn program_writes_global(program: &MirProgram, name: &str) -> bool {
    if program.globals.iter().any(|g| g.name == name) {
        return true;
    }
    program.functions.iter().any(|f| {
        f.blocks.iter().any(|b| {
            b.statements
                .iter()
                .any(|s| matches!(s, Statement::Assign(Place::Global(_, n), _) if n == name))
        })
    })
}

/// The class a local statically holds an instance of, if any. An untyped
/// `var d = new Dog()` carries the class in `inferred_ty` (its declared
/// `ty` stays `Any`), so both are consulted.
pub(super) fn instance_class(decl: &LocalDecl) -> Option<&str> {
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
pub(super) fn object_locals(f: &MirFunction) -> HashSet<LocalId> {
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
                acc.entry(*id)
                    .and_modify(|cur| *cur &= is_obj)
                    .or_insert(is_obj);
            }
        }
        let next: HashSet<LocalId> = acc
            .into_iter()
            .filter_map(|(k, v)| v.then_some(k))
            .collect();
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
pub(super) fn object_field_srcs(f: &MirFunction) -> HashMap<LocalId, HashMap<String, Operand>> {
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

pub(super) fn new_class_locals(f: &MirFunction) -> HashMap<LocalId, String> {
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
        let next: HashMap<LocalId, String> = acc
            .into_iter()
            .filter_map(|(k, v)| v.map(|c| (k, c)))
            .collect();
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
pub(super) fn aliased_class_locals(f: &MirFunction) -> HashMap<LocalId, String> {
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
pub(super) fn classref_locals(f: &MirFunction) -> HashMap<LocalId, String> {
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
        let next: HashMap<LocalId, String> = acc
            .into_iter()
            .filter_map(|(k, v)| v.map(|c| (k, c)))
            .collect();
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
pub(super) fn super_locals(f: &MirFunction) -> HashMap<LocalId, (LocalId, String)> {
    let mut map = HashMap::new();
    for b in &f.blocks {
        for s in &b.statements {
            if let Statement::Assign(Place::Local(id), Rvalue::MakeSuper { this, parent_class }) = s
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
pub(super) fn resolve_static_method(
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
            if let Some(m) = c
                .methods
                .iter()
                .find(|m| m.is_static && m.name == method && (!want_arity || m.user_arity == argc))
            {
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
pub(super) fn class_reflect(
    program: &MirProgram,
    class_name: &str,
    member: &str,
) -> Option<leek_runtime::Value> {
    use leek_runtime::Value as RtValue;
    use std::cell::RefCell;
    use std::rc::Rc;
    let str_arr = |names: Vec<String>| {
        RtValue::Array(Rc::new(RefCell::new(
            names
                .into_iter()
                .map(|n| RtValue::String(Rc::new(n)))
                .collect(),
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
            c.methods
                .iter()
                .filter(|m| !m.is_static)
                .map(|m| m.name.clone())
                .collect()
        }))),
        "static_methods" | "staticMethods" => Some(str_arr(walk(&|c| {
            c.methods
                .iter()
                .filter(|m| m.is_static)
                .map(|m| m.name.clone())
                .collect()
        }))),
        "constructors" => {
            let n = program
                .class_by_name(class_name)
                .map_or(0, |c| c.constructors.len());
            Some(RtValue::Array(Rc::new(RefCell::new(vec![
                RtValue::Int(0);
                n
            ]))))
        }
        _ => None,
    }
}

/// Resolve a *static* field by name, walking `class_name` and its parents.
/// Returns the declaring class's `DefId` (the storage key) and the field.
pub(super) fn resolve_static_field<'a>(
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
pub(super) fn static_field_accesses(
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
pub fn function_ref_info(program: &MirProgram, reachable_indices: &[usize]) -> HashMap<u32, usize> {
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
pub(super) fn resolve_static_method_value(
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
pub(super) fn resolve_instance_method_value(
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
pub(super) fn static_method_value_refs(
    program: &MirProgram,
    f: &MirFunction,
) -> Vec<(DefId, usize)> {
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
pub(super) fn receiver_class<'a>(
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
pub(super) fn builtin_ancestor(program: &MirProgram, c: &leek_mir::ir::MirClass) -> Option<String> {
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

pub(super) fn class_extends_builtin(program: &MirProgram, c: &leek_mir::ir::MirClass) -> bool {
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
