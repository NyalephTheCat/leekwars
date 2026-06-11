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
    /// Arbitrary-precision integer (`big_integer`, `2L` literals).
    BigInteger,
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
    FunctionWithReturn {
        params: Vec<Type>,
        ret: Box<Type>,
    },
    /// `[a..b]` interval literal.
    Interval,
    /// `T?` — nullable wrapper. Permits `null`, but the inner type
    /// still drives coercion (`real? a = 12` stores `12.0`).
    Nullable(Box<Type>),
    /// `A | B | …` — a value that is one of the listed types. Kept in
    /// canonical form by [`Type::union_of`]: flattened (no nested
    /// unions), deduplicated, at least two members, never containing
    /// `Any`/`Null`/`Nullable` (null-ness lifts to a `Nullable`
    /// wrapper around the union), and sorted by display name so
    /// `integer | string` and `string | integer` compare equal.
    Union(Vec<Type>),
    /// **Experimental** (`LEEK_EXPERIMENTAL_TYPES`).
    /// `Array[T0, T1, …]` — an array with per-position element types,
    /// so `[1, true]` types as `Array[integer, boolean]`. At runtime
    /// it is an ordinary array; the shape exists only for checking,
    /// and a tuple is assignable wherever a plain `Array` is.
    Tuple(Vec<Type>),
}

/// Joins wider than this collapse to `Any` — keeps inferred types
/// readable and the checker's structural recursion cheap. Explicit
/// annotations are *not* capped; a user who writes five variants
/// gets five variants.
const MAX_INFERRED_UNION: usize = 4;

/// Array literals at most this long infer as tuple shapes
/// (`Array[T0, T1, …]`) under `LEEK_EXPERIMENTAL_TYPES`; longer
/// literals keep the homogeneous `Array<T>` inference — per-position
/// types past a few elements are noise, not signal.
pub(crate) const MAX_INFERRED_TUPLE: usize = 8;

impl Type {
    /// Build a `Function<params… => ret>` type.
    pub fn function_with(params: Vec<Type>, ret: Type) -> Self {
        Type::FunctionWithReturn {
            params,
            ret: Box::new(ret),
        }
    }

    /// Canonicalizing union constructor. Flattens nested unions,
    /// deduplicates members, lifts `Null`/`Nullable` members into an
    /// outer `Nullable` wrapper, short-circuits to `Any` when any
    /// member is `Any`, and sorts members by display name so member
    /// order never affects equality. Zero distinct members yield
    /// `Null` (all-null) or `Any` (empty input); a single member
    /// yields that member unwrapped.
    pub fn union_of(members: Vec<Type>) -> Type {
        fn add(t: Type, flat: &mut Vec<Type>, has_null: &mut bool, has_any: &mut bool) {
            match t {
                Type::Any => *has_any = true,
                Type::Null => *has_null = true,
                Type::Nullable(inner) => {
                    *has_null = true;
                    add(*inner, flat, has_null, has_any);
                }
                Type::Union(ms) => {
                    for m in ms {
                        add(m, flat, has_null, has_any);
                    }
                }
                other => {
                    if !flat.contains(&other) {
                        flat.push(other);
                    }
                }
            }
        }
        let mut flat = Vec::new();
        let (mut has_null, mut has_any) = (false, false);
        for m in members {
            add(m, &mut flat, &mut has_null, &mut has_any);
        }
        if has_any {
            return Type::Any;
        }
        flat.sort_by_key(type_name);
        let inner = match flat.len() {
            0 => return if has_null { Type::Null } else { Type::Any },
            1 => flat.pop().expect("len checked"),
            _ => Type::Union(flat),
        };
        if has_null { nullable_of(&inner) } else { inner }
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
        // A union source must fit wholly: every member assignable.
        // Decomposed BEFORE the expected side so `A|B → A|B` checks
        // member-by-member rather than demanding all of `A|B` fit `A`.
        if let Type::Union(members) = actual {
            return members.iter().all(|m| Type::assignable_to(m, expected));
        }
        // A union target accepts a value fitting any one member.
        if let Type::Union(members) = expected {
            return members.iter().any(|m| Type::assignable_to(actual, m));
        }
        match (actual, expected) {
            (Type::Any, _) | (_, Type::Any) => true,
            // Tuple-shaped arrays: position-wise against another tuple;
            // member-wise against a plain `Array<T>` (a tuple *is* an
            // array). A plain `Array` is NOT assignable to a tuple —
            // its length and per-position types are unknown, and that
            // strictness is the point of the shape.
            (Type::Tuple(a), Type::Tuple(b)) => {
                a.len() == b.len()
                    && a.iter()
                        .zip(b.iter())
                        .all(|(x, y)| Type::assignable_to(x, y))
            }
            (Type::Tuple(ms), Type::Array(el)) => ms.iter().all(|m| Type::assignable_to(m, el)),
            // Null is universally assignable in dynamic semantics.
            (Type::Null, _) | (_, Type::Null) => true,
            // Numeric crosses are permitted (per type-system.md §5.1):
            // Integer ↔ Real, and either into/out of `big_integer`
            // (assignment to a `big_integer` slot coerces, truncating reals).
            (
                Type::Integer | Type::Real | Type::BigInteger,
                Type::Integer | Type::Real | Type::BigInteger,
            ) => true,
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
            (Type::FunctionWithReturn { ret: a, .. }, Type::FunctionWithReturn { ret: b, .. }) => {
                Type::assignable_to(a, b)
            }
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

/// Wrap `t` as nullable, collapsing `Null`/`Any`/already-nullable cases.
pub(crate) fn nullable_of(t: &Type) -> Type {
    match t {
        Type::Null => Type::Null,
        // `any` already admits null — wrapping would only add noise.
        Type::Any => Type::Any,
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

/// Collapse an *inferred* union that grew too wide back to `Any`.
/// Applied to `unify_types` joins only — explicit annotations keep
/// however many members the user wrote.
fn cap_union(t: Type) -> Type {
    let width = match &t {
        Type::Union(ms) => ms.len(),
        Type::Nullable(inner) => match inner.as_ref() {
            Type::Union(ms) => ms.len(),
            _ => 0,
        },
        _ => 0,
    };
    if width > MAX_INFERRED_UNION {
        Type::Any
    } else {
        t
    }
}

/// Join two branch types (ternary arms, narrowed merges, container
/// element joins). Equal types collapse; `int`/`real` promote to
/// `real`; `null` + `T` becomes `T?`; distinct types form a bounded
/// union (wider than [`MAX_INFERRED_UNION`] widens to `any`).
pub(crate) fn unify_types(a: &Type, b: &Type) -> Type {
    if a == b {
        return a.clone();
    }
    match (a, b) {
        (Type::Any, _) | (_, Type::Any) => Type::Any,
        (Type::Integer, Type::Real) | (Type::Real, Type::Integer) => Type::Real,
        (Type::Null, t) | (t, Type::Null) => nullable_of(t),
        (Type::Nullable(x), y) | (y, Type::Nullable(x)) => {
            cap_union(nullable_of(&unify_types(x, y)))
        }
        // Same-length tuple shapes join position-wise; everything
        // else (length mismatch, tuple vs plain array) goes through
        // the bounded-union fallback below.
        (Type::Tuple(x), Type::Tuple(y)) if x.len() == y.len() => {
            Type::Tuple(x.iter().zip(y).map(|(m, n)| unify_types(m, n)).collect())
        }
        _ => cap_union(Type::union_of(vec![a.clone(), b.clone()])),
    }
}

/// Numeric-promotion result for binary `+`/`-`/`*`/`/`/`%`/`**`.
/// String concatenation is handled by the caller before invoking
/// this — `Plus` with a string operand short-circuits to String.
pub(crate) fn promote_numeric(lhs: &Type, rhs: &Type) -> Type {
    use Type::{Any, BigInteger, Boolean, Integer, Real};
    match (lhs, rhs) {
        // big_integer dominates every numeric mix — upstream's
        // `BigIntegerValue` checks run before the `Double` check, so
        // even `2L + 0.5` stays a (truncated) big_integer.
        (BigInteger, _) | (_, BigInteger) => BigInteger,
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
        for tok in list
            .children_with_tokens()
            .filter_map(rowan::NodeOrToken::into_token)
        {
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
        for tok in list
            .children_with_tokens()
            .filter_map(rowan::NodeOrToken::into_token)
        {
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
        // Unions don't participate in generic-variable binding; resolve
        // the whole annotation as a concrete (union) type instead.
        return GType::Concrete(type_from_node(node));
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
            .map_or(GType::Concrete(Type::Any), |c| {
                gtype_from_node(&c, typarams)
            })
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
    // Union types parse FLAT: `Array<integer> | string` is ONE
    // `TypeRef` whose direct tokens are the member heads separated by
    // `Pipe`, with each member's generic args nested as child
    // `TypeRef`s. Split the element stream at top-level pipes and
    // resolve each segment independently.
    let elements: Vec<leek_syntax::SyntaxElement> = node.children_with_tokens().collect();
    let has_pipe = elements
        .iter()
        .any(|el| el.as_token().is_some_and(|t| t.kind() == SyntaxKind::Pipe));
    if has_pipe {
        let mut segments: Vec<Vec<leek_syntax::SyntaxElement>> = vec![Vec::new()];
        for el in elements {
            if el.as_token().is_some_and(|t| t.kind() == SyntaxKind::Pipe) {
                segments.push(Vec::new());
            } else {
                segments.last_mut().expect("non-empty").push(el);
            }
        }
        let members: Vec<Type> = segments.iter().map(|s| type_from_segment(s)).collect();
        return Type::union_of(members);
    }
    let has_nullable = elements.iter().any(|el| {
        el.as_token()
            .is_some_and(|t| t.kind() == SyntaxKind::Question)
    });
    let inner = type_from_node_primitive(node);
    if has_nullable {
        return nullable_of(&inner);
    }
    inner
}

/// Resolve one pipe-separated segment of a flat union `TypeRef`: the
/// segment's head token names the type, nested `TypeRef` children in
/// the segment are its generic arguments, and a trailing `?` makes
/// the member nullable (lifted to the whole union by `union_of`).
fn type_from_segment(seg: &[leek_syntax::SyntaxElement]) -> Type {
    let has_nullable = seg.iter().any(|el| {
        el.as_token()
            .is_some_and(|t| t.kind() == SyntaxKind::Question)
    });
    let head = seg.iter().filter_map(|el| el.as_token()).find(|t| {
        matches!(
            t.kind(),
            SyntaxKind::Ident | SyntaxKind::KwBoolean | SyntaxKind::KwVoid | SyntaxKind::KwNull
        )
    });
    let Some(head) = head else {
        return Type::Any;
    };
    let args: Vec<SyntaxNode> = seg
        .iter()
        .filter_map(|el| el.as_node())
        .filter(|n| n.kind() == SyntaxKind::TypeRef)
        .cloned()
        .collect();
    // `Array[T0, T1, …]` (experimental tuple shape): the parser keeps
    // the square brackets as direct tokens, which is what tells the
    // shape apart from `<…>` generic arguments.
    let has_bracket = seg.iter().any(|el| {
        el.as_token()
            .is_some_and(|t| t.kind() == SyntaxKind::LBracket)
    });
    let inner = if has_bracket {
        Type::Tuple(args.iter().map(type_from_node).collect())
    } else {
        type_from_parts(head.text(), &args)
    };
    if has_nullable {
        nullable_of(&inner)
    } else {
        inner
    }
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
    let args: Vec<SyntaxNode> = node
        .children()
        .filter(|n| n.kind() == SyntaxKind::TypeRef)
        .collect();
    // `Array[T0, T1, …]` (experimental tuple shape): the parser keeps
    // the square brackets as direct tokens, which is what tells the
    // shape apart from `<…>` generic arguments.
    let has_bracket = node
        .children_with_tokens()
        .filter_map(rowan::NodeOrToken::into_token)
        .any(|t| t.kind() == SyntaxKind::LBracket);
    if has_bracket {
        return Type::Tuple(args.iter().map(type_from_node).collect());
    }
    type_from_parts(head.text(), &args)
}

/// Resolve a type from its head *name* and generic-argument nodes.
/// Shared by whole-`TypeRef` resolution and per-segment union-member
/// resolution (where the head and args come from an element slice
/// rather than a node).
fn type_from_parts(name: &str, args: &[SyntaxNode]) -> Type {
    let has_generic = !args.is_empty();
    let lower = name.to_ascii_lowercase();
    match lower.as_str() {
        "integer" | "int" => Type::Integer,
        "real" | "number" | "float" | "double" => Type::Real,
        "big_integer" => Type::BigInteger,
        "boolean" | "bool" => Type::Boolean,
        "string" => Type::String,
        "null" => Type::Null,
        "void" => Type::Void,
        "any" => Type::Any,
        "array" if has_generic => {
            let arg = args.first().map_or(Type::Any, type_from_node);
            Type::Array(Box::new(arg))
        }
        "array" => Type::Array(Box::new(Type::Any)),
        "map" => {
            let k = args.first().map_or(Type::Any, type_from_node);
            let v = args.get(1).map_or(Type::Any, type_from_node);
            Type::Map(Box::new(k), Box::new(v))
        }
        "set" => {
            let arg = args.first().map_or(Type::Any, type_from_node);
            Type::Set(Box::new(arg))
        }
        "object" => Type::Object,
        "function" if has_generic => {
            // `Function<P0, …, Plast => R>`. The angle-bracket args are
            // the parameter types; the FatArrow inside the *last* arg
            // separates its head (the final param) from the return type,
            // which is nested as a `TypeRef` after the `=>`.
            // `Function< => R>` has no params.
            let mut params = Vec::new();
            let mut ret_ty = Type::Any;
            if let Some((last, init)) = args.split_last() {
                for a in init {
                    params.push(type_from_node(a));
                }
                let arrow = last.children_with_tokens().any(|el| {
                    el.as_token()
                        .is_some_and(|t| t.kind() == SyntaxKind::FatArrow)
                });
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
                let targs: Vec<Type> = args.iter().map(type_from_node).collect();
                Type::ClassInstance(name.to_string(), targs)
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
        "big_integer" => Type::BigInteger,
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
        Type::BigInteger => "big_integer".into(),
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
        Type::Nullable(inner) => match inner.as_ref() {
            // Spell nullable unions `A | B | null` — clearer than the
            // equally-parseable `A | B?`, where the `?` visually binds
            // to the last member even though null-ness is union-wide.
            Type::Union(_) => format!("{} | null", type_name(inner)),
            _ => format!("{}?", type_name(inner)),
        },
        Type::Union(members) => {
            let names: Vec<String> = members.iter().map(type_name).collect();
            names.join(" | ")
        }
        Type::Tuple(members) => {
            let names: Vec<String> = members.iter().map(type_name).collect();
            format!("Array[{}]", names.join(", "))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn union_of_canonicalizes_order_and_dupes() {
        let a = Type::union_of(vec![Type::String, Type::Integer, Type::String]);
        let b = Type::union_of(vec![Type::Integer, Type::String]);
        assert_eq!(a, b);
        assert_eq!(type_name(&a), "integer | string");
    }

    #[test]
    fn union_of_flattens_nested_and_lifts_null() {
        let inner = Type::union_of(vec![Type::Integer, Type::Boolean]);
        let t = Type::union_of(vec![inner, Type::Null, Type::String]);
        assert_eq!(
            t,
            Type::Nullable(Box::new(Type::Union(vec![
                Type::Boolean,
                Type::Integer,
                Type::String,
            ])))
        );
        assert_eq!(type_name(&t), "boolean | integer | string | null");
    }

    #[test]
    fn union_of_collapses_degenerate_forms() {
        assert_eq!(Type::union_of(vec![Type::Integer]), Type::Integer);
        assert_eq!(
            Type::union_of(vec![Type::Integer, Type::Any]),
            Type::Any,
            "any absorbs the union"
        );
        assert_eq!(Type::union_of(vec![Type::Null, Type::Null]), Type::Null);
        // A nullable member lifts its null-ness to the whole union —
        // `A | B?` and `A? | B` both mean `(A | B) | null`.
        assert_eq!(
            Type::union_of(vec![Type::Integer, Type::Nullable(Box::new(Type::String))]),
            Type::union_of(vec![Type::Nullable(Box::new(Type::Integer)), Type::String]),
        );
    }

    #[test]
    fn union_assignability_decomposes_actual_first() {
        let int_or_str = Type::union_of(vec![Type::Integer, Type::String]);
        // Member into union.
        assert!(Type::assignable_to(&Type::Integer, &int_or_str));
        assert!(!Type::assignable_to(&Type::Boolean, &int_or_str));
        // Union into itself / into a wider union.
        assert!(Type::assignable_to(&int_or_str, &int_or_str));
        let wider = Type::union_of(vec![Type::Integer, Type::String, Type::Boolean]);
        assert!(Type::assignable_to(&int_or_str, &wider));
        assert!(!Type::assignable_to(&wider, &int_or_str));
        // Union into a single member only if every member fits.
        assert!(!Type::assignable_to(&int_or_str, &Type::Integer));
    }

    #[test]
    fn unify_joins_to_bounded_union() {
        assert_eq!(
            unify_types(&Type::Integer, &Type::String),
            Type::union_of(vec![Type::Integer, Type::String])
        );
        // int/real still promote rather than union.
        assert_eq!(unify_types(&Type::Integer, &Type::Real), Type::Real);
        // Joins wider than MAX_INFERRED_UNION collapse to any.
        let mut t = Type::Integer;
        for next in [
            Type::String,
            Type::Boolean,
            Type::Object,
            Type::Interval,
            Type::Function,
        ] {
            t = unify_types(&t, &next);
        }
        assert_eq!(t, Type::Any);
    }

    #[test]
    fn unify_nullable_unions_through_wrapper() {
        let t = unify_types(&Type::Nullable(Box::new(Type::Integer)), &Type::String);
        assert_eq!(
            t,
            Type::Nullable(Box::new(Type::union_of(vec![Type::Integer, Type::String])))
        );
    }
}
