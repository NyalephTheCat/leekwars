//! Per-run mutable state: op budget / charging, strict mode, the
//! first-runtime-error slot, and the installer functions (`set_*` /
//! `clear_globals`) that populate the dispatch tables and per-run maps
//! before `main` runs.

use super::{
    CLASS_CTOR_THUNK, CLASS_PARENT, CLASS_REFLECT, CLASS_STRING_METHOD, DISPATCH, ENFORCE_BUDGET,
    GLOBALS, NATIVE_RNG, OP_COUNT, OP_LIMIT, RUNTIME_ERROR, STATIC_FIELDS, STATIC_INIT, STRICT,
};
use leek_runtime::{Rng, Value};
use std::collections::{HashMap, HashSet};

/// Set whether op-budget back-edge checks are emitted (compile-time flag).
pub fn set_enforce_budget(on: bool) {
    ENFORCE_BUDGET.with(|c| c.set(on));
}

/// Whether to emit op-budget back-edge checks for the function being compiled.
pub fn enforce_budget() -> bool {
    ENFORCE_BUDGET.with(std::cell::Cell::get)
}

/// Reset the op counter and install the budget for a new run.
pub fn reset_ops(limit: u64) {
    OP_COUNT.with(|c| c.set(0));
    OP_LIMIT.with(|c| c.set(limit));
}

/// Operations charged during the run just completed.
pub fn ops_used() -> u64 {
    OP_COUNT.with(std::cell::Cell::get)
}

/// Charge `n` operations. Called from JIT'd code at each MIR charge site
/// (matching the interpreter's `charge_ops`). On exceeding the budget it
/// records `TOO_MUCH_OPERATIONS`; the JIT'd code can't unwind, so loops poll
/// [`op_budget_exceeded`] at their back-edges to stop promptly.
#[unsafe(no_mangle)]
pub extern "C" fn leek_charge_ops(n: i64) {
    let next = OP_COUNT.with(|c| {
        let v = c.get().saturating_add(n.max(0) as u64);
        c.set(v);
        v
    });
    if next > OP_LIMIT.with(std::cell::Cell::get) {
        raise_runtime_error("TOO_MUCH_OPERATIONS");
    }
}

/// Whether the op budget has been exceeded — polled at loop back-edges so the
/// JIT'd code can branch out instead of running an unbounded loop to the end.
#[unsafe(no_mangle)]
pub extern "C" fn leek_op_budget_exceeded() -> i64 {
    i64::from(OP_COUNT.with(std::cell::Cell::get) > OP_LIMIT.with(std::cell::Cell::get))
}

/// Charge upstream's string-concatenation cost (`AI.add` string branch):
/// rendering each operand ticks 3 ops for a number (`string(Long)` /
/// `doubleToString` both charge 3), then the concatenation itself charges
/// `len(s1) + len(s2)` in UTF-16 code units (Java `String.length()`).
/// Called by the `leek_value_binop*` shims before dispatching an `Add`
/// where either side is a string; anything else charges nothing.
pub(super) fn charge_concat(l: &Value, r: &Value) {
    if !(matches!(l, Value::String(_)) || matches!(r, Value::String(_))) {
        return;
    }
    let conv = |v: &Value| match v {
        Value::Int(_) | Value::Real(_) => 3i64,
        _ => 0,
    };
    let len = |v: &Value| match v {
        Value::String(s) => s.encode_utf16().count() as i64,
        other => leek_runtime::value_as_concat_string(other)
            .encode_utf16()
            .count() as i64,
    };
    leek_charge_ops(conv(l) + conv(r) + len(l) + len(r));
}

/// Charge upstream's `AI.eq` string costs (`neq` delegates to `eq`):
/// comparing two strings ticks `min(len1, len2)`; comparing a number
/// against a string parses it, ticking `len(s)` — except the trivial
/// literals (`"true"`/`"false"`/`"0"`/`""`, and `"1"` against an exact 1)
/// which short-circuit before the parse and charge nothing. Lengths in
/// UTF-16 code units (Java `String.length()`). Every other operand mix
/// (bools, arrays, functions) charges nothing at this level.
pub(super) fn charge_eq(l: &Value, r: &Value) {
    let utf16 = |s: &str| s.encode_utf16().count() as i64;
    match (l, r) {
        (Value::String(a), Value::String(b)) => {
            leek_charge_ops(utf16(a).min(utf16(b)));
        }
        (Value::String(s), Value::Int(_) | Value::Real(_))
        | (Value::Int(_) | Value::Real(_), Value::String(s)) => {
            let n = if let Value::Int(i) = if matches!(l, Value::String(_)) { r } else { l } {
                #[allow(clippy::cast_precision_loss)]
                {
                    *i as f64
                }
            } else if let Value::Real(x) = if matches!(l, Value::String(_)) { r } else { l } {
                *x
            } else {
                return;
            };
            match s.as_str() {
                "true" | "false" | "0" | "" => {}
                "1" if n == 1.0 => {}
                _ => leek_charge_ops(utf16(s)),
            }
        }
        _ => {}
    }
}

/// Charge a builtin's runtime op cost (`builtin_op_cost`, which depends on the
/// argument values — e.g. batch ops over a collection's length). Called by the
/// `leek_builtinN` shims before dispatch, mirroring the interpreter's
/// `run_builtin`, so a `.ops(N)` case over a builtin matches.
///
/// Returns `true` if the budget is now exhausted — the shim then skips the
/// actual dispatch (returning null) so a single huge-allocation builtin
/// (`fill(a, 1, 1e9)`, `range(0, huge)`) can't exhaust host memory after the
/// budget is already spent. Mirrors the interpreter's `run_builtin`, which
/// returns the over-budget error *before* calling the builtin.
#[must_use]
pub(super) fn charge_builtin_ops(name: &str, args: &[Value], version: i64) -> bool {
    leek_charge_ops(leek_runtime::builtin_op_cost(name, args, version as u8) as i64);
    OP_COUNT.with(std::cell::Cell::get) > OP_LIMIT.with(std::cell::Cell::get)
}

/// Install the strict-typing flag for this run.
pub fn set_strict(strict: bool) {
    STRICT.with(|s| s.set(strict));
}

/// Clear any recorded runtime error before a run begins.
pub fn reset_runtime_error() {
    RUNTIME_ERROR.with(|e| *e.borrow_mut() = None);
}

/// Take the runtime error recorded during the run, if any.
pub fn take_runtime_error() -> Option<String> {
    RUNTIME_ERROR.with(|e| e.borrow_mut().take())
}

/// Record a runtime error (first one wins). Called by shims that detect a
/// fault the JIT'd code can't itself signal.
pub(super) fn raise_runtime_error(code: &str) {
    RUNTIME_ERROR.with(|e| {
        let mut slot = e.borrow_mut();
        if slot.is_none() {
            *slot = Some(code.to_string());
        }
    });
}

/// Install the per-class reflection name tables for this run.
pub fn set_class_reflect(map: HashMap<u32, HashMap<String, Vec<String>>>) {
    CLASS_REFLECT.with(|c| *c.borrow_mut() = map);
}

/// Install the per-class constructor-thunk table for this run.
pub fn set_class_ctor_thunk(map: HashMap<u32, usize>) {
    CLASS_CTOR_THUNK.with(|c| *c.borrow_mut() = map);
}

/// Install the per-class `string()`-display table for this run.
pub fn set_class_string_method(map: HashMap<u32, usize>) {
    CLASS_STRING_METHOD.with(|c| *c.borrow_mut() = map);
}

/// Install the class-parent table for this run.
pub fn set_class_parent(map: HashMap<u32, Option<(u32, String)>>) {
    CLASS_PARENT.with(|c| *c.borrow_mut() = map);
}

/// Install the user-function-reference table for this run.
pub fn set_user_fn_idx(map: HashMap<u32, usize>) {
    DISPATCH.with(|c| c.borrow_mut().user_fn_idx = map);
}

/// Install the set of method-derived user-fn `DefId`s requiring exact arity.
pub fn set_user_fn_exact_arity(set: HashSet<u32>) {
    DISPATCH.with(|c| c.borrow_mut().user_fn_exact_arity = set);
}

/// Install the method-resolution table for this run (clearing any prior).
pub fn set_method_resolve(map: HashMap<u32, HashMap<String, usize>>) {
    DISPATCH.with(|c| c.borrow_mut().method_resolve = map);
}

/// Install the static-field initialiser table for this run.
pub fn set_static_init(map: HashMap<(u32, String), usize>) {
    STATIC_INIT.with(|c| *c.borrow_mut() = map);
}

/// Reset the global + static-field stores. Called before every JIT run so a
/// run can't observe a previous run's mutable class state.
pub fn clear_globals() {
    GLOBALS.with(|g| g.borrow_mut().clear());
    STATIC_FIELDS.with(|g| g.borrow_mut().clear());
    // Reseed the PRNG so each run starts from the same sequence the
    // interpreter does (deterministic, reproducible).
    NATIVE_RNG.with(|r| *r.borrow_mut() = Rng::new());
}

/// Install the JIT-finalized lambda table for this run (clearing any prior).
pub fn set_lambda_fns(map: HashMap<usize, (*const u8, usize)>) {
    DISPATCH.with(|c| c.borrow_mut().lambda_fns = map);
}

/// Install the per-lambda user-param `@`-by-ref masks for this run.
pub fn set_lambda_byref(map: HashMap<usize, Vec<bool>>) {
    DISPATCH.with(|c| c.borrow_mut().lambda_byref = map);
}
