//! Builtin function dispatch.
//!
//! The Leekscript stdlib has hundreds of free functions; this is a
//! best-effort subset focused on what the upstream test corpus
//! exercises. Anything unrecognised returns `null`, matching
//! upstream's "missing builtin" runtime behavior.

use crate::value::Value;
use crate::{BuiltinFlow, BuiltinHost};

mod array;
mod core;
mod map;
mod misc;
mod string;

use array::dispatch_array;
use core::{dispatch_constant, dispatch_unary_math};
use map::dispatch_map;
use misc::dispatch_misc;
use string::dispatch_string;

pub use array::{deep_clone_for_v1, take_pending_promotion};

/// Total operation cost upstream charges for a builtin call: a per-call
/// base plus, for batch operations, a per-element multiplier over the
/// input size. Pure (no interpreter state) so any caller can meter it.
pub fn builtin_op_cost(name: &str, args: &[Value], version: u8) -> u64 {
    let total = builtin_cost(name);

    // `range(lo, hi)` allocates `hi - lo + 1` integers, but the size lives in
    // the numeric args, not a container in `arg[0]`, so it isn't in the batch
    // catalog. Meter it explicitly: the interpreter charges this cost *before*
    // dispatching, so a huge range (`range(0, Number.MAX_VALUE)`) trips the op
    // budget before it can allocate — closing an OOM/DoS hole.
    if name == "range" {
        return total.saturating_add(range_result_len(args));
    }

    let mut total = total;
    if let Some(mut mult) = batch_op_multiplier(name) {
        // v1-3 LegacyArray push is more expensive than v4 Array push.
        if version <= 3 && matches!(name, "intervalToArray" | "intervalToSet" | "fill") {
            mult = 5;
        }
        let first_n = args
            .first()
            .map_or(0, |v| match v {
                Value::Array(a) => a.borrow().len() as u64,
                Value::Map(m) => m.borrow().len() as u64,
                Value::String(s) => s.len() as u64,
                Value::Interval(iv) => {
                    match (iv.start, iv.end) {
                        (Some(s), Some(e)) if !iv.is_empty() => {
                            let lo = if iv.start_inclusive { s } else { s + 1.0 };
                            let hi = if iv.end_inclusive { e } else { e - 1.0 };
                            // Saturating `+1` so a very wide interval can't
                            // overflow before the `.max(0)` clamp.
                            u64::try_from(crate::real_to_int(hi - lo).saturating_add(1).max(0))
                                .unwrap_or(0)
                        }
                        _ => 0,
                    }
                }
                _ => 0,
            });
        // `fill(arr, value, n)` allocates `n` slots; the relevant
        // size is the last numeric arg, not the (often empty) input.
        let n_arg = if name == "fill" {
            args.last()
                .and_then(super::value::types::Value::as_int)
                .map_or(0, |i| u64::try_from(i.max(0)).unwrap_or(0))
        } else {
            0
        };
        let n = first_n.max(n_arg);
        total = total.saturating_add(n.saturating_mul(mult));
    }
    total
}

/// Number of integers `range(lo, hi)` would allocate (`hi - lo + 1`, clamped
/// to 0 when `hi < lo`), with saturating arithmetic so wide bounds can't
/// overflow `i64`/`u64`. Used to meter `range` for the op budget.
fn range_result_len(args: &[Value]) -> u64 {
    let lo = args.first().and_then(super::value::types::Value::as_int);
    let hi = args.get(1).and_then(super::value::types::Value::as_int);
    match (lo, hi) {
        (Some(lo), Some(hi)) if hi >= lo => u64::try_from(hi.saturating_sub(lo))
            .unwrap_or(u64::MAX)
            .saturating_add(1),
        _ => 0,
    }
}

/// Dispatch a stdlib builtin to its implementation. Pure of operation
/// metering (callers charge [`builtin_op_cost`] separately) and of any
/// concrete backend — stateful needs (version, RNG, higher-order
/// callbacks) come through the [`BuiltinHost`].
pub fn call_builtin(
    host: &mut dyn BuiltinHost,
    name: &str,
    args: &[Value],
) -> Result<Value, BuiltinFlow> {
    if let Some(v) = dispatch_constant(name) {
        return Ok(v);
    }
    if let Some(v) = dispatch_unary_math(name, args) {
        return Ok(v);
    }
    if let Some(v) = dispatch_array(host, name, args)? {
        return Ok(v);
    }
    if let Some(v) = dispatch_map(host, name, args)? {
        return Ok(v);
    }
    if let Some(v) = dispatch_string(name, args) {
        return Ok(v);
    }
    if let Some(v) = dispatch_misc(host, name, args)? {
        return Ok(v);
    }
    Ok(Value::Null)
}

/// Per-element op multiplier for batch operations. Mirrors
/// upstream `ai.ops(size * N)` charges so stress tests (8k+
/// element maps/filters/concats/clones) correctly hit the op
/// budget. Returns `None` for names without an element-scaled
/// cost.
fn batch_op_multiplier(name: &str) -> Option<u64> {
    leek_builtins::batch_multiplier_u64(name)
}

/// Per-call op cost — shared catalog; default 1 op when not listed.
pub(crate) fn builtin_cost(name: &str) -> u64 {
    leek_builtins::op_cost_u64(name)
}


// ---- Constants ----

pub fn lookup_constant(name: &str) -> Option<Value> {
    dispatch_constant(name)
}

/// Declared arity of a builtin when known. Used by higher-order
/// builtins (`arrayMap`, `arraySort`, ...) to decide how many
/// args to pass through. `None` means "unknown — pass at most 1".
pub fn builtin_arity(name: &str) -> Option<usize> {
    Some(match name {
        // Two-arg numeric helpers — needed so `arrayMap(arr, atan2)`
        // dispatches as 2-arg `(value, index)` rather than 1-arg.
        "atan2" | "hypot" | "pow" | "max" | "min" => 2,
        // Most unary math takes one user arg.
        "abs" | "acos" | "asin" | "atan" | "cbrt" | "ceil" | "cos" | "cosh" | "exp" | "floor"
        | "log" | "log10" | "log2" | "round" | "sin" | "sinh" | "sqrt" | "tan" | "tanh"
        | "toDegrees" | "toRadians" | "isFinite" | "isInfinite" | "isNaN" | "signum" | "number"
        | "string" | "length" | "typeOf" => 1,
        _ => return None,
    })
}

/// True if `name` is a built-in callable / value / type. Used to
/// distinguish a real builtin reference (`var f = sqrt`) from an
/// unresolved-name read (`var r = x` where `x` was never declared)
/// — the latter should evaluate to `null`, not a phantom function
/// value. The list is union of the standard library names handled
/// by the dispatchers below.
pub fn is_known_builtin(name: &str) -> bool {
    KNOWN_BUILTIN_NAMES.contains(&name)
}

/// Every standard-library name the interpreter can reach via a
/// `Callee::Builtin` / `BuiltinRef`. New builtins must be added
/// here AND to the relevant `dispatch_*`.
const KNOWN_BUILTIN_NAMES: &[&str] = &[
    "abs",
    "acos",
    "Array",
    "arrayChunk",
    "arrayClear",
    "arrayConcat",
    "arrayCount",
    "arrayDistinct",
    "arrayEvery",
    "arrayFilter",
    "arrayFind",
    "arrayFirst",
    "arrayFlatten",
    "arrayFoldLeft",
    "arrayFoldRight",
    "arrayFrequencies",
    "arrayGet",
    "arrayIter",
    "arrayLast",
    "arrayMap",
    "arrayMax",
    "arrayMin",
    "arrayPartition",
    "arrayRandom",
    "arrayRemoveAll",
    "arraySize",
    "arraySlice",
    "arraySome",
    "arraySort",
    "arrayToSet",
    "arrayUnique",
    "asin",
    "assocReverse",
    "assocSort",
    "atan",
    "atan2",
    "average",
    "binString",
    "bitCount",
    "bitReverse",
    "bitsToReal",
    "Boolean",
    "byteReverse",
    "cbrt",
    "ceil",
    "charAt",
    "charCodeAt",
    "chr",
    "Class",
    "clone",
    "codePointAt",
    "color",
    "concat",
    "contains",
    "cos",
    "cosh",
    "count",
    "debug",
    "debugC",
    "debugE",
    "debugW",
    "distinct",
    "E",
    "endsWith",
    "exp",
    "false",
    "fill",
    "first",
    "Float",
    "floor",
    "fromCharCode",
    "fromCodePoint",
    "Function",
    "getBlue",
    "getColor",
    "getDate",
    "getGreen",
    "getInstructionsCount",
    "getMaxOperations",
    "getMaxRAM",
    "getOperations",
    "getRed",
    "getTime",
    "getTimestamp",
    "getUsedRAM",
    "hash",
    "hashCode",
    "hexString",
    "hypot",
    "inArray",
    "indexOf",
    "Infinity",
    "INFINITY",
    "insert",
    "Integer",
    "Interval",
    "intervalAverage",
    "intervalCombine",
    "intervalContains",
    "intervalIntersection",
    "intervalIsBounded",
    "intervalIsClosed",
    "intervalIsEmpty",
    "intervalIsLeftBounded",
    "intervalIsLeftClosed",
    "intervalIsRightBounded",
    "intervalIsRightClosed",
    "intervalMax",
    "intervalMin",
    "intervalSize",
    "intervalToArray",
    "intervalToSet",
    "isEmpty",
    "isFinite",
    "isInfinite",
    "isNaN",
    "isPermutation",
    "join",
    "jsonDecode",
    "jsonEncode",
    "keySort",
    "last",
    "leadingZeros",
    "length",
    "log",
    "Map",
    "mapAverage",
    "mapClear",
    "mapContains",
    "mapContainsKey",
    "mapContainsValue",
    "mapEvery",
    "mapFill",
    "mapFilter",
    "mapFold",
    "mapGet",
    "mapIsEmpty",
    "mapIter",
    "mapKeys",
    "mapMap",
    "mapMax",
    "mapMerge",
    "mapMin",
    "mapPut",
    "mapPutAll",
    "mapRemove",
    "mapRemoveAll",
    "mapReplace",
    "mapReplaceAll",
    "mapSearch",
    "mapSize",
    "mapSome",
    "mapSum",
    "mapValues",
    "max",
    "min",
    "NaN",
    "NAN",
    "null",
    "Null",
    "Number",
    "number",
    "Object",
    "ord",
    "PI",
    "pop",
    "pow",
    "print",
    "println",
    "push",
    "pushAll",
    "rand",
    "randFloat",
    "randInt",
    "randReal",
    "range",
    "Real",
    "realBits",
    "remove",
    "removeElement",
    "removeKey",
    "repeat",
    "replace",
    "reverse",
    "rotateLeft",
    "rotateRight",
    "round",
    "search",
    "Set",
    "setClear",
    "setContains",
    "setDifference",
    "setDisjunction",
    "setFilter",
    "setForEach",
    "setIntersection",
    "setIsEmpty",
    "setIsSubsetOf",
    "setIsSupersetOf",
    "setIter",
    "setMap",
    "setPut",
    "setRemove",
    "setSize",
    "setToArray",
    "setUnion",
    "shift",
    "shuffle",
    "signum",
    "sin",
    "sinh",
    "sort",
    "SORT_ASC",
    "SORT_DESC",
    "SORT_RANDOM",
    "split",
    "sqrt",
    "startsWith",
    "String",
    "string",
    "stringHash",
    "subArray",
    "substring",
    "sum",
    "tan",
    "tanh",
    "toDegrees",
    "toLower",
    "toRadians",
    "toUpper",
    "trailingZeros",
    "trim",
    "true",
    "typeOf",
    "TYPE_ARRAY",
    "TYPE_BOOLEAN",
    "TYPE_FUNCTION",
    "TYPE_NULL",
    "TYPE_NUMBER",
    "TYPE_OBJECT",
    "TYPE_STRING",
    "unknown",
    "unshift",
    "USE_CRITICAL",
    "USE_FAILED",
    "USE_INVALID_POSITION",
    "USE_INVALID_TARGET",
    "USE_NOT_ENOUGH_TP",
    "USE_RESURRECT",
    "USE_SUCCESS",
    "Value",
    "JSON",
    "System",
];

/// True if this builtin requires at least one user argument when
/// invoked indirectly (`[cos][0]()`). Direct calls (`cos()`) still
/// get the compile-time default-arg injection so they stay
/// permissive — only the indirect callsite needs the gate.
pub fn needs_at_least_one_arg(name: &str) -> bool {
    // All single-arg math + length-like builtins.
    matches!(
        name,
        "abs"
            | "sqrt"
            | "cbrt"
            | "ceil"
            | "floor"
            | "round"
            | "sin"
            | "cos"
            | "tan"
            | "asin"
            | "acos"
            | "atan"
            | "sinh"
            | "cosh"
            | "tanh"
            | "exp"
            | "log"
            | "log10"
            | "log2"
            | "signum"
            | "number"
            | "toDegrees"
            | "toRadians"
            | "isFinite"
            | "isInfinite"
            | "isNaN"
            | "string"
            | "length"
            | "typeOf"
    )
}
