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
///
/// **Rows are sorted by name** — [`lookup`] binary-searches them, so an
/// insertion in the wrong place would make that name unfindable. The
/// value class tells you which family a row belongs to (`Array`, `Map`,
/// `Set`, `Interval`); `receiver_table_is_sorted` guards the ordering.
const RECEIVER_TABLE: &[(&str, &str, &str)] = &[
    ("arrayChunk", "ArrayLeekValue", "LegacyArrayLeekValue"),
    ("arrayClear", "ArrayLeekValue", "LegacyArrayLeekValue"),
    ("arrayConcat", "ArrayLeekValue", "LegacyArrayLeekValue"),
    ("arrayEvery", "ArrayLeekValue", "LegacyArrayLeekValue"),
    ("arrayFilter", "ArrayLeekValue", "LegacyArrayLeekValue"),
    ("arrayFind", "ArrayLeekValue", "LegacyArrayLeekValue"),
    ("arrayFlatten", "ArrayLeekValue", "LegacyArrayLeekValue"),
    ("arrayFoldLeft", "ArrayLeekValue", "LegacyArrayLeekValue"),
    ("arrayFoldRight", "ArrayLeekValue", "LegacyArrayLeekValue"),
    ("arrayFrequencies", "ArrayLeekValue", "LegacyArrayLeekValue"),
    ("arrayGet", "ArrayLeekValue", "LegacyArrayLeekValue"),
    ("arrayIter", "ArrayLeekValue", "LegacyArrayLeekValue"),
    ("arrayMap", "ArrayLeekValue", "LegacyArrayLeekValue"),
    ("arrayMax", "ArrayLeekValue", "LegacyArrayLeekValue"),
    ("arrayMin", "ArrayLeekValue", "LegacyArrayLeekValue"),
    ("arrayPartition", "ArrayLeekValue", "LegacyArrayLeekValue"),
    ("arrayRandom", "ArrayLeekValue", "LegacyArrayLeekValue"),
    ("arrayRemoveAll", "ArrayLeekValue", "LegacyArrayLeekValue"),
    ("arraySlice", "ArrayLeekValue", "LegacyArrayLeekValue"),
    ("arraySome", "ArrayLeekValue", "LegacyArrayLeekValue"),
    ("arraySort", "ArrayLeekValue", "LegacyArrayLeekValue"),
    ("arrayToSet", "ArrayLeekValue", "LegacyArrayLeekValue"),
    ("arrayUnique", "ArrayLeekValue", "LegacyArrayLeekValue"),
    ("assocReverse", "ArrayLeekValue", "LegacyArrayLeekValue"),
    ("assocSort", "ArrayLeekValue", "LegacyArrayLeekValue"),
    ("average", "ArrayLeekValue", "LegacyArrayLeekValue"),
    ("count", "ArrayLeekValue", "LegacyArrayLeekValue"),
    ("fill", "ArrayLeekValue", "LegacyArrayLeekValue"),
    ("inArray", "ArrayLeekValue", "LegacyArrayLeekValue"),
    ("insert", "ArrayLeekValue", "LegacyArrayLeekValue"),
    ("intervalAverage", "IntervalLeekValue", "IntervalLeekValue"),
    ("intervalCombine", "IntervalLeekValue", "IntervalLeekValue"),
    ("intervalContains", "IntervalLeekValue", "IntervalLeekValue"),
    (
        "intervalIntersection",
        "IntervalLeekValue",
        "IntervalLeekValue",
    ),
    (
        "intervalIsBounded",
        "IntervalLeekValue",
        "IntervalLeekValue",
    ),
    ("intervalIsClosed", "IntervalLeekValue", "IntervalLeekValue"),
    ("intervalIsEmpty", "IntervalLeekValue", "IntervalLeekValue"),
    (
        "intervalIsLeftBounded",
        "IntervalLeekValue",
        "IntervalLeekValue",
    ),
    (
        "intervalIsLeftClosed",
        "IntervalLeekValue",
        "IntervalLeekValue",
    ),
    (
        "intervalIsRightBounded",
        "IntervalLeekValue",
        "IntervalLeekValue",
    ),
    (
        "intervalIsRightClosed",
        "IntervalLeekValue",
        "IntervalLeekValue",
    ),
    ("intervalMax", "IntervalLeekValue", "IntervalLeekValue"),
    ("intervalMin", "IntervalLeekValue", "IntervalLeekValue"),
    ("intervalSize", "IntervalLeekValue", "IntervalLeekValue"),
    ("intervalToArray", "IntervalLeekValue", "IntervalLeekValue"),
    ("intervalToSet", "IntervalLeekValue", "IntervalLeekValue"),
    ("isEmpty", "ArrayLeekValue", "LegacyArrayLeekValue"),
    ("join", "ArrayLeekValue", "LegacyArrayLeekValue"),
    ("keySort", "ArrayLeekValue", "LegacyArrayLeekValue"),
    ("mapAverage", "MapLeekValue", "LegacyArrayLeekValue"),
    ("mapClear", "MapLeekValue", "LegacyArrayLeekValue"),
    ("mapContains", "MapLeekValue", "LegacyArrayLeekValue"),
    ("mapContainsKey", "MapLeekValue", "LegacyArrayLeekValue"),
    ("mapEvery", "MapLeekValue", "LegacyArrayLeekValue"),
    ("mapFill", "MapLeekValue", "LegacyArrayLeekValue"),
    ("mapFilter", "MapLeekValue", "LegacyArrayLeekValue"),
    ("mapFold", "MapLeekValue", "LegacyArrayLeekValue"),
    ("mapGet", "MapLeekValue", "LegacyArrayLeekValue"),
    ("mapIsEmpty", "MapLeekValue", "LegacyArrayLeekValue"),
    ("mapIter", "MapLeekValue", "LegacyArrayLeekValue"),
    ("mapKeys", "MapLeekValue", "LegacyArrayLeekValue"),
    ("mapMap", "MapLeekValue", "LegacyArrayLeekValue"),
    ("mapMax", "MapLeekValue", "LegacyArrayLeekValue"),
    ("mapMerge", "MapLeekValue", "LegacyArrayLeekValue"),
    ("mapMin", "MapLeekValue", "LegacyArrayLeekValue"),
    ("mapPut", "MapLeekValue", "LegacyArrayLeekValue"),
    ("mapPutAll", "MapLeekValue", "LegacyArrayLeekValue"),
    ("mapRemove", "MapLeekValue", "LegacyArrayLeekValue"),
    ("mapRemoveAll", "MapLeekValue", "LegacyArrayLeekValue"),
    ("mapReplace", "MapLeekValue", "LegacyArrayLeekValue"),
    ("mapReplaceAll", "MapLeekValue", "LegacyArrayLeekValue"),
    ("mapSearch", "MapLeekValue", "LegacyArrayLeekValue"),
    ("mapSize", "MapLeekValue", "LegacyArrayLeekValue"),
    ("mapSome", "MapLeekValue", "LegacyArrayLeekValue"),
    ("mapSum", "MapLeekValue", "LegacyArrayLeekValue"),
    ("mapValues", "MapLeekValue", "LegacyArrayLeekValue"),
    ("pop", "ArrayLeekValue", "LegacyArrayLeekValue"),
    ("push", "ArrayLeekValue", "LegacyArrayLeekValue"),
    ("pushAll", "ArrayLeekValue", "LegacyArrayLeekValue"),
    ("remove", "ArrayLeekValue", "LegacyArrayLeekValue"),
    ("removeElement", "ArrayLeekValue", "LegacyArrayLeekValue"),
    ("removeKey", "MapLeekValue", "LegacyArrayLeekValue"),
    ("reverse", "ArrayLeekValue", "LegacyArrayLeekValue"),
    ("search", "ArrayLeekValue", "LegacyArrayLeekValue"),
    ("setClear", "SetLeekValue", "SetLeekValue"),
    ("setContains", "SetLeekValue", "SetLeekValue"),
    ("setDifference", "SetLeekValue", "SetLeekValue"),
    ("setDisjunction", "SetLeekValue", "SetLeekValue"),
    ("setFilter", "SetLeekValue", "SetLeekValue"),
    ("setIntersection", "SetLeekValue", "SetLeekValue"),
    ("setIsEmpty", "SetLeekValue", "SetLeekValue"),
    ("setIsSubsetOf", "SetLeekValue", "SetLeekValue"),
    ("setPut", "SetLeekValue", "SetLeekValue"),
    ("setRemove", "SetLeekValue", "SetLeekValue"),
    ("setSize", "SetLeekValue", "SetLeekValue"),
    ("setToArray", "SetLeekValue", "SetLeekValue"),
    ("setUnion", "SetLeekValue", "SetLeekValue"),
    ("shift", "ArrayLeekValue", "LegacyArrayLeekValue"),
    ("shuffle", "ArrayLeekValue", "LegacyArrayLeekValue"),
    ("sort", "ArrayLeekValue", "LegacyArrayLeekValue"),
    ("subArray", "ArrayLeekValue", "LegacyArrayLeekValue"),
    ("sum", "ArrayLeekValue", "LegacyArrayLeekValue"),
    ("unshift", "ArrayLeekValue", "LegacyArrayLeekValue"),
];

/// Look up `name` and return its dispatch shape + arg-coercion hint.
/// `None` for unknown names — caller falls back to a bare AI-instance
/// call.
pub fn lookup(name: &str) -> Option<Builtin> {
    if let Ok(idx) = RECEIVER_TABLE.binary_search_by_key(&name, |&(n, _, _)| n) {
        let (_, v4_class, legacy_class) = RECEIVER_TABLE[idx];
        return Some(Builtin {
            dispatch: Dispatch::Receiver {
                v4_class,
                legacy_class,
            },
            prefer_long: false,
        });
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

#[cfg(test)]
mod tests {
    use super::{Dispatch, RECEIVER_TABLE, lookup};

    /// [`lookup`] binary-searches [`RECEIVER_TABLE`], so a row inserted
    /// out of order silently stops resolving. Strict `<` also rejects a
    /// name listed twice.
    #[test]
    fn receiver_table_is_sorted() {
        for pair in RECEIVER_TABLE.windows(2) {
            assert!(
                pair[0].0 < pair[1].0,
                "RECEIVER_TABLE must stay sorted by name for the binary search in \
                 lookup(): {:?} precedes {:?}",
                pair[0].0,
                pair[1].0
            );
        }
    }

    /// Every authored row is reachable through the binary search.
    #[test]
    fn every_receiver_row_is_found() {
        for &(name, v4_class, legacy_class) in RECEIVER_TABLE {
            let found = lookup(name).unwrap_or_else(|| panic!("{name} not found"));
            assert!(
                matches!(
                    found.dispatch,
                    Dispatch::Receiver { v4_class: v4, legacy_class: legacy }
                        if v4 == v4_class && legacy == legacy_class
                ),
                "{name} resolved to the wrong dispatch shape"
            );
        }
    }
}
