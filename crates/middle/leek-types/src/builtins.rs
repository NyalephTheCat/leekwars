//! Per-builtin type signatures used by strict-mode
//! WRONG_ARGUMENT_TYPE detection. Intentionally narrow — only the
//! builtins we've seen mismatch cases for. New entries should land
//! with their upstream test reference.

use crate::ty::Type;

bitflags::bitflags! {
    /// Per-parameter "allowed types" bitset. We don't model full
    /// signatures yet; instead a small set of common builtins lists
    /// the type categories their args must fall into. One bit per
    /// category; compose sets with `|`.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub(crate) struct TypeSet: u16 {
        const INTEGER = 1 << 0;
        const REAL = 1 << 1;
        const BOOLEAN = 1 << 2;
        const STRING = 1 << 3;
        const NULL = 1 << 4;
        const ARRAY = 1 << 5;
        const MAP = 1 << 6;
        const SET = 1 << 7;
        const OBJECT = 1 << 8;
        const FUNCTION = 1 << 9;
        /// Accepts everything — types we don't categorize (class
        /// instances, intervals) only pass when this bit is set.
        const ANY = 1 << 10;

        /// `boolean` counts as numeric: Leekscript coerces it to 0/1.
        const NUMERIC = Self::INTEGER.bits() | Self::REAL.bits() | Self::BOOLEAN.bits();
        const CONTAINER = Self::ARRAY.bits() | Self::MAP.bits() | Self::SET.bits();
        const ARRAY_ONLY = Self::ARRAY.bits();
    }
}

pub(crate) fn type_in_set(t: &Type, s: TypeSet) -> bool {
    if s.contains(TypeSet::ANY) {
        return true;
    }
    match t {
        Type::Any => true,
        Type::Integer => s.contains(TypeSet::INTEGER),
        Type::Real => s.contains(TypeSet::REAL),
        // big_integer flows wherever the numeric family does.
        Type::BigInteger => s.intersects(TypeSet::NUMERIC),
        Type::Boolean => s.contains(TypeSet::BOOLEAN),
        Type::String => s.contains(TypeSet::STRING),
        Type::Null | Type::Void => s.contains(TypeSet::NULL),
        // A tuple-shaped array is an ordinary array at runtime.
        Type::Array(_) | Type::Tuple(_) => s.contains(TypeSet::ARRAY),
        Type::Map(_, _) => s.contains(TypeSet::MAP),
        Type::Set(_) => s.contains(TypeSet::SET),
        Type::Object => s.contains(TypeSet::OBJECT),
        Type::Function | Type::FunctionWithReturn { .. } => s.contains(TypeSet::FUNCTION),
        Type::ClassInstance(..) | Type::Interval => false,
        // Nullable types accept whatever their inner type accepts
        // (plus null, but null is always accepted via Type::Null).
        Type::Nullable(inner) => type_in_set(inner, s),
        // A union fits when at least one member could fit at
        // runtime — flagging only certain mismatches keeps this
        // check false-positive-free.
        Type::Union(members) => members.iter().any(|m| type_in_set(m, s)),
    }
}

pub(crate) fn describe_type_set(s: TypeSet) -> String {
    let mut parts = Vec::new();
    if s.intersects(TypeSet::NUMERIC) {
        parts.push("number");
    }
    if s.contains(TypeSet::STRING) {
        parts.push("string");
    }
    if s.contains(TypeSet::ARRAY) {
        parts.push("Array");
    }
    if s.contains(TypeSet::MAP) {
        parts.push("Map");
    }
    if s.contains(TypeSet::SET) {
        parts.push("Set");
    }
    if s.contains(TypeSet::OBJECT) {
        parts.push("Object");
    }
    if s.contains(TypeSet::FUNCTION) {
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
