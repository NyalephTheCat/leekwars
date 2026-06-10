//! The `Checker` walker — pushes through the AST, infers types,
//! and emits diagnostics. All state lives on the `Checker` struct;
//! pure type relations and AST→type conversions live in [`ty`].
//!
//! [`ty`]: super::ty

use std::collections::HashMap;

use leek_diagnostics::Diagnostic;
use leek_span::SourceId;
use leek_syntax::Version;

use crate::Options;
use crate::index::TypedExpr;
use crate::ty::Type;

mod binary;
mod call;
mod expr;
mod file;
mod helpers;
mod narrow;
mod prelude;
mod scope;
mod scope_ops;
mod stmt;

pub(crate) use scope::Scope;

pub(crate) struct Checker {
    pub(crate) source: SourceId,
    pub(crate) scopes: Vec<Scope>,
    pub(crate) diagnostics: Vec<Diagnostic>,
    /// LSP-facing table: every inferred-expression result is appended
    /// here. Exposed via [`crate::check_collecting`].
    pub(crate) typed_exprs: Vec<TypedExpr>,
    pub(crate) opts: Options,
    pub(crate) version: Version,
    /// Stack of declared return types — one entry per enclosing
    /// function/method. `None` means "no annotation" (return type
    /// is dynamic).
    pub(crate) return_types: Vec<Option<Type>>,
    /// Names declared as `var x = []` / `var x = [:]` (empty
    /// literal). In strict v4 mode, the first index-assignment to
    /// any of these vars is `ASSIGNMENT_INCOMPATIBLE_TYPE` — upstream
    /// infers the literal's element type as "nothing" and rejects.
    pub(crate) empty_collection_vars: std::collections::HashSet<String>,
    /// Per top-level user function: declared parameter types
    /// (`Type::Any` when no annotation). Indexed by source order.
    pub(crate) user_fn_param_types: HashMap<String, Vec<Type>>,
    /// Per top-level user function: declared (or inferred-as-null
    /// for bodies without an explicit `return`) return type.
    pub(crate) user_fn_return_type: HashMap<String, Type>,
    /// Experimental: per generic user function (`f<T>(…) -> T`), the
    /// [`GenericSig`](crate::generic::GenericSig) built from its
    /// signature. Present only when generic syntax is parsed; call
    /// sites instantiate it against concrete argument types so the
    /// result type is resolved precisely (e.g. `first(intArr)` →
    /// `integer`) instead of widening to the declared `T`.
    pub(crate) user_fn_generic: HashMap<String, crate::generic::GenericSig>,
    /// Names of the classes whose bodies we're currently walking.
    /// The top of the stack is the class `this` refers to. A stack
    /// (rather than a single slot) keeps lambdas / nested scopes
    /// honest, though Leekscript doesn't nest class declarations.
    pub(crate) class_stack: Vec<String>,
    /// `class Child extends Parent` → `Child` ⇒ `Parent`. Collected
    /// in a pre-pass so `super` can be typed as a parent instance.
    pub(crate) class_parents: HashMap<String, String>,
    /// Per class: declared field name → type (`Type::Any` when
    /// unannotated). Collected in a pre-pass for member-access
    /// inference (`obj.field`). Inheritance is resolved at lookup time
    /// via [`class_parents`].
    pub(crate) class_field_types: HashMap<String, HashMap<String, Type>>,
    /// Per class: method name → declared return type. Powers
    /// `obj.method()` / `Class.method()` call-result inference.
    pub(crate) class_method_returns: HashMap<String, HashMap<String, Type>>,
    /// Experimental: per *generic* class (`class Box<T> { … }`), member
    /// patterns expressed over its type variables. Present only when the
    /// class declares a `TypeParamList`. Drives member-access inference
    /// against a bound instance type (`Box<integer>` → `value: integer`).
    pub(crate) generic_classes: HashMap<String, GenericClassInfo>,
    /// Experimental (`LEEK_EXPERIMENTAL_TYPES`): `type Name = T`
    /// aliases, collected in a file-wide pre-pass. Alias names follow
    /// the class-name convention (uppercase initial) so a reference
    /// resolves as `ClassInstance(Name, [])` and [`Checker::
    /// substitute_aliases`] can swap the body in.
    pub(crate) type_aliases: HashMap<String, Type>,
    /// Experimental (`LEEK_EXPERIMENTAL_INTERFACES`): `interface Name
    /// { … }` declarations, collected in a file-wide pre-pass. An
    /// interface name in an annotation resolves as `ClassInstance(Name,
    /// [])` (uppercase fallback); [`Checker::types_assignable`] treats
    /// instances of implementing classes as assignable to it.
    pub(crate) interfaces: HashMap<String, InterfaceInfo>,
    /// Experimental: `class C implements I, J` → `C` ⇒ `[I, J]`.
    /// Checked against [`interfaces`](Self::interfaces) and consulted
    /// (through the `extends` chain) by interface assignability.
    pub(crate) class_implements: HashMap<String, Vec<String>>,
}

/// Declared members of an experimental `interface Name { … }` — what an
/// implementing class must provide. Member entries carry the declared
/// type (fields) or return type (methods); presence on the class (or an
/// ancestor) is what's verified.
#[derive(Debug, Default, Clone)]
pub(crate) struct InterfaceInfo {
    /// Field name → declared type.
    pub fields: HashMap<String, Type>,
    /// Method name → declared return type.
    pub methods: HashMap<String, Type>,
}

/// Collected generic metadata for a `class C<T, …>`. Member types are
/// [`GType`](crate::generic::GType) patterns over the class's type
/// variables (and, for methods, the method's own type variables too).
#[derive(Debug, Default, Clone)]
pub(crate) struct GenericClassInfo {
    /// Declared type-parameter names, in order (`Box<T>` → `["T"]`).
    pub type_params: Vec<String>,
    /// Field name → declared type pattern.
    pub fields: HashMap<String, crate::generic::GType>,
    /// Method name → signature (params + return) over the class's *and*
    /// the method's own type variables.
    pub methods: HashMap<String, crate::generic::GenericSig>,
    /// Constructor parameter patterns, used to bind the class's type
    /// variables from `new C(args)` argument types.
    pub ctor_params: Vec<crate::generic::GType>,
    /// `extends Parent` name, if any.
    pub parent: Option<String>,
    /// The parent's type arguments as patterns over *this* class's type
    /// variables — `Box<T> extends Container<T>` → `[Var("T")]`. Empty
    /// for a non-generic parent. Used to re-map type arguments when
    /// resolving an inherited generic member.
    pub parent_args: Vec<crate::generic::GType>,
}

impl Checker {
    pub(crate) fn new(source: SourceId, version: Version, opts: Options) -> Self {
        Self {
            source,
            scopes: vec![Scope::empty()],
            diagnostics: Vec::new(),
            typed_exprs: Vec::new(),
            opts,
            version,
            return_types: Vec::new(),
            empty_collection_vars: std::collections::HashSet::new(),
            user_fn_param_types: HashMap::new(),
            user_fn_return_type: HashMap::new(),
            user_fn_generic: HashMap::new(),
            class_stack: Vec::new(),
            class_parents: HashMap::new(),
            class_field_types: HashMap::new(),
            class_method_returns: HashMap::new(),
            generic_classes: HashMap::new(),
            type_aliases: HashMap::new(),
            interfaces: HashMap::new(),
            class_implements: HashMap::new(),
        }
    }

    /// Resolve a `TypeRef` node with experimental `type` aliases
    /// applied. Checker call sites use this instead of bare
    /// [`type_from_node`](crate::ty::type_from_node) so annotations
    /// can reference declared aliases.
    pub(crate) fn resolve_type_node(&self, node: &leek_syntax::SyntaxNode) -> Type {
        self.substitute_aliases(crate::ty::type_from_node(node))
    }

    /// Replace experimental alias references in a resolved type with
    /// their bodies. Alias names resolve as argument-less unknown
    /// `ClassInstance`s (uppercase initial), so the swap is a
    /// recursive rewrite of those nodes; everything else passes
    /// through structurally.
    pub(crate) fn substitute_aliases(&self, t: Type) -> Type {
        if self.type_aliases.is_empty() {
            return t;
        }
        self.subst_alias_rec(&t)
    }

    fn subst_alias_rec(&self, t: &Type) -> Type {
        use crate::ty::nullable_of;
        match t {
            Type::ClassInstance(name, args) if args.is_empty() => self
                .type_aliases
                .get(name)
                .cloned()
                .unwrap_or_else(|| t.clone()),
            Type::Array(el) => Type::Array(Box::new(self.subst_alias_rec(el))),
            Type::Set(el) => Type::Set(Box::new(self.subst_alias_rec(el))),
            Type::Map(k, v) => Type::Map(
                Box::new(self.subst_alias_rec(k)),
                Box::new(self.subst_alias_rec(v)),
            ),
            // Re-wrap through the canonicalizing constructors: an
            // alias body may itself be nullable or a union, and the
            // result must stay in canonical form (flattened, null
            // lifted outward).
            Type::Nullable(inner) => nullable_of(&self.subst_alias_rec(inner)),
            Type::Union(ms) => Type::union_of(ms.iter().map(|m| self.subst_alias_rec(m)).collect()),
            Type::Tuple(ms) => Type::Tuple(ms.iter().map(|m| self.subst_alias_rec(m)).collect()),
            Type::FunctionWithReturn { params, ret } => Type::FunctionWithReturn {
                params: params.iter().map(|p| self.subst_alias_rec(p)).collect(),
                ret: Box::new(self.subst_alias_rec(ret)),
            },
            _ => t.clone(),
        }
    }

    /// Interface-aware assignability: [`Type::assignable_to`], plus —
    /// when experimental interfaces are collected — an instance of a
    /// class is assignable to an interface it (or an ancestor)
    /// `implements`. Checker diagnostics sites use this instead of bare
    /// `assignable_to` so interface-typed slots accept implementors.
    pub(crate) fn types_assignable(&self, actual: &Type, expected: &Type) -> bool {
        Type::assignable_to(actual, expected) || self.interface_accepts(actual, expected)
    }

    /// Whether `expected` is (modulo nullability) a declared interface
    /// that `actual`'s class implements. Unions recurse through
    /// [`Self::types_assignable`] so a member can satisfy the slot
    /// either nominally or via an interface.
    fn interface_accepts(&self, actual: &Type, expected: &Type) -> bool {
        if self.interfaces.is_empty() {
            return false;
        }
        match (actual, expected) {
            // Pairwise nullability: a nullable actual only fits a
            // nullable expected; a plain actual fits either.
            (Type::Nullable(a), Type::Nullable(e)) => self.interface_accepts(a, e),
            // (a nullable actual against a plain expected falls through
            // to the catch-all below and is rejected.)
            (a, Type::Nullable(e)) => self.interface_accepts(a, e),
            // Every member of a union actual must fit; any member of a
            // union expected may accept.
            (Type::Union(ms), e) => ms.iter().all(|m| self.types_assignable(m, e)),
            (a, Type::Union(ms)) => ms.iter().any(|m| self.types_assignable(a, m)),
            (Type::ClassInstance(class, _), Type::ClassInstance(iface, eargs)) => {
                eargs.is_empty()
                    && self.interfaces.contains_key(iface)
                    && self.class_implements_iface(class, iface)
            }
            _ => false,
        }
    }

    /// Whether `class` (or an ancestor on its `extends` chain) declares
    /// `implements iface`.
    pub(crate) fn class_implements_iface(&self, class: &str, iface: &str) -> bool {
        self.walk_chain(class, |c| {
            self.class_implements
                .get(c)
                .is_some_and(|list| list.iter().any(|i| i == iface))
                .then_some(())
        })
        .is_some()
    }

    /// The class `this` refers to at the current walk position, if any.
    pub(crate) fn current_class(&self) -> Option<&str> {
        self.class_stack.last().map(String::as_str)
    }

    /// The parent of the class `super` refers to at the current walk
    /// position, if the enclosing class declares `extends Parent`.
    pub(crate) fn current_super_class(&self) -> Option<&str> {
        let cur = self.current_class()?;
        self.class_parents.get(cur).map(String::as_str)
    }

    /// The declared type of `field` on `class` (or an ancestor). `None`
    /// if no such field is declared anywhere in the chain.
    pub(crate) fn lookup_field_type(&self, class: &str, field: &str) -> Option<Type> {
        self.walk_chain(class, |c| {
            self.class_field_types
                .get(c)
                .and_then(|m| m.get(field))
                .cloned()
        })
    }

    /// The declared return type of `method` on `class` (or an ancestor).
    pub(crate) fn lookup_method_return(&self, class: &str, method: &str) -> Option<Type> {
        self.walk_chain(class, |c| {
            self.class_method_returns
                .get(c)
                .and_then(|m| m.get(method))
                .cloned()
        })
    }

    /// Resolve a *generic* field on `class` instantiated with type
    /// arguments `args`, walking the inheritance chain and re-mapping the
    /// type arguments at each `extends Parent<…>` boundary. For
    /// `class IntBox extends Box<integer>` (i.e. `Box<T> { T value }`),
    /// `IntBox.value` resolves to `integer`: the field lives on the parent
    /// `Box`, whose `T` is bound to `integer` by the `extends` clause.
    /// `None` when the field isn't a generic member anywhere in the chain.
    pub(crate) fn resolve_generic_field(
        &self,
        class: &str,
        args: &[Type],
        field: &str,
    ) -> Option<Type> {
        self.walk_generic_chain(class, args, &mut 0, |info, bindings| {
            info.fields
                .get(field)
                .map(|pat| crate::generic::apply(pat, bindings))
        })
    }

    /// Resolve a *generic* method's return type on `class` instantiated
    /// with class type arguments `cls_args`, against the concrete call
    /// `arg_types`, walking + re-mapping the inheritance chain (see
    /// [`Self::resolve_generic_field`]).
    pub(crate) fn resolve_generic_method(
        &self,
        class: &str,
        cls_args: &[Type],
        method: &str,
        arg_types: &[Type],
    ) -> Option<Type> {
        self.walk_generic_chain(class, cls_args, &mut 0, |info, bindings| {
            info.methods.get(method).map(|sig| {
                let mut b = bindings.clone();
                crate::generic::solve(&sig.params, arg_types, &mut b);
                crate::generic::apply(&sig.ret, &b)
            })
        })
    }

    /// Walk `class` and its generic ancestors, calling `f` with each
    /// class's [`GenericClassInfo`] and the bindings of its type variables
    /// to the concrete `args` at that level. Returns the first `Some` `f`
    /// produces. At each `extends Parent<pat…>` step, the parent's
    /// arguments are computed by substituting the current bindings into
    /// the stored parent-arg patterns. `depth` guards cyclic `extends`.
    fn walk_generic_chain<T>(
        &self,
        class: &str,
        args: &[Type],
        depth: &mut u32,
        f: impl Fn(&GenericClassInfo, &HashMap<String, Type>) -> Option<T> + Copy,
    ) -> Option<T> {
        if *depth > 64 {
            return None;
        }
        *depth += 1;
        let info = self.generic_classes.get(class)?;
        let bindings: HashMap<String, Type> = info
            .type_params
            .iter()
            .cloned()
            .zip(args.iter().cloned())
            .collect();
        if let Some(found) = f(info, &bindings) {
            return Some(found);
        }
        // Inherited: re-map the parent's type arguments through this
        // class's bindings, then recurse.
        let parent = info.parent.as_deref()?;
        let parent_args: Vec<Type> = info
            .parent_args
            .iter()
            .map(|pat| crate::generic::apply(pat, &bindings))
            .collect();
        self.walk_generic_chain(parent, &parent_args, depth, f)
    }

    /// Walk `class` and its ancestors, returning the first `Some` that
    /// `f` produces. Guards against cyclic `extends` chains.
    fn walk_chain<T>(&self, class: &str, f: impl Fn(&str) -> Option<T>) -> Option<T> {
        let mut current = Some(class.to_string());
        let mut seen = 0;
        while let Some(c) = current {
            if seen > 64 {
                break;
            }
            seen += 1;
            if let Some(found) = f(&c) {
                return Some(found);
            }
            current = self.class_parents.get(&c).cloned();
        }
        None
    }
}
