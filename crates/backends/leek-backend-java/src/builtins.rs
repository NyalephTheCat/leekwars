//! Map from Leekscript builtin function name to the upstream
//! `*Class.java` it lives on. The reference emits builtin calls as
//! `<Class>.<name>(this, args...)` — matching that shape here lets
//! our emitted Java compile cleanly against the same runtime jars.
//!
//! There are two dispatch shapes:
//!
//! 1. **Static** — `<Class>.<name>(this, args...)`. Mirrors the
//!    upstream `system_function.isStatic()` path in
//!    `LeekFunctionCall.compileL`. Used for `StringClass.length`,
//!    `NumberClass.abs`, etc. — names registered with `isStatic=true`
//!    in `LeekFunctions.java`. The static rows live in `builtins.tsv`.
//!
//! 2. **Receiver** — `((<class>) arg0).<name>(this, args[1..])`.
//!    Mirrors the upstream "not isStatic" path that cast-dispatches
//!    on the receiver's static type. Used for `push`, `count`,
//!    `arrayMap`, and most `Array`/`Map`/`Set`/`Interval` builtins.
//!    The receiver class differs between v4 (`ArrayLeekValue`,
//!    `MapLeekValue`, …) and v1–v3 (`LegacyArrayLeekValue`,
//!    which stands in for both Array and Map pre-v4). These rows
//!    live inline in [`RECEIVER_TABLE`].
//!
//! The static TSV lives in `crates/core/leek-builtins/builtins.tsv` and
//! is shared with resolver lookup via `leek-builtins`. Regenerate after
//! an upstream change:
//!
//! ```text
//! tools/builtin-extract.sh --write
//! ```
//!
//! Names not present in either table fall back to a bare `name(...)`
//! call so the surrounding code at least compiles when AI exposes
//! the name as an instance method.

use leek_syntax::Version;

/// Dispatch shape for a builtin function call.
pub enum Dispatch {
    /// `<class>.<name>(this, args...)` — utility class with static methods.
    Static { class: &'static str },
    /// `((<class>) arg0).<name>(this, args[1..])` — instance method on
    /// the receiver. `v4_class` is the value-class name at v4;
    /// `legacy_class` is used at v1–v3 (where Array and Map both lower
    /// to `LegacyArrayLeekValue`).
    Receiver {
        v4_class: &'static str,
        legacy_class: &'static str,
    },
}

/// One row of the lookup table: dispatch shape + arg-coercion hint.
pub struct Builtin {
    pub dispatch: Dispatch,
    /// True when at least one overload's first non-AI parameter is
    /// `long` / `int` — drives `((Number) X).longValue()` casts for
    /// the Static dispatch shape. Ignored for Receiver dispatch.
    pub prefer_long: bool,
}

impl Builtin {
    /// Resolve the dispatch shape to a concrete (class, is_receiver) pair
    /// for the given version. Hides the v4/legacy fork at call sites.
    pub fn resolved_class(&self, version: Version) -> (&'static str, bool) {
        match self.dispatch {
            Dispatch::Static { class } => (class, false),
            Dispatch::Receiver {
                v4_class,
                legacy_class,
            } => {
                if matches!(version, Version::V4) {
                    (v4_class, true)
                } else {
                    (legacy_class, true)
                }
            }
        }
    }
}

/// Receiver-dispatched builtins. The receiver is `args[0]`; remaining
/// args are passed through after the `this` AI reference.
///
/// Sourced from `LeekFunctions.java` — every `method("X", "Array", …)`
/// / `"Map"` / `"Set"` / `"Interval"` registration that doesn't pass
/// `true` for `isStatic`. The `prefer_long` field is left at false:
/// the receiver-method overloads in `ArrayLeekValue` / etc. accept
/// `Object` for value parameters, so the coercion logic that's used
/// for `NumberClass.abs` doesn't apply.
const RECEIVER_TABLE: &[(&str, &str, &str)] = &[
    // ─── Array ────────────────────────────────────────────────────────
    ("push", "ArrayLeekValue", "LegacyArrayLeekValue"),
    ("pushAll", "ArrayLeekValue", "LegacyArrayLeekValue"),
    ("unshift", "ArrayLeekValue", "LegacyArrayLeekValue"),
    ("shift", "ArrayLeekValue", "LegacyArrayLeekValue"),
    ("pop", "ArrayLeekValue", "LegacyArrayLeekValue"),
    ("insert", "ArrayLeekValue", "LegacyArrayLeekValue"),
    ("remove", "ArrayLeekValue", "LegacyArrayLeekValue"),
    ("removeElement", "ArrayLeekValue", "LegacyArrayLeekValue"),
    ("arrayRemoveAll", "ArrayLeekValue", "LegacyArrayLeekValue"),
    ("count", "ArrayLeekValue", "LegacyArrayLeekValue"),
    ("join", "ArrayLeekValue", "LegacyArrayLeekValue"),
    ("sort", "ArrayLeekValue", "LegacyArrayLeekValue"),
    ("shuffle", "ArrayLeekValue", "LegacyArrayLeekValue"),
    ("search", "ArrayLeekValue", "LegacyArrayLeekValue"),
    ("inArray", "ArrayLeekValue", "LegacyArrayLeekValue"),
    ("reverse", "ArrayLeekValue", "LegacyArrayLeekValue"),
    ("arrayMin", "ArrayLeekValue", "LegacyArrayLeekValue"),
    ("arrayMax", "ArrayLeekValue", "LegacyArrayLeekValue"),
    ("sum", "ArrayLeekValue", "LegacyArrayLeekValue"),
    ("average", "ArrayLeekValue", "LegacyArrayLeekValue"),
    ("fill", "ArrayLeekValue", "LegacyArrayLeekValue"),
    ("isEmpty", "ArrayLeekValue", "LegacyArrayLeekValue"),
    ("subArray", "ArrayLeekValue", "LegacyArrayLeekValue"),
    ("arraySlice", "ArrayLeekValue", "LegacyArrayLeekValue"),
    ("arrayMap", "ArrayLeekValue", "LegacyArrayLeekValue"),
    ("arrayFilter", "ArrayLeekValue", "LegacyArrayLeekValue"),
    ("arrayFind", "ArrayLeekValue", "LegacyArrayLeekValue"),
    ("arrayFlatten", "ArrayLeekValue", "LegacyArrayLeekValue"),
    ("arrayFoldLeft", "ArrayLeekValue", "LegacyArrayLeekValue"),
    ("arrayFoldRight", "ArrayLeekValue", "LegacyArrayLeekValue"),
    ("arrayPartition", "ArrayLeekValue", "LegacyArrayLeekValue"),
    ("arrayIter", "ArrayLeekValue", "LegacyArrayLeekValue"),
    ("arrayConcat", "ArrayLeekValue", "LegacyArrayLeekValue"),
    ("arraySort", "ArrayLeekValue", "LegacyArrayLeekValue"),
    ("arraySome", "ArrayLeekValue", "LegacyArrayLeekValue"),
    ("arrayEvery", "ArrayLeekValue", "LegacyArrayLeekValue"),
    ("arrayGet", "ArrayLeekValue", "LegacyArrayLeekValue"),
    ("arrayRandom", "ArrayLeekValue", "LegacyArrayLeekValue"),
    ("arrayFrequencies", "ArrayLeekValue", "LegacyArrayLeekValue"),
    ("arrayChunk", "ArrayLeekValue", "LegacyArrayLeekValue"),
    ("arrayUnique", "ArrayLeekValue", "LegacyArrayLeekValue"),
    ("arrayClear", "ArrayLeekValue", "LegacyArrayLeekValue"),
    ("arrayToSet", "ArrayLeekValue", "LegacyArrayLeekValue"),
    ("assocReverse", "ArrayLeekValue", "LegacyArrayLeekValue"),
    // ─── Map (v4 only; v1–v3 Maps lower to LegacyArrayLeekValue) ─────
    ("mapSize", "MapLeekValue", "LegacyArrayLeekValue"),
    ("mapIsEmpty", "MapLeekValue", "LegacyArrayLeekValue"),
    ("mapClear", "MapLeekValue", "LegacyArrayLeekValue"),
    ("mapGet", "MapLeekValue", "LegacyArrayLeekValue"),
    ("mapValues", "MapLeekValue", "LegacyArrayLeekValue"),
    ("mapKeys", "MapLeekValue", "LegacyArrayLeekValue"),
    ("mapIter", "MapLeekValue", "LegacyArrayLeekValue"),
    ("mapMap", "MapLeekValue", "LegacyArrayLeekValue"),
    ("mapSum", "MapLeekValue", "LegacyArrayLeekValue"),
    ("mapAverage", "MapLeekValue", "LegacyArrayLeekValue"),
    ("mapMin", "MapLeekValue", "LegacyArrayLeekValue"),
    ("mapMax", "MapLeekValue", "LegacyArrayLeekValue"),
    ("mapSearch", "MapLeekValue", "LegacyArrayLeekValue"),
    ("mapContains", "MapLeekValue", "LegacyArrayLeekValue"),
    ("mapContainsKey", "MapLeekValue", "LegacyArrayLeekValue"),
    ("mapRemove", "MapLeekValue", "LegacyArrayLeekValue"),
    ("mapRemoveAll", "MapLeekValue", "LegacyArrayLeekValue"),
    ("mapReplace", "MapLeekValue", "LegacyArrayLeekValue"),
    ("mapReplaceAll", "MapLeekValue", "LegacyArrayLeekValue"),
    ("mapFill", "MapLeekValue", "LegacyArrayLeekValue"),
    ("mapEvery", "MapLeekValue", "LegacyArrayLeekValue"),
    ("mapSome", "MapLeekValue", "LegacyArrayLeekValue"),
    ("mapFold", "MapLeekValue", "LegacyArrayLeekValue"),
    ("mapFilter", "MapLeekValue", "LegacyArrayLeekValue"),
    ("mapMerge", "MapLeekValue", "LegacyArrayLeekValue"),
    ("mapPut", "MapLeekValue", "LegacyArrayLeekValue"),
    ("mapPutAll", "MapLeekValue", "LegacyArrayLeekValue"),
    ("assocSort", "ArrayLeekValue", "LegacyArrayLeekValue"),
    ("keySort", "ArrayLeekValue", "LegacyArrayLeekValue"),
    ("removeKey", "MapLeekValue", "LegacyArrayLeekValue"),
    // ─── Set (v4 only — but emit the cast either way; v1–v3 won't
    //          parse `<a,b,c>` so these can only appear at v4) ─────────
    ("setPut", "SetLeekValue", "SetLeekValue"),
    ("setRemove", "SetLeekValue", "SetLeekValue"),
    ("setClear", "SetLeekValue", "SetLeekValue"),
    ("setContains", "SetLeekValue", "SetLeekValue"),
    ("setSize", "SetLeekValue", "SetLeekValue"),
    ("setIsEmpty", "SetLeekValue", "SetLeekValue"),
    ("setIsSubsetOf", "SetLeekValue", "SetLeekValue"),
    ("setUnion", "SetLeekValue", "SetLeekValue"),
    ("setIntersection", "SetLeekValue", "SetLeekValue"),
    ("setDifference", "SetLeekValue", "SetLeekValue"),
    ("setDisjunction", "SetLeekValue", "SetLeekValue"),
    ("setFilter", "SetLeekValue", "SetLeekValue"),
    ("setToArray", "SetLeekValue", "SetLeekValue"),
    // ─── Interval ────────────────────────────────────────────────────
    ("intervalMin", "IntervalLeekValue", "IntervalLeekValue"),
    ("intervalMax", "IntervalLeekValue", "IntervalLeekValue"),
    ("intervalSize", "IntervalLeekValue", "IntervalLeekValue"),
    ("intervalIsEmpty", "IntervalLeekValue", "IntervalLeekValue"),
    (
        "intervalIsBounded",
        "IntervalLeekValue",
        "IntervalLeekValue",
    ),
    (
        "intervalIsRightBounded",
        "IntervalLeekValue",
        "IntervalLeekValue",
    ),
    (
        "intervalIsLeftBounded",
        "IntervalLeekValue",
        "IntervalLeekValue",
    ),
    ("intervalIsClosed", "IntervalLeekValue", "IntervalLeekValue"),
    (
        "intervalIsRightClosed",
        "IntervalLeekValue",
        "IntervalLeekValue",
    ),
    (
        "intervalIsLeftClosed",
        "IntervalLeekValue",
        "IntervalLeekValue",
    ),
    ("intervalContains", "IntervalLeekValue", "IntervalLeekValue"),
    ("intervalAverage", "IntervalLeekValue", "IntervalLeekValue"),
    (
        "intervalIntersection",
        "IntervalLeekValue",
        "IntervalLeekValue",
    ),
    ("intervalCombine", "IntervalLeekValue", "IntervalLeekValue"),
    ("intervalToArray", "IntervalLeekValue", "IntervalLeekValue"),
    ("intervalToSet", "IntervalLeekValue", "IntervalLeekValue"),
];

/// Look up `name` and return its dispatch shape + arg-coercion hint.
/// `None` for unknown names — caller falls back to a bare AI-instance
/// call.
pub fn lookup(name: &str) -> Option<Builtin> {
    for &(n, v4_class, legacy_class) in RECEIVER_TABLE {
        if n == name {
            return Some(Builtin {
                dispatch: Dispatch::Receiver {
                    v4_class,
                    legacy_class,
                },
                prefer_long: false,
            });
        }
    }
    if let Some(row) = leek_builtins::lookup_java(name) {
        return Some(Builtin {
            dispatch: Dispatch::Static {
                class: row.java_class,
            },
            prefer_long: leek_builtins::java_prefer_long(name),
        });
    }
    None
}

/// Legacy entry point: just the class name in the static case. Kept
/// for any callers that don't need the full dispatch shape.
#[allow(dead_code)]
pub fn class_of(name: &str) -> Option<&'static str> {
    match lookup(name)?.dispatch {
        Dispatch::Static { class } => Some(class),
        Dispatch::Receiver { v4_class, .. } => Some(v4_class),
    }
}
