//! The `Type` enum and the conversions between it and the CST —
//! `type_from_node`, `fn_return_type`. Plus the small primitive
//! relations: assignability and numeric promotion.

use leek_syntax::{SyntaxKind, SyntaxNode};

/// Canonical Leekscript types — see `doc/type-system.md`.
#[cfg_attr(feature = "salsa", derive(salsa::Update))]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Type {
    /// Top type / unknown — accepts anything.
    Any,
    /// The `null` literal's type. Treated as compatible with any
    /// other type for assignment purposes (Leekscript is permissive
    /// about null values).
    Null,
    /// `void` — function "returns nothing". Distinct from `Null` for
    /// return-type checking: a void function cannot `return value`,
    /// not even `return null`.
    Void,
    Boolean,
    Integer,
    Real,
    String,
    /// `Array<T>`. Element type may be `Any`.
    Array(Box<Type>),
    /// `Map<K, V>`.
    Map(Box<Type>, Box<Type>),
    /// `Set<T>`.
    Set(Box<Type>),
    /// `{f: v}` literal — not a class instance.
    Object,
    /// `new ClassName(...)`. The `Vec` holds bound generic type
    /// arguments for an experimental generic class (`Box<integer>` →
    /// `["Box", [Integer]]`); it is empty for non-generic classes and
    /// for generic ones whose arguments couldn't be inferred. Codegen
    /// (MIR/backends) ignores the arguments — they exist only to drive
    /// member-access inference.
    ClassInstance(String, Vec<Type>),
    /// First-class function reference (signature not tracked yet).
    Function,
    /// `Function<P0, … => R>` — a function reference whose parameter
    /// and return types are (partially) known. `params` may be empty
    /// (`Function< => R>`, or a bare typed return). Powers the
    /// IMPOSSIBLE_CAST check (return side) and arity/parameter
    /// inspection.
    FunctionWithReturn { params: Vec<Type>, ret: Box<Type> },
    /// `[a..b]` interval literal.
    Interval,
    /// `T?` — nullable wrapper. Permits `null`, but the inner type
    /// still drives coercion (`real? a = 12` stores `12.0`).
    Nullable(Box<Type>),
}

impl Type {
    /// Build a `Function<params… => ret>` type.
    pub fn function_with(params: Vec<Type>, ret: Type) -> Self {
        Type::FunctionWithReturn {
            params,
            ret: Box::new(ret),
        }
    }

    /// True if a value of `actual` can be assigned to a slot of
    /// `expected`. Liberal toward `Any` and `Null` since the
    /// runtime is dynamic; strict for primitive mismatches. For
    /// composite types we ignore the type arguments — generic-arg
    /// variance is intentionally out of scope for slice 1.
    pub fn assignable_to(actual: &Type, expected: &Type) -> bool {
        // A nullable target accepts whatever its inner type accepts
        // (plus null). Strip the wrapper before delegating so the
        // primitive table below stays small.
        if let Type::Nullable(inner) = expected {
            return matches!(actual, Type::Null) || Type::assignable_to(actual, inner);
        }
        // A nullable source is assignable wherever its inner is —
        // dynamic checks at runtime handle the null case.
        if let Type::Nullable(inner) = actual {
            return Type::assignable_to(inner, expected);
        }
        match (actual, expected) {
            (Type::Any, _) | (_, Type::Any) => true,
            // Null is universally assignable in dynamic semantics.
            (Type::Null, _) | (_, Type::Null) => true,
            // Integer ↔ Real cross is permitted (per type-system.md §5.1).
            (Type::Integer, Type::Real) | (Type::Real, Type::Integer) => true,
            // Composite outer-type match — ignore inner args.
            (Type::Array(_), Type::Array(_)) => true,
            (Type::Map(_, _), Type::Map(_, _)) => true,
            (Type::Set(_), Type::Set(_)) => true,
            // Function ↔ FunctionWithReturn cross-assignability:
            // assigning an un-annotated function to a typed slot is
            // allowed (the return type is just unknown). And a
            // typed function with matching return type is also OK;
            // mismatches between two annotated return types are
            // caught at call sites by `check_call`.
            (Type::Function, Type::FunctionWithReturn { .. }) => true,
            (Type::FunctionWithReturn { .. }, Type::Function) => true,
            (
                Type::FunctionWithReturn { ret: a, .. },
                Type::FunctionWithReturn { ret: b, .. },
            ) => Type::assignable_to(a, b),
            // Class-instance match is name-based; generic arguments
            // don't affect assignment compatibility.
            (Type::ClassInstance(a, _), Type::ClassInstance(b, _)) => a == b,
            (a, b) => a == b,
        }
    }
}

/// Unwrap `Nullable` down to the underlying `ClassInstance` name (used
/// to find the class behind a possibly-nullable receiver).
pub(crate) fn class_name_of_type(ty: &Type) -> Option<String> {
    match ty {
        Type::ClassInstance(n, _) => Some(n.clone()),
        Type::Nullable(inner) => class_name_of_type(inner),
        _ => None,
    }
}

/// The bound generic type arguments of a (possibly nullable) class
/// instance — `Box<integer>` → `[Integer]`; empty for non-generic or
/// non-instance types.
pub(crate) fn instance_type_args(ty: &Type) -> Vec<Type> {
    match ty {
        Type::ClassInstance(_, args) => args.clone(),
        Type::Nullable(inner) => instance_type_args(inner),
        _ => Vec::new(),
    }
}

/// Wrap `t` as nullable, collapsing `Null`/already-nullable cases.
pub(crate) fn nullable_of(t: &Type) -> Type {
    match t {
        Type::Null => Type::Null,
        Type::Nullable(_) => t.clone(),
        _ => Type::Nullable(Box::new(t.clone())),
    }
}

/// Strip a `Nullable` wrapper to its inner type. `Null`/`Any` widen to
/// `Any` (we don't know the non-null type). Used by `!= null` narrowing.
pub(crate) fn strip_nullable(t: &Type) -> Type {
    match t {
        Type::Nullable(inner) => (**inner).clone(),
        Type::Null | Type::Any => Type::Any,
        other => other.clone(),
    }
}

/// Join two branch types (ternary arms, narrowed merges). Equal types
/// collapse; `int`/`real` promote to `real`; `null` + `T` becomes `T?`;
/// anything else widens to `any`.
pub(crate) fn unify_types(a: &Type, b: &Type) -> Type {
    if a == b {
        return a.clone();
    }
    match (a, b) {
        (Type::Any, _) | (_, Type::Any) => Type::Any,
        (Type::Integer, Type::Real) | (Type::Real, Type::Integer) => Type::Real,
        (Type::Null, t) | (t, Type::Null) => nullable_of(t),
        (Type::Nullable(x), y) | (y, Type::Nullable(x)) => {
            if **x == *y {
                Type::Nullable(x.clone())
            } else {
                Type::Any
            }
        }
        _ => Type::Any,
    }
}

/// Numeric-promotion result for binary `+`/`-`/`*`/`/`/`%`/`**`.
/// String concatenation is handled by the caller before invoking
/// this — `Plus` with a string operand short-circuits to String.
pub(crate) fn promote_numeric(lhs: &Type, rhs: &Type) -> Type {
    use Type::{Real, Integer, Boolean, Any};
    match (lhs, rhs) {
        (Real, _) | (_, Real) => Real,
        (Integer, Integer) => Integer,
        (Integer, Boolean) | (Boolean, Integer) => Integer,
        (Boolean, Boolean) => Integer,
        (Any, _) | (_, Any) => Any,
        _ => Any,
    }
}

/// Find the declared return type of a function-shaped node
/// (FnDecl, ClassMethod, ClassConstructor). Returns `None` when no
/// `-> T` / `=> T` is annotated.
pub(crate) fn fn_return_type(node: &SyntaxNode) -> Option<Type> {
    // Scan children in order. Track whether we've passed the
    // ParamList — the return type's TypeRef appears strictly after
    // the parameter list, after an Arrow/FatArrow token.
    let mut past_params = false;
    let mut saw_arrow = false;
    for el in node.children_with_tokens() {
        match el {
            rowan::NodeOrToken::Node(n) => {
                if !past_params {
                    if n.kind() == SyntaxKind::ParamList {
                        past_params = true;
                    }
                    continue;
                }
                if saw_arrow && n.kind() == SyntaxKind::TypeRef {
                    return Some(type_from_node(&n));
                }
            }
            rowan::NodeOrToken::Token(t) => {
                if past_params && matches!(t.kind(), SyntaxKind::Arrow | SyntaxKind::FatArrow) {
                    saw_arrow = true;
                }
            }
        }
    }
    None
}

/// Like [`fn_return_type`] but yields the return-type `TypeRef`
/// *node* (for generic-pattern building), not the resolved [`Type`].
pub(crate) fn fn_return_type_node(node: &SyntaxNode) -> Option<SyntaxNode> {
    let mut past_params = false;
    let mut saw_arrow = false;
    for el in node.children_with_tokens() {
        match el {
            rowan::NodeOrToken::Node(n) => {
                if !past_params {
                    if n.kind() == SyntaxKind::ParamList {
                        past_params = true;
                    }
                    continue;
                }
                if saw_arrow && n.kind() == SyntaxKind::TypeRef {
                    return Some(n);
                }
            }
            rowan::NodeOrToken::Token(t) => {
                if past_params && matches!(t.kind(), SyntaxKind::Arrow | SyntaxKind::FatArrow) {
                    saw_arrow = true;
                }
            }
        }
    }
    None
}

/// The node's *own* `<T, …>` [`TypeParamList`](SyntaxKind::TypeParamList):
/// the first one that appears *before* any `extends` keyword. A class's
/// own type params come right after its name; the list following
/// `extends Parent<…>` holds the parent's type *arguments* and must not be
/// mistaken for the class's parameters. Functions/methods have no
/// `extends`, so this is just their first `TypeParamList`.
fn own_type_param_list(node: &SyntaxNode) -> Option<SyntaxNode> {
    for el in node.children_with_tokens() {
        match el {
            rowan::NodeOrToken::Node(n) if n.kind() == SyntaxKind::TypeParamList => {
                return Some(n);
            }
            rowan::NodeOrToken::Token(t) if t.kind() == SyntaxKind::KwExtends => {
                return None;
            }
            _ => {}
        }
    }
    None
}

/// The names declared in a function/class/method's `<T, U, …>`
/// [`TypeParamList`](SyntaxKind::TypeParamList), if any. Empty when the
/// node is non-generic (or experimental generic syntax is off, since
/// the parser only produces the node then).
pub(crate) fn collect_type_params(node: &SyntaxNode) -> std::collections::HashSet<String> {
    let mut out = std::collections::HashSet::new();
    if let Some(list) = own_type_param_list(node) {
        for tok in list.children_with_tokens().filter_map(rowan::NodeOrToken::into_token) {
            if tok.kind() == SyntaxKind::Ident {
                out.insert(tok.text().to_string());
            }
        }
    }
    out
}

/// The `<T, U, …>` type-parameter names in declared order. Unlike
/// [`collect_type_params`] (a set, for membership tests) this preserves
/// order so a class's type arguments can be bound positionally.
pub(crate) fn type_param_list_names(node: &SyntaxNode) -> Vec<String> {
    let mut out = Vec::new();
    if let Some(list) = own_type_param_list(node) {
        for tok in list.children_with_tokens().filter_map(rowan::NodeOrToken::into_token) {
            if tok.kind() == SyntaxKind::Ident {
                out.push(tok.text().to_string());
            }
        }
    }
    out
}

/// Build a [`GType`] pattern from a `TypeRef` node, treating any head
/// name listed in `typarams` as a generic variable. Composite shapes
/// (`Array<T>`, `Map<K,V>`, `Set<T>`, `T?`) recurse so a variable nested
/// inside is preserved; everything else collapses to a concrete leaf via
/// [`type_from_node`].
pub(crate) fn gtype_from_node(
    node: &SyntaxNode,
    typarams: &std::collections::HashSet<String>,
) -> crate::generic::GType {
    use crate::generic::GType;
    let has_pipe = node
        .children_with_tokens()
        .filter_map(rowan::NodeOrToken::into_token)
        .any(|t| t.kind() == SyntaxKind::Pipe);
    if has_pipe {
        return GType::Concrete(Type::Any);
    }
    let has_nullable = node
        .children_with_tokens()
        .filter_map(rowan::NodeOrToken::into_token)
        .any(|t| t.kind() == SyntaxKind::Question);
    let inner = gtype_from_node_primitive(node, typarams);
    if has_nullable {
        GType::Nullable(Box::new(inner))
    } else {
        inner
    }
}

fn gtype_from_node_primitive(
    node: &SyntaxNode,
    typarams: &std::collections::HashSet<String>,
) -> crate::generic::GType {
    use crate::generic::GType;
    let head = node
        .children_with_tokens()
        .filter_map(rowan::NodeOrToken::into_token)
        .find(|t| {
            matches!(
                t.kind(),
                SyntaxKind::Ident | SyntaxKind::KwBoolean | SyntaxKind::KwVoid | SyntaxKind::KwNull
            )
        });
    let Some(head) = head else {
        return GType::Concrete(Type::Any);
    };
    let name = head.text();
    if typarams.contains(name) {
        return GType::Var(name.to_string());
    }
    let has_generic = node.children().any(|n| n.kind() == SyntaxKind::TypeRef);
    let arg_gtype = |n: &SyntaxNode, skip: usize| {
        n.children()
            .filter(|c| c.kind() == SyntaxKind::TypeRef)
            .nth(skip)
            .map_or(GType::Concrete(Type::Any), |c| gtype_from_node(&c, typarams))
    };
    match name.to_ascii_lowercase().as_str() {
        "array" if has_generic => GType::Array(Box::new(arg_gtype(node, 0))),
        "array" => GType::Array(Box::new(GType::Concrete(Type::Any))),
        "map" => GType::Map(Box::new(arg_gtype(node, 0)), Box::new(arg_gtype(node, 1))),
        "set" => GType::Set(Box::new(arg_gtype(node, 0))),
        // Non-generic leaf: reuse the plain resolver.
        _ => GType::Concrete(type_from_node_primitive(node)),
    }
}

/// Read a `TypeRef` CST node and return the corresponding [`Type`].
/// Unknown/complex types are mapped to `Type::Any` (conservatively
/// accepts anything), so we never raise a false-positive on a type
/// we don't model.
pub fn type_from_node(node: &SyntaxNode) -> Type {
    // Union types (`integer|real`, `real|null`) collapse to `Any`
    // for coercion purposes — the runtime should accept any of the
    // listed variants without conversion. Nullable annotations
    // (`real?`) still coerce numerically (so `real? a = 12` stores
    // `12.0`) but tolerate `null`; those are handled below in the
    // primitive switch by mapping through a tiny helper that
    // strips the trailing `?` token.
    let has_pipe = node
        .children_with_tokens()
        .filter_map(rowan::NodeOrToken::into_token)
        .any(|t| t.kind() == SyntaxKind::Pipe);
    if has_pipe {
        return Type::Any;
    }
    let has_nullable = node
        .children_with_tokens()
        .filter_map(rowan::NodeOrToken::into_token)
        .any(|t| t.kind() == SyntaxKind::Question);
    let inner = type_from_node_primitive(node);
    if has_nullable {
        return Type::Nullable(Box::new(inner));
    }
    inner
}

/// Like `type_from_node` but skips the union/nullable detection.
/// Used to compute the inner type of a nullable wrapper.
fn type_from_node_primitive(node: &SyntaxNode) -> Type {
    // The "type-name" token can be a normal Ident or one of a few
    // type-keyword tokens (`void`, `null`, …) at v3+. Treat them
    // uniformly by name.
    let head = node
        .children_with_tokens()
        .filter_map(rowan::NodeOrToken::into_token)
        .find(|t| {
            matches!(
                t.kind(),
                SyntaxKind::Ident | SyntaxKind::KwBoolean | SyntaxKind::KwVoid | SyntaxKind::KwNull
            )
        });
    let Some(head) = head else {
        return Type::Any;
    };
    let name = head.text();
    let has_generic = node.children().any(|n| n.kind() == SyntaxKind::TypeRef);
    let lower = name.to_ascii_lowercase();
    match lower.as_str() {
        "integer" | "int" => Type::Integer,
        "real" | "number" | "float" | "double" => Type::Real,
        "boolean" | "bool" => Type::Boolean,
        "string" => Type::String,
        "null" => Type::Null,
        "void" => Type::Void,
        "any" => Type::Any,
        "array" if has_generic => {
            let arg = node
                .children()
                .find(|n| n.kind() == SyntaxKind::TypeRef)
                .map_or(Type::Any, |n| type_from_node(&n));
            Type::Array(Box::new(arg))
        }
        "array" => Type::Array(Box::new(Type::Any)),
        "map" => {
            let mut args = node
                .children()
                .filter(|n| n.kind() == SyntaxKind::TypeRef)
                .map(|n| type_from_node(&n));
            let k = args.next().unwrap_or(Type::Any);
            let v = args.next().unwrap_or(Type::Any);
            Type::Map(Box::new(k), Box::new(v))
        }
        "set" => {
            let arg = node
                .children()
                .find(|n| n.kind() == SyntaxKind::TypeRef)
                .map_or(Type::Any, |n| type_from_node(&n));
            Type::Set(Box::new(arg))
        }
        "object" => Type::Object,
        "function" if has_generic => {
            // `Function<P0, …, Plast => R>`. The angle-bracket args are
            // the parameter types; the FatArrow inside the *last* arg
            // separates its head (the final param) from the return type,
            // which is nested as a `TypeRef` after the `=>`.
            // `Function< => R>` has no params.
            let args: Vec<SyntaxNode> = node
                .children()
                .filter(|n| n.kind() == SyntaxKind::TypeRef)
                .collect();
            let mut params = Vec::new();
            let mut ret_ty = Type::Any;
            if let Some((last, init)) = args.split_last() {
                for a in init {
                    params.push(type_from_node(a));
                }
                let arrow = last
                    .children_with_tokens()
                    .any(|el| el.as_token().is_some_and(|t| t.kind() == SyntaxKind::FatArrow));
                if arrow {
                    // Head type-name token before the `=>` is the final
                    // param (absent for `=> R`); the nested TypeRef after
                    // is the return.
                    if let Some(head) = type_name_token_before_arrow(last) {
                        params.push(type_from_name(&head));
                    }
                    if let Some(rnode) = last
                        .children()
                        .filter(|n| n.kind() == SyntaxKind::TypeRef)
                        .last()
                    {
                        ret_ty = type_from_node(&rnode);
                    }
                } else {
                    params.push(type_from_node(last));
                }
            }
            Type::function_with(params, ret_ty)
        }
        "function" => Type::Function,
        "interval" => Type::Interval,
        _ => {
            // Anything starting with uppercase letter and not a
            // primitive we can model is treated as a class name.
            // Generic arguments (`Box<integer>`) are captured so a typed
            // binding carries them into member-access inference.
            if name.chars().next().is_some_and(|c| c.is_ascii_uppercase()) {
                let args: Vec<Type> = node
                    .children()
                    .filter(|n| n.kind() == SyntaxKind::TypeRef)
                    .map(|n| type_from_node(&n))
                    .collect();
                Type::ClassInstance(name.to_string(), args)
            } else {
                Type::Any
            }
        }
    }
}

/// Map a bare type *name* (no CST children) to a [`Type`]. Used when
/// only a head token is available (e.g. a function-type parameter
/// `string` in `Function<string => R>`); composites collapse to their
/// `Any`-argument form.
pub(crate) fn type_from_name(name: &str) -> Type {
    match name.to_ascii_lowercase().as_str() {
        "integer" | "int" => Type::Integer,
        "real" | "number" | "float" | "double" => Type::Real,
        "boolean" | "bool" => Type::Boolean,
        "string" => Type::String,
        "null" => Type::Null,
        "void" => Type::Void,
        "any" => Type::Any,
        "array" => Type::Array(Box::new(Type::Any)),
        "map" => Type::Map(Box::new(Type::Any), Box::new(Type::Any)),
        "set" => Type::Set(Box::new(Type::Any)),
        "object" => Type::Object,
        "function" => Type::Function,
        "interval" => Type::Interval,
        _ if name.chars().next().is_some_and(|c| c.is_ascii_uppercase()) => {
            Type::ClassInstance(name.to_string(), Vec::new())
        }
        _ => Type::Any,
    }
}

/// The first type-name token appearing *before* a `=>` inside a
/// function-type's last argument — the final parameter's head. `None`
/// when the argument is `=> R` (no parameter before the arrow).
fn type_name_token_before_arrow(arg: &SyntaxNode) -> Option<String> {
    let mut head: Option<String> = None;
    for el in arg.children_with_tokens() {
        let Some(tok) = el.as_token() else { continue };
        match tok.kind() {
            SyntaxKind::FatArrow => break,
            SyntaxKind::Ident | SyntaxKind::KwBoolean | SyntaxKind::KwVoid | SyntaxKind::KwNull
                if head.is_none() =>
            {
                head = Some(tok.text().to_string());
            }
            _ => {}
        }
    }
    head
}

/// Human-readable label used in diagnostic messages.
pub(crate) fn type_name(t: &Type) -> String {
    match t {
        Type::Any => "any".into(),
        Type::Null => "null".into(),
        Type::Void => "void".into(),
        Type::Boolean => "boolean".into(),
        Type::Integer => "integer".into(),
        Type::Real => "real".into(),
        Type::String => "string".into(),
        Type::Array(_) => "Array".into(),
        Type::Map(_, _) => "Map".into(),
        Type::Set(_) => "Set".into(),
        Type::Object => "Object".into(),
        Type::ClassInstance(c, args) if !args.is_empty() => {
            let inner: Vec<String> = args.iter().map(type_name).collect();
            format!("{c}<{}>", inner.join(", "))
        }
        Type::ClassInstance(c, _) => c.clone(),
        Type::Function => "function".into(),
        Type::FunctionWithReturn { params, ret } => {
            let ps: Vec<String> = params.iter().map(type_name).collect();
            format!("Function<{} => {}>", ps.join(", "), type_name(ret))
        }
        Type::Interval => "Interval".into(),
        Type::Nullable(inner) => format!("{}?", type_name(inner)),
    }
}
