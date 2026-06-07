//! MIR interpreter — class.

use std::cell::RefCell;
use std::rc::Rc;

use leek_hir::DefId;
use leek_mir::Visibility;
use leek_types::Type;

use crate::value::{Function as FnValue, Value};

use super::value::{walk_class_chain_parent, construct_builtin_class, coerce_to_type, builtin_class_static, read_field, legacy_map_key, read_index};
use super::{Interpreter, Outcome};

impl Interpreter<'_> {
    pub(crate) fn construct_user_class(
        &mut self,
        class_name: &str,
        args: Vec<Value>,
    ) -> Result<Value, Outcome> {
        let idx = self
            .class_by_name
            .get(class_name)
            .copied()
            .ok_or_else(|| Outcome::Error(format!("unknown class `{class_name}`")))?;
        let class = &self.program.classes[idx];
        let class_def = class.def_id;
        let class_name_owned = class.name.clone();

        // `class A extends Array {}` — upstream models the
        // instance as a real Array (so `push(new A(), 12)` works
        // and `return new A()` prints `[]`). We don't track full
        // builtin-class inheritance, so collapse the user-side
        // class to the underlying primitive constructor here.
        if walk_class_chain_parent(self, &class_name_owned, "Array") {
            return Ok(construct_builtin_class("Array", args));
        }
        if walk_class_chain_parent(self, &class_name_owned, "Map") {
            return Ok(construct_builtin_class("Map", args));
        }
        if walk_class_chain_parent(self, &class_name_owned, "Set") {
            return Ok(construct_builtin_class("Set", args));
        }
        if walk_class_chain_parent(self, &class_name_owned, "Object") {
            return Ok(construct_builtin_class("Object", args));
        }

        // Collect field-init function indices from the class chain
        // (parents first so child fields override). We snapshot
        // the list of (field_name, init_fn) so we can drop the
        // borrow before calling.
        let mut field_inits: Vec<(String, Option<usize>, Type)> = Vec::new();
        let mut cursor = class_name.to_string();
        let mut chain: Vec<String> = Vec::new();
        let mut visited: std::collections::HashSet<String> = std::collections::HashSet::new();
        loop {
            if !visited.insert(cursor.clone()) {
                break;
            }
            let Some(&i) = self.class_by_name.get(&cursor) else {
                break;
            };
            chain.push(cursor.clone());
            let c = &self.program.classes[i];
            let Some(p) = c.parent.clone() else {
                break;
            };
            cursor = p;
        }
        for cname in chain.iter().rev() {
            let &i = self.class_by_name.get(cname).unwrap();
            for f in &self.program.classes[i].instance_fields {
                field_inits.push((f.name.clone(), f.init_fn, f.ty.clone()));
            }
        }

        let instance = Rc::new(RefCell::new(crate::value::Instance {
            class: class_def,
            class_name: class_name_owned.clone(),
            fields: crate::value::ObjectData::new(),
        }));
        let this_value = Value::Instance(instance.clone());

        // Initialize fields. Each init_fn takes `this` as its only
        // param; we call it and assign the result to the field,
        // coerced to the declared field type so `real x = 12`
        // stores `12.0` not `12`.
        for (name, init, ty) in field_inits {
            let value = match init {
                Some(fn_idx) => self.run_function(fn_idx, vec![this_value.clone()])?,
                None => Value::Null,
            };
            let coerced = coerce_to_type(&value, &ty);
            instance.borrow_mut().fields.set(&name, coerced);
        }

        // Run the matching constructor, if any. Upstream allows
        // omitting the constructor entirely — in that case fields
        // are initialized but no extra code runs.
        if let Some(ctor_idx) = self.find_constructor(&class_name_owned, args.len()) {
            let mut full_args = Vec::with_capacity(args.len() + 1);
            full_args.push(this_value.clone());
            full_args.extend(args);
            self.run_function(ctor_idx, full_args)?;
        }

        Ok(this_value)
    }

    pub(crate) fn caller_class_def(&self) -> Option<DefId> {
        let idx = *self.function_stack.last()?;
        self.function(idx).owning_class
    }

    /// True if `caller` (a class DefId, or `None` for outside any
    /// class) is the same class as `owner` or a descendant of it.
    pub(crate) fn class_descends_from(&self, caller: Option<DefId>, owner: DefId) -> bool {
        let Some(c) = caller else {
            return false;
        };
        let mut cursor: Option<DefId> = Some(c);
        let mut seen: std::collections::HashSet<DefId> = std::collections::HashSet::new();
        while let Some(d) = cursor {
            if !seen.insert(d) {
                return false;
            }
            if d == owner {
                return true;
            }
            cursor = self
                .program
                .class(d)
                .and_then(|cl| cl.parent.as_deref())
                .and_then(|p| self.class_by_name.get(p).copied())
                .map(|idx| self.program.classes[idx].def_id);
        }
        false
    }

    /// True if the caller (from `caller_class_def`) is allowed to
    /// see a member with the given visibility owned by `owner`.
    pub(crate) fn member_visible(&self, owner: DefId, vis: Visibility) -> bool {
        match vis {
            Visibility::Public => true,
            Visibility::Private => self.caller_class_def() == Some(owner),
            Visibility::Protected => self.class_descends_from(self.caller_class_def(), owner),
        }
    }

    /// Look up the (owner, visibility) of a method by name. Returns
    /// `None` when no such method exists. `is_static` selects the
    /// static or instance overload set.
    pub(crate) fn method_visibility(
        &self,
        class_def: DefId,
        name: &str,
        is_static: bool,
    ) -> Option<(DefId, Visibility)> {
        let mut cursor = Some(class_def);
        let mut seen: std::collections::HashSet<DefId> = std::collections::HashSet::new();
        while let Some(d) = cursor {
            if !seen.insert(d) {
                return None;
            }
            let c = self.program.class(d)?;
            if let Some(m) = c
                .methods
                .iter()
                .find(|m| m.name == name && m.is_static == is_static)
            {
                return Some((d, m.visibility));
            }
            cursor = c
                .parent
                .as_deref()
                .and_then(|p| self.class_by_name.get(p).copied())
                .map(|idx| self.program.classes[idx].def_id);
        }
        None
    }

    /// Look up the declared type of a static field across the
    /// class chain. Returns `None` when the field isn't declared.
    pub(crate) fn static_field_type(&self, class_def: DefId, name: &str) -> Option<Type> {
        let mut cursor = Some(class_def);
        let mut seen: std::collections::HashSet<DefId> = std::collections::HashSet::new();
        while let Some(d) = cursor {
            if !seen.insert(d) {
                return None;
            }
            let c = self.program.class(d)?;
            if let Some(f) = c.static_fields.iter().find(|f| f.name == name) {
                return Some(f.ty.clone());
            }
            cursor = c
                .parent
                .as_deref()
                .and_then(|p| self.class_by_name.get(p).copied())
                .map(|idx| self.program.classes[idx].def_id);
        }
        None
    }

    /// Find the class DefId that *declares* the named static field
    /// (vs an inheritor). Used by both reads and writes so the
    /// `static_fields` cache key is consistent.
    pub(crate) fn static_field_owner(&self, class_def: DefId, name: &str) -> Option<DefId> {
        let mut cursor = Some(class_def);
        let mut seen: std::collections::HashSet<DefId> = std::collections::HashSet::new();
        while let Some(d) = cursor {
            if !seen.insert(d) {
                return None;
            }
            let c = self.program.class(d)?;
            if c.static_fields.iter().any(|f| f.name == name) {
                return Some(d);
            }
            cursor = c
                .parent
                .as_deref()
                .and_then(|p| self.class_by_name.get(p).copied())
                .map(|idx| self.program.classes[idx].def_id);
        }
        None
    }

    /// Look up the declared type of an instance field across the
    /// class chain. Returns `None` when the field isn't declared
    /// on this chain (free fields take whatever value is written).
    pub(crate) fn instance_field_type(&self, class_def: DefId, name: &str) -> Option<Type> {
        let mut cursor = Some(class_def);
        let mut seen: std::collections::HashSet<DefId> = std::collections::HashSet::new();
        while let Some(d) = cursor {
            if !seen.insert(d) {
                return None;
            }
            let c = self.program.class(d)?;
            if let Some(f) = c.instance_fields.iter().find(|f| f.name == name) {
                return Some(f.ty.clone());
            }
            cursor = c
                .parent
                .as_deref()
                .and_then(|p| self.class_by_name.get(p).copied())
                .map(|idx| self.program.classes[idx].def_id);
        }
        None
    }

    /// Check whether the named instance field is declared `final`
    /// anywhere in the class's chain. Final fields silently no-op
    /// on assignment.
    pub(crate) fn is_instance_field_final(&self, class_def: DefId, name: &str) -> bool {
        let mut cursor = Some(class_def);
        let mut seen: std::collections::HashSet<DefId> = std::collections::HashSet::new();
        while let Some(d) = cursor {
            if !seen.insert(d) {
                return false;
            }
            let Some(c) = self.program.class(d) else {
                return false;
            };
            if let Some(f) = c.instance_fields.iter().find(|f| f.name == name) {
                return f.is_final;
            }
            cursor = c
                .parent
                .as_deref()
                .and_then(|p| self.class_by_name.get(p).copied())
                .map(|idx| self.program.classes[idx].def_id);
        }
        false
    }

    /// Same as [`Self::is_instance_field_final`] but for static
    /// fields. `static final x = ...` is the common shape.
    pub(crate) fn is_static_field_final(&self, class_def: DefId, name: &str) -> bool {
        let mut cursor = Some(class_def);
        let mut seen: std::collections::HashSet<DefId> = std::collections::HashSet::new();
        while let Some(d) = cursor {
            if !seen.insert(d) {
                return false;
            }
            let Some(c) = self.program.class(d) else {
                return false;
            };
            if let Some(f) = c.static_fields.iter().find(|f| f.name == name) {
                return f.is_final;
            }
            cursor = c
                .parent
                .as_deref()
                .and_then(|p| self.class_by_name.get(p).copied())
                .map(|idx| self.program.classes[idx].def_id);
        }
        false
    }

    /// Walk a class and its parent chain looking for a static
    /// field with this name. Returns true if any ancestor declares
    /// it. Cycle-safe.
    pub(crate) fn has_static_field(&self, class_def: DefId, name: &str) -> bool {
        let mut cursor = Some(class_def);
        let mut seen: std::collections::HashSet<DefId> = std::collections::HashSet::new();
        while let Some(d) = cursor {
            if !seen.insert(d) {
                return false;
            }
            let Some(c) = self.program.class(d) else {
                return false;
            };
            if c.static_fields.iter().any(|f| f.name == name) {
                return true;
            }
            cursor = c
                .parent
                .as_deref()
                .and_then(|p| self.class_by_name.get(p).copied())
                .map(|idx| self.program.classes[idx].def_id);
        }
        false
    }

    /// Lazily-initialised static field read. Returns the cached
    /// value when present; otherwise runs the field's init
    /// function once and stores the result. Fields declared
    /// without an initialiser default to null. Walks the parent
    /// chain — `class B extends A { } B.a` returns A's `a`.
    pub(crate) fn read_static_field(&mut self, class_def: DefId, name: &str) -> Value {
        // Search this class then walk parents until we find a class
        // that declares this field. We need both the init function
        // and the visibility so callers outside the access scope
        // see `null` instead of the value.
        let mut owner = Some(class_def);
        let mut found: Option<(DefId, Option<usize>, Visibility, Type)> = None;
        let mut seen: std::collections::HashSet<DefId> = std::collections::HashSet::new();
        while let Some(d) = owner {
            if !seen.insert(d) {
                break;
            }
            let Some(c) = self.program.class(d) else {
                break;
            };
            if let Some(f) = c.static_fields.iter().find(|f| f.name == name) {
                found = Some((d, f.init_fn, f.visibility, f.ty.clone()));
                break;
            }
            owner = c
                .parent
                .as_deref()
                .and_then(|p| self.class_by_name.get(p).copied())
                .map(|idx| self.program.classes[idx].def_id);
        }
        let Some((owner_def, init_fn, vis, ty)) = found else {
            return Value::Null;
        };
        if !self.member_visible(owner_def, vis) {
            return Value::Null;
        }
        // Cache per-(owner, name) so subclasses share their parent's
        // singleton static slot. The init function still runs only
        // the first time the slot is read.
        let key = (owner_def, name.to_string());
        if let Some(v) = self.static_fields.get(&key) {
            return v.clone();
        }
        let v = match init_fn {
            Some(fn_idx) => self.run_function(fn_idx, Vec::new()).unwrap_or(Value::Null),
            None => Value::Null,
        };
        let coerced = coerce_to_type(&v, &ty);
        self.static_fields.insert(key, coerced.clone());
        coerced
    }

    /// For an array-typed or map-typed local, return its declared
    /// element/value type (so an indexed write can coerce the
    /// RHS). Returns `None` when the local isn't known to carry
    /// a typed element / value.
    pub(crate) fn current_frame_local_type(&self, local: leek_mir::LocalId) -> Option<Type> {
        let &fn_idx = self.function_stack.last()?;
        let f = self.function(fn_idx);
        let decl = f.locals.get(local.0 as usize)?;
        match &decl.ty {
            Type::Array(elt) => Some((**elt).clone()),
            Type::Map(_, val) => Some((**val).clone()),
            _ => None,
        }
    }

    /// `value.class` returns the BuiltinClass / user ClassRef the
    /// value belongs to. Mirrors upstream's class introspection.
    // Thin wrapper over `leek_runtime::class_of`, kept as a method for
    // call-site ergonomics alongside the other `Interp` helpers.
    #[allow(clippy::unused_self)]
    pub(crate) fn class_of(&self, v: &Value) -> Value {
        leek_runtime::class_of(v)
    }

    /// `base.name` with method-as-value fallback. Reads the field
    /// directly when the receiver has one; for class instances
    /// that don't have a matching field but do have a matching
    /// method, returns a [`FnValue::BoundMethod`] so the caller
    /// can invoke it later (`var m = obj.foo; m()`).
    // The parent-chain walks below move `cursor` via `while let Some(..) =
    // cursor`, so the reassignment needs a fresh clone — `clone_from` can't apply.
    #[allow(clippy::assigning_clones)]
    pub(crate) fn read_field_with_methods(&mut self, base: &Value, name: &str) -> Value {
        // Every value exposes a `.class` field. Matches upstream's
        // `__class__` slot, available on primitives as well as
        // user-class instances.
        if name == "class" {
            return self.class_of(base);
        }
        // `class.name` returns the class's source name. Works on
        // both user `ClassRef` and `BuiltinClass`.
        if name == "name" {
            match base {
                Value::ClassRef(def, _) => {
                    let n = self
                        .program
                        .class(*def)
                        .map(|c| c.name.clone())
                        .unwrap_or_default();
                    return Value::String(Rc::new(n));
                }
                Value::BuiltinClass(n) => return Value::String(Rc::new((*n).to_string())),
                _ => {}
            }
        }
        // `Class.fields` / `Class.methods` / `Class.parent` —
        // upstream's reflective introspection on a class. Walks
        // the parent chain so inherited members surface too.
        if let Value::ClassRef(def, _) = base {
            if name == "fields" {
                let mut names: Vec<Value> = Vec::new();
                let mut cursor = self.program.class(*def).map(|c| c.name.clone());
                let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
                while let Some(cn) = cursor {
                    if !seen.insert(cn.clone()) {
                        break;
                    }
                    let Some(idx) = self.class_by_name.get(&cn).copied() else {
                        break;
                    };
                    let c = &self.program.classes[idx];
                    for f in &c.instance_fields {
                        names.push(Value::String(Rc::new(f.name.clone())));
                    }
                    cursor = c.parent.clone();
                }
                return Value::Array(Rc::new(RefCell::new(names)));
            }
            if name == "static_fields" || name == "staticFields" {
                let mut names: Vec<Value> = Vec::new();
                let mut cursor = self.program.class(*def).map(|c| c.name.clone());
                let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
                while let Some(cn) = cursor {
                    if !seen.insert(cn.clone()) {
                        break;
                    }
                    let Some(idx) = self.class_by_name.get(&cn).copied() else {
                        break;
                    };
                    let c = &self.program.classes[idx];
                    for f in &c.static_fields {
                        names.push(Value::String(Rc::new(f.name.clone())));
                    }
                    cursor = c.parent.clone();
                }
                return Value::Array(Rc::new(RefCell::new(names)));
            }
            if name == "methods" {
                let mut names: Vec<Value> = Vec::new();
                let mut cursor = self.program.class(*def).map(|c| c.name.clone());
                let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
                while let Some(cn) = cursor {
                    if !seen.insert(cn.clone()) {
                        break;
                    }
                    let Some(idx) = self.class_by_name.get(&cn).copied() else {
                        break;
                    };
                    let c = &self.program.classes[idx];
                    for m in &c.methods {
                        if !m.is_static {
                            names.push(Value::String(Rc::new(m.name.clone())));
                        }
                    }
                    cursor = c.parent.clone();
                }
                return Value::Array(Rc::new(RefCell::new(names)));
            }
            if name == "static_methods" || name == "staticMethods" {
                let mut names: Vec<Value> = Vec::new();
                let mut cursor = self.program.class(*def).map(|c| c.name.clone());
                let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
                while let Some(cn) = cursor {
                    if !seen.insert(cn.clone()) {
                        break;
                    }
                    let Some(idx) = self.class_by_name.get(&cn).copied() else {
                        break;
                    };
                    let c = &self.program.classes[idx];
                    for m in &c.methods {
                        if m.is_static {
                            names.push(Value::String(Rc::new(m.name.clone())));
                        }
                    }
                    cursor = c.parent.clone();
                }
                return Value::Array(Rc::new(RefCell::new(names)));
            }
            if name == "constructors" {
                let mut names: Vec<Value> = Vec::new();
                if let Some(c) = self.program.class(*def) {
                    for _ in &c.constructors {
                        names.push(Value::Int(0));
                    }
                }
                return Value::Array(Rc::new(RefCell::new(names)));
            }
            if name == "parent" || name == "super" || name == "superclass" {
                if let Some(c) = self.program.class(*def) {
                    if let Some(p) = &c.parent {
                        if let Some(pidx) = self.class_by_name.get(p).copied() {
                            let pc = &self.program.classes[pidx];
                            return Value::ClassRef(pc.def_id, Rc::new(pc.name.clone()));
                        }
                    }
                }
                // `A.class.super` returns the synthetic root `Value`
                // class for any user class with no explicit parent.
                if name == "super" {
                    return Value::BuiltinClass("Value");
                }
                return Value::Null;
            }
        }
        if let Value::Instance(inst) = base {
            if let Some(v) = inst.borrow().fields.get(name).cloned() {
                return v;
            }
            let class_name = inst.borrow().class_name.clone();
            if let Some(function_idx) = self.find_method(&class_name, name) {
                return Value::Function(FnValue::BoundMethod {
                    function_idx,
                    receiver: Box::new(base.clone()),
                });
            }
            return Value::Null;
        }
        if let Value::ClassRef(def_id, _) = base {
            if let Some(class) = self.program.class(*def_id) {
                // Look up static fields first; static methods second;
                // then fall back to instance methods returned as
                // free functions — `A.m` for an instance method `m`
                // yields a callable that takes the receiver as its
                // first (explicit) argument.
                if self.has_static_field(*def_id, name) {
                    return self.read_static_field(*def_id, name);
                }
                if let Some(function_idx) = self.find_static_method(&class.name, name) {
                    return Value::Function(FnValue::User(
                        self.function(function_idx)
                            .def_id
                            .unwrap_or(DefId(u32::MAX)),
                    ));
                }
                if let Some(function_idx) = self.find_method(&class.name, name) {
                    return Value::Function(FnValue::User(
                        self.function(function_idx)
                            .def_id
                            .unwrap_or(DefId(u32::MAX)),
                    ));
                }
            }
        }
        if let Value::BuiltinClass(class_name) = base {
            // Reflective bits common to all builtin classes — these
            // return empty arrays since builtins don't carry
            // user-declared fields / methods / parent. Matches
            // upstream `LeekClass.fields` / etc.
            if matches!(
                name,
                "fields"
                    | "static_fields"
                    | "methods"
                    | "static_methods"
                    | "staticFields"
                    | "staticMethods"
            ) {
                return Value::Array(Rc::new(RefCell::new(Vec::new())));
            }
            if name == "parent" || name == "super" || name == "superclass" {
                // Every builtin class except `Value` itself
                // inherits from `Value`. `Class.super` / `Array.super`
                // etc. all return `<class Value>`.
                if *class_name == "Value" {
                    return Value::Null;
                }
                return Value::BuiltinClass("Value");
            }
            if name == "name" {
                return Value::String(Rc::new((*class_name).to_string()));
            }
            if let Some(v) = builtin_class_static(class_name, name) {
                return v;
            }
        }
        read_field(base, name)
    }

    /// `base[idx]` with method-as-value fallback. Mirrors
    /// [`Self::read_field_with_methods`] but for index syntax —
    /// `obj['methodName']` returns a bound method just like
    /// `obj.methodName` does.
    pub(crate) fn read_index_with_methods(&mut self, base: &Value, idx: &Value) -> Value {
        if matches!(base, Value::Instance(_)) {
            if let Value::String(s) = idx {
                return self.read_field_with_methods(base, s.as_str());
            }
        }
        // `class['fieldName']` inside a method body resolves to
        // the static field. Route through the field path so the
        // lazy init runs.
        if matches!(base, Value::ClassRef(_, _) | Value::BuiltinClass(_)) {
            if let Value::String(s) = idx {
                return self.read_field_with_methods(base, s.as_str());
            }
        }
        // v1-v3 map key lookups truncated real indices to integers
        // before matching — `m[5.7]` on `[5: 12]` returned `12`. v4
        // tightened this to strict equality. We replicate the older
        // behaviour for the older versions only.
        if self.version <= 3 {
            if let (Value::Map(m), Value::Real(r)) = (base, idx) {
                let truncated = Value::Int(leek_runtime::real_to_int(*r));
                if let Some(v) = m.borrow().get(&truncated).cloned() {
                    return v;
                }
            }
            // Collection-typed keys collapse to `to_long` (size)
            // when read in v1-v3 — matches `LegacyArray.transformKey`.
            if matches!(base, Value::Map(_)) {
                let key = legacy_map_key(idx);
                if let Value::Map(m) = base {
                    if let Some(v) = m.borrow().get(&key).cloned() {
                        return v;
                    }
                }
            }
        }
        read_index(base, idx)
    }
}
