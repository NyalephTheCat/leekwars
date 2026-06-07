//! Per-builtin type signatures used by strict-mode
//! WRONG_ARGUMENT_TYPE detection. Intentionally narrow — only the
//! builtins we've seen mismatch cases for. New entries should land
//! with their upstream test reference.

use crate::ty::Type;

/// Per-parameter "allowed types" bitmask. We don't model full
/// signatures yet; instead a small set of common builtins lists
/// the type categories their args must fall into.
#[derive(Debug, Clone, Copy)]
pub(crate) struct TypeSet {
    pub(crate) accept_any: bool,
    pub(crate) accept_integer: bool,
    pub(crate) accept_real: bool,
    pub(crate) accept_boolean: bool,
    pub(crate) accept_string: bool,
    pub(crate) accept_null: bool,
    pub(crate) accept_array: bool,
    pub(crate) accept_map: bool,
    pub(crate) accept_set: bool,
    pub(crate) accept_object: bool,
    pub(crate) accept_function: bool,
}

impl TypeSet {
    pub(crate) const NUMERIC: TypeSet = TypeSet {
        accept_any: false,
        accept_integer: true,
        accept_real: true,
        accept_boolean: true,
        accept_string: false,
        accept_null: false,
        accept_array: false,
        accept_map: false,
        accept_set: false,
        accept_object: false,
        accept_function: false,
    };
    pub(crate) const CONTAINER: TypeSet = TypeSet {
        accept_any: false,
        accept_integer: false,
        accept_real: false,
        accept_boolean: false,
        accept_string: false,
        accept_null: false,
        accept_array: true,
        accept_map: true,
        accept_set: true,
        accept_object: false,
        accept_function: false,
    };
    pub(crate) const ARRAY_ONLY: TypeSet = TypeSet {
        accept_any: false,
        accept_integer: false,
        accept_real: false,
        accept_boolean: false,
        accept_string: false,
        accept_null: false,
        accept_array: true,
        accept_map: false,
        accept_set: false,
        accept_object: false,
        accept_function: false,
    };
}

pub(crate) fn type_in_set(t: &Type, s: &TypeSet) -> bool {
    match t {
        Type::Any => true,
        Type::Integer => s.accept_integer || s.accept_any,
        Type::Real => s.accept_real || s.accept_any,
        Type::Boolean => s.accept_boolean || s.accept_any,
        Type::String => s.accept_string || s.accept_any,
        Type::Null => s.accept_null || s.accept_any,
        Type::Void => s.accept_null || s.accept_any,
        Type::Array(_) => s.accept_array || s.accept_any,
        Type::Map(_, _) => s.accept_map || s.accept_any,
        Type::Set(_) => s.accept_set || s.accept_any,
        Type::Object => s.accept_object || s.accept_any,
        Type::Function | Type::FunctionWithReturn { .. } => s.accept_function || s.accept_any,
        Type::ClassInstance(..) | Type::Interval => s.accept_any,
        // Nullable types accept whatever their inner type accepts
        // (plus null, but null is always accepted via Type::Null).
        Type::Nullable(inner) => type_in_set(inner, s),
    }
}

pub(crate) fn describe_type_set(s: &TypeSet) -> String {
    let mut parts = Vec::new();
    if s.accept_integer || s.accept_real || s.accept_boolean {
        parts.push("number");
    }
    if s.accept_string {
        parts.push("string");
    }
    if s.accept_array {
        parts.push("Array");
    }
    if s.accept_map {
        parts.push("Map");
    }
    if s.accept_set {
        parts.push("Set");
    }
    if s.accept_object {
        parts.push("Object");
    }
    if s.accept_function {
        parts.push("function");
    }
    if parts.is_empty() {
        return "any".into();
    }
    parts.join("/")
}

pub(crate) struct BuiltinSig {
    pub(crate) name: &'static str,
    pub(crate) params: &'static [TypeSet],
    /// Minimum language version where this stricter signature
    /// applies. Earlier versions accept a wider type set
    /// (e.g. v1-v3 `count` accepts Map; v4 doesn't).
    pub(crate) min_version: u8,
}

/// Tiny signature table — intentionally narrow, just enough to fire
/// the upstream WRONG_ARGUMENT_TYPE cases without false positives.
/// Expanded as more cases come in.
pub(crate) const BUILTIN_SIGS: &[BuiltinSig] = &[
    // v4 tightens `count()` to Array only — v1-v3 still accept the
    // wider CONTAINER (Map / Set). Both entries fire in strict mode;
    // the lookup picks the highest min_version ≤ self.version.
    BuiltinSig {
        name: "count",
        params: &[TypeSet::CONTAINER],
        min_version: 1,
    },
    BuiltinSig {
        name: "count",
        params: &[TypeSet::ARRAY_ONLY],
        min_version: 4,
    },
    BuiltinSig {
        name: "abs",
        params: &[TypeSet::NUMERIC],
        min_version: 1,
    },
];
