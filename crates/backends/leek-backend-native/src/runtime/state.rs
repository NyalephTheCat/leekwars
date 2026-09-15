//! Per-run mutable state: op budget / charging, strict mode, the
//! first-runtime-error slot, and the installer functions (`set_*` /
//! `clear_globals`) that populate the dispatch tables and per-run maps
//! before `main` runs.

use super::{
    CLASS_CTOR_THUNK, CLASS_PARENT, CLASS_REFLECT, CLASS_STRING_METHOD, DISPATCH, GLOBALS,
    NATIVE_RNG, OP_COUNT, OP_LIMIT, RUNTIME_ERROR, STATIC_FIELD_OWNER, STATIC_FIELDS, STATIC_INIT,
    STRICT,
};
use leek_runtime::{Rng, Value};
use std::collections::{HashMap, HashSet};

/// Reset the op counter and install the budget for a new run.
pub fn reset_ops(limit: u64) {
    OP_COUNT.with(|c| c.set(0));
    OP_LIMIT.with(|c| c.set(limit));
}

/// Operations charged during the run just completed.
pub fn ops_used() -> u64 {
    OP_COUNT.with(std::cell::Cell::get)
}

shim! {
    /// Charge `n` operations. Called from JIT'd code at each MIR charge site
    /// (matching upstream's op charging). On exceeding the budget it
    /// records `TOO_MUCH_OPERATIONS`; the JIT'd code can't unwind, so loops poll
    /// `leek_op_budget_exceeded` at their back-edges to stop promptly.
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
}

shim! {
    /// Whether the run must stop — polled at loop back-edges so the JIT'd code
    /// branches out instead of running on. True once any runtime error has been
    /// recorded, which includes the op budget being exceeded
    /// ([`leek_charge_ops`] records `TOO_MUCH_OPERATIONS`), so a loop whose ops
    /// are charged inside callees stops too.
    pub extern "C" fn leek_op_budget_exceeded() -> i64 {
        i64::from(aborting())
    }
}

/// Arm the recursion guard for a new run: reset the frame counter, install the
/// call-depth limit, and set the stack floor `max_stack_bytes` below the
/// caller's current stack position (`usize::MAX` disables the stack check).
///
/// Call it from the frame that then invokes the JIT'd entry, so the budget
/// covers only the program's own frames.
#[inline(never)]
pub fn arm_call_guard(max_call_depth: u32, max_stack_bytes: usize) {
    super::MAX_CALL_DEPTH.with(|c| c.set(max_call_depth));
    super::CALL_DEPTH.with(|c| c.set(0));
    let marker = 0u8;
    let sp = std::ptr::addr_of!(marker) as usize;
    super::STACK_FLOOR.with(|c| c.set(sp.saturating_sub(max_stack_bytes)));
}

/// The upstream error for runaway recursion (`Error.STACKOVERFLOW`, which the
/// generator records when the JVM throws `StackOverflowError`).
pub const STACK_OVERFLOW: &str = "STACKOVERFLOW";

shim! {
    /// Function-entry prologue of every compiled user function (not `main`).
    /// Returns non-zero when the function must return its default value at
    /// once: the run has already errored, or entering this frame would exceed
    /// the call-depth limit or the native stack budget (the frame starts below
    /// the floor [`arm_call_guard`] set) — which records [`STACK_OVERFLOW`]. Only a frame
    /// that returns 0 is counted, and must be matched by [`leek_leave_frame`].
    ///
    /// Without it, unbounded recursion (`function f(x) { return f(x) }`) runs
    /// the native thread out of stack and the OS kills the whole host process.
    pub extern "C" fn leek_enter_frame() -> i64 {
        if aborting() {
            return 1;
        }
        let depth = super::CALL_DEPTH.with(std::cell::Cell::get).saturating_add(1);
        let marker = 0u8;
        let sp = std::ptr::addr_of!(marker) as usize;
        if depth > super::MAX_CALL_DEPTH.with(std::cell::Cell::get)
            || sp < super::STACK_FLOOR.with(std::cell::Cell::get)
        {
            raise_runtime_error(STACK_OVERFLOW);
            return 1;
        }
        super::CALL_DEPTH.with(|c| c.set(depth));
        0
    }
}

shim! {
    /// Function-return epilogue: pops the frame [`leek_enter_frame`] counted.
    /// (Early returns taken after an error skip it; the counter only matters
    /// until the run stops and is reset for the next one.)
    pub extern "C" fn leek_leave_frame() {
        super::CALL_DEPTH.with(|c| c.set(c.get().saturating_sub(1)));
    }
}

/// Whether a runtime error has been recorded this run, i.e. execution must
/// stop. Side-effecting shims check it and do nothing once it is set, matching
/// upstream, where the error is an exception that ends the AI immediately.
pub(super) fn aborting() -> bool {
    super::ABORT.with(std::cell::Cell::get)
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
        Value::String(s) => leek_runtime::len_as_int(leek_runtime::jstr::len16(s)),
        other => leek_runtime::len_as_int(leek_runtime::jstr::len16(
            &leek_runtime::value_as_concat_string(other),
        )),
    };
    leek_charge_ops(conv(l) + conv(r) + len(l) + len(r));
}

/// Charge upstream's `AI.eq` costs (`neq` delegates to `eq`).
///
/// Strings: comparing two strings ticks `min(len1, len2)`; comparing a
/// number against a string parses it, ticking `len(s)` — except the
/// trivial literals (`"true"`/`"false"`/`"0"`/`""`, and `"1"` against an
/// exact 1) which short-circuit before the parse and charge nothing.
/// Lengths in UTF-16 code units (Java `String.length()`).
///
/// Collections: `AI.eq` reaches `ArrayLeekValue.eq` / `MapLeekValue.eq` /
/// `SetLeekValue.eq` only when BOTH sides are that same kind, and each of
/// the three opens with `ai.ops(1)`, then returns on a size mismatch or an
/// empty pair, then charges the per-element part — `ops(size())` for the
/// array, `ops(2 * size())` for the map and the set. `size()` there is the
/// receiver's, i.e. the LEFT operand, which only matters for the flat `1`
/// since the rest is gated on the two sizes being equal. (v1–v3 arrays go
/// through `LegacyArrayLeekValue.equals(AI, LegacyArrayLeekValue)`, whose
/// charges are the same `ops(1)` + `ops(mSize)`, so the array arm needs no
/// version split.)
///
/// Two upstream costs stay unmodelled, both because they depend on where
/// the comparison stops rather than on the operands alone: the per-element
/// `ai.eq` recursion's own charges (`['ab'] == ['ab']` ticks a further
/// `min(2, 2)` for the two strings), and `MapLeekValue.eq`'s one
/// `map.get(key)` per left entry, which goes through the overridden
/// `MapLeekValue.get` and so adds `READ_OPERATIONS` (2) apiece up to the
/// first mismatch. An equal 1-entry map pair is therefore 10 ops upstream
/// (`return [5: 5] == [5: 5]`, reference.tsv) where this charges 8. As
/// elsewhere in this function, only the top-level operand pair is priced.
pub(super) fn charge_eq(l: &Value, r: &Value) {
    let utf16 = |s: &str| leek_runtime::len_as_int(leek_runtime::jstr::len16(s));
    // `ops(1)` unconditionally, then the per-element part only when the
    // sizes agree — and `per * 0` keeps the empty pair at the bare 1, the
    // way upstream's early `return true` does.
    let collection = |a: usize, b: usize, per: i64| {
        let per_element = if a == b {
            leek_runtime::len_as_int(a)
        } else {
            0
        };
        leek_charge_ops(1 + per * per_element);
    };
    match (l, r) {
        (Value::Array(a), Value::Array(b)) => {
            collection(a.borrow().len(), b.borrow().len(), 1);
        }
        (Value::Map(a), Value::Map(b)) => {
            collection(a.borrow().len(), b.borrow().len(), 2);
        }
        (Value::Set(a), Value::Set(b)) => {
            collection(a.borrow().len(), b.borrow().len(), 2);
        }
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
/// `leek_builtinN` shims before dispatch, mirroring upstream's
/// `run_builtin`, so a `.ops(N)` case over a builtin matches.
///
/// Returns `true` if the run must stop (the budget is now exhausted, or an
/// earlier runtime error was recorded) — the shim then skips the actual
/// dispatch (returning null) so a single huge-allocation builtin
/// (`fill(a, 1, 1e9)`, `range(0, huge)`) can't exhaust host memory after the
/// budget is already spent, and no builtin acts after an error. Mirrors the
/// upstream, which returns the over-budget error *before*
/// calling the builtin.
#[must_use]
pub(super) fn charge_builtin_ops(name: &str, args: &[Value], version: i64) -> bool {
    leek_charge_ops(leek_runtime::builtin_op_cost(name, args, version as u8) as i64);
    aborting()
}

/// Install the strict-typing flag for this run.
pub fn set_strict(strict: bool) {
    STRICT.with(|s| s.set(strict));
}

/// Clear any recorded runtime error (and the abort flag) before a run begins.
pub fn reset_runtime_error() {
    RUNTIME_ERROR.with(|e| *e.borrow_mut() = None);
    super::ABORT.with(|a| a.set(false));
}

/// Take the runtime error recorded during the run, if any.
pub fn take_runtime_error() -> Option<String> {
    RUNTIME_ERROR.with(|e| e.borrow_mut().take())
}

/// The error recorded so far this run, cloned WITHOUT consuming it — so a
/// mid-run reader (a builtin reporting the fault up its own error channel)
/// leaves the slot for [`take_runtime_error`], which is what turns the run
/// into an `Err` at the end. Only called once [`aborting`] is already true,
/// so the clone never costs anything on the hot path.
pub(super) fn current_runtime_error() -> Option<String> {
    RUNTIME_ERROR.with(|e| e.borrow().clone())
}

/// Record a runtime error (first one wins) and raise the abort flag, so loop
/// back-edges and side-effecting shims stop the run from here on. Called by
/// shims that detect a fault the JIT'd code can't itself signal.
pub(super) fn raise_runtime_error(code: &str) {
    super::ABORT.with(|a| a.set(true));
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

/// Install the static-method-resolution table for this run.
pub fn set_static_method_resolve(map: HashMap<u32, HashMap<String, usize>>) {
    DISPATCH.with(|c| c.borrow_mut().static_method_resolve = map);
}

/// Install the static-field ownership table for this run.
pub fn set_static_field_owner(map: HashMap<u32, HashMap<String, u32>>) {
    STATIC_FIELD_OWNER.with(|c| *c.borrow_mut() = map);
}

/// Reset the global + static-field stores. Called before every JIT run so a
/// run can't observe a previous run's mutable class state.
pub fn clear_globals() {
    GLOBALS.with(|g| g.borrow_mut().clear());
    STATIC_FIELDS.with(|g| g.borrow_mut().clear());
    // Reseed the PRNG so each run starts from the same sequence the
    // upstream does (deterministic, reproducible).
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
