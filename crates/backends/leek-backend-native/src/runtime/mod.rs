//! C-ABI runtime shims for composite values (arrays first).
//!
//! Native code keeps composite — and boxed scalar — values as opaque
//! `*mut Value` *handles*. A handle is a leaked `Box<Value>`: there's no
//! garbage collector yet, which is sound for the run-once JIT execution
//! the corpus runner and `leekc --emit native` perform (the process exits
//! shortly after). The shims box/unbox scalars and implement array
//! operations by delegating to the shared `leek_runtime` value logic, so
//! the semantics match the interpreter.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};

use leek_runtime::{Rng, Value};

mod calls;
mod collections;
mod objects;
mod state;
mod values;

pub use calls::*;
pub use collections::*;
pub use objects::*;
pub use state::*;
pub use values::*;

/// A JIT-compiled lambda body, called with the uniform ABI
/// `(argv, argc) -> result` where `argv` is `captured ++ args`, each a boxed
/// `*mut Value` handle, and the result is a boxed handle.
type LambdaFn = extern "C" fn(*const *mut Value, i64) -> *mut Value;

/// Per-run storage for every value handle: a bump arena that owns the `Value`s
/// plus the list of pointers to drop. Held in ONE thread-local so the hot
/// `handle` path does a single TLS access (it allocates *and* records together).
///
/// The arena is behind an `Option` so [`BOX_STATE`] can be `const`-initialized —
/// that matters: a `const` thread-local skips the lazy-initialization guard the
/// stdlib runs on *every* access of a non-const one, which profiling showed was
/// ~45% of native `fib` self-time (`LocalKey::with`). The arena is created on
/// first use (a predictable branch) and reused across runs via `reset`.
struct BoxState {
    arena: Option<bumpalo::Bump>,
    /// Bumpalo does NOT run destructors on `reset`, so this is the drop list:
    /// each pointer is `drop_in_place`d exactly once at run end (releasing the
    /// `Rc`-backed storage the value holds) before the arena is reset. Handles
    /// are read by *cloning* (never freed) elsewhere, so there is no double-free.
    boxes: Vec<*mut Value>,
}

/// Per-run dispatch tables — all installed once at run setup (before `main`)
/// and READ-ONLY during execution. Merged into one thread-local so the hot
/// call paths (`leek_call_method`, `dispatch_call_value`) resolve a method /
/// lambda in a SINGLE `LocalKey::with` + `borrow` instead of 2–4 separate
/// thread-local accesses. Lookups always copy out before invoking any callee,
/// so the (shared) `RefCell` borrow is never held across re-entrant runtime code.
#[derive(Default)]
struct DispatchTables {
    /// `function_idx` → (uniform-ABI address, param count incl. captures/`this`).
    lambda_fns: HashMap<usize, (*const u8, usize)>,
    /// `function_idx` → per-lambda `@`-by-ref mask over its user params.
    lambda_byref: HashMap<usize, Vec<bool>>,
    /// class `DefId` raw → method name → method's `program.functions` index.
    method_resolve: HashMap<u32, HashMap<String, usize>>,
    /// named-function-ref `DefId` raw → `program.functions` index.
    user_fn_idx: HashMap<u32, usize>,
    /// `DefId`s of method-valued user fns needing exact arity on an indirect call.
    user_fn_exact_arity: HashSet<u32>,
}

thread_local! {
    /// See [`DispatchTables`].
    static DISPATCH: RefCell<DispatchTables> = RefCell::new(DispatchTables::default());

    /// See [`BoxState`]. Bump allocation replaces a `malloc` per value (the
    /// dominant native cost on boxing-heavy code) and `reset` retains the
    /// largest chunk, so steady-state runs (corpus, LSP, fights) allocate ~zero.
    static BOX_STATE: RefCell<BoxState> =
        const { RefCell::new(BoxState { arena: None, boxes: Vec::new() }) };

    /// File-level globals, keyed by name (matching the interpreter), each
    /// holding a value handle. Cleared by [`clear_globals`] before every
    /// JIT run so programs don't see a previous run's globals.
    static GLOBALS: RefCell<HashMap<String, *mut Value>> = RefCell::new(HashMap::new());

    /// The program's PRNG — ONE generator persisted across the whole run
    /// (like the interpreter's `self.rng`), so successive `rand`/`randInt`
    /// calls advance a single xorshift sequence. (Constructing a fresh
    /// `Rng::new()` per builtin shim, as native used to, reset the
    /// sequence on every call.) Default-seeded and reset per run in
    /// [`clear_globals`]; the same seed + sequence as the interpreter, so
    /// native reproduces the interpreter's RNG-dependent results exactly.
    static NATIVE_RNG: RefCell<Rng> = RefCell::new(Rng::new());

    /// Static-field storage, keyed by `(owning-class def_id, field name)`,
    /// each holding a value handle. Lazily initialised on first read.
    static STATIC_FIELDS: RefCell<HashMap<(u32, String), *mut Value>> = RefCell::new(HashMap::new());

    /// Static-field initialisers: `(class def_id, field name)` → the
    /// nullary init function's `program.functions` index (uniform-ABI,
    /// registered in `LAMBDA_FNS`). Only fields with an initialiser appear.
    static STATIC_INIT: RefCell<HashMap<(u32, String), usize>> = RefCell::new(HashMap::new());

    /// Each user class's parent for runtime `.super` navigation: class `DefId`
    /// raw → `Some((parent def, parent name))` for an explicit user parent, or
    /// `None` for a class with no explicit parent (implicit builtin `Value`
    /// base). Lets `x.class.super` resolve at runtime.
    static CLASS_PARENT: RefCell<HashMap<u32, Option<(u32, String)>>> =
        RefCell::new(HashMap::new());

    /// Per-class constructor *thunk*: class `DefId` raw → the synthetic thunk
    /// function's `program.functions` index (uniform-ABI, in `LAMBDA_FNS`).
    /// The thunk does `new C(args)` and returns the instance, so a class
    /// reference used as a *value* — `arrayMap(a, A)`, or an object slot
    /// holding `A` that's then called — constructs through `dispatch_call_value`.
    /// Only classes detected as used-as-value (and constructible) get one.
    static CLASS_CTOR_THUNK: RefCell<HashMap<u32, usize>> = RefCell::new(HashMap::new());

    /// Per-class 0-arg `string()` display override: class `DefId` raw → the
    /// method's `program.functions` index (uniform-ABI, in `LAMBDA_FNS`). When
    /// the *top-level* program result is an instance of such a class, the result
    /// goes through `string()` (mirroring the interpreter's
    /// `invoke_instance_string_method`). Only constructed classes get one.
    static CLASS_STRING_METHOD: RefCell<HashMap<u32, usize>> = RefCell::new(HashMap::new());

    /// Per-class reflection name tables for runtime `x.class.<member>`:
    /// class `DefId` raw → member → `[names]` (`fields`/`methods`/…). The
    /// compile-time `C.fields` path (a class-ref local) is handled separately;
    /// this serves a `ClassRef` *value* reached dynamically.
    static CLASS_REFLECT: RefCell<HashMap<u32, HashMap<String, Vec<String>>>> =
        RefCell::new(HashMap::new());

    /// First runtime error raised during the current JIT run, if any. The
    /// JIT'd code has no exception mechanism, so a shim that detects a runtime
    /// fault (e.g. a v4-strict out-of-bounds array write) records it here and
    /// returns benignly; `run()` checks this *after* `main` returns and turns a
    /// recorded error into `NativeError::Runtime`. First error wins (later
    /// statements may run, but the program's outcome is the first fault).
    static RUNTIME_ERROR: RefCell<Option<String>> = const { RefCell::new(None) };

    /// Whether the current run uses strict typing. Mirrors the interpreter's
    /// `strict` flag, which some runtime-fault rules depend on (e.g. an
    /// out-of-bounds array write only errors under v4 *strict*).
    static STRICT: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };

    /// Operations charged during the current run. The JIT'd code calls
    /// [`leek_charge_ops`] at the same MIR sites the interpreter charges, so
    /// the two backends produce identical op counts. Read after `main` returns.
    static OP_COUNT: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };

    /// Operation budget for the current run. When [`OP_COUNT`] exceeds it,
    /// `leek_charge_ops` records `TOO_MUCH_OPERATIONS` (mirroring the
    /// interpreter's `charge_ops`). `u64::MAX` ≈ unlimited (for op-count
    /// verification of small programs that must run to completion).
    static OP_LIMIT: std::cell::Cell<u64> = const { std::cell::Cell::new(u64::MAX) };
}

thread_local! {
    /// Whether the *currently-compiling* function should emit op-budget checks
    /// at branch back-edges (so an unbounded loop stops instead of spinning /
    /// exhausting memory). Set from `opts.op_limit != u64::MAX` at compile time;
    /// off for ordinary runs (unlimited budget) to avoid per-branch overhead.
    static ENFORCE_BUDGET: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

#[inline]
fn handle(v: Value) -> *mut Value {
    // ONE thread-local access: allocate the value's storage from the per-run
    // bump arena (cheap, chunked — `Bump::alloc` takes `&self` and returns a
    // unique `&mut Value` with a stable address valid until the next `reset`)
    // AND record the pointer for `free_run_boxes` to drop. `BOX_STATE` is
    // `const`-initialized, so this skips the lazy-init guard a non-const
    // thread-local pays on every access.
    BOX_STATE.with(|s| {
        let s = &mut *s.borrow_mut();
        let r = s.arena.get_or_insert_with(bumpalo::Bump::new).alloc(v);
        // Only register values that own heap (`Rc`/`Box`/`String`) for the
        // run-end drop sweep. A trivially-droppable scalar (`Int`/`Real`/`Bool`/
        // `Null`) — and `BuiltinClass`, a `&'static str` — has a no-op `Drop`, so
        // recording it would just cost a `boxes` push here and a wasted
        // `drop_in_place` in `free_run_boxes`. Its bump storage is reclaimed by
        // the arena `reset` regardless.
        let needs_drop = !matches!(
            r,
            Value::Int(_) | Value::Real(_) | Value::Bool(_) | Value::Null | Value::BuiltinClass(_)
        );
        let p = std::ptr::from_mut(r);
        if needs_drop {
            s.boxes.push(p);
        }
        p
    })
}

/// Borrow the `Value` behind a handle.
///
/// # Safety
/// `p` must be a handle previously produced by one of these shims (and
/// still live — handles are leaked, so they always are).
#[inline]
unsafe fn val<'a>(p: *mut Value) -> &'a Value {
    unsafe { &*p }
}

/// Box an arbitrary compile-time-known `Value` (e.g. a builtin constant
/// like `PI` / `SORT_ASC`) into a leaked handle whose pointer is embedded
/// as a constant in the generated code.
pub fn box_value(v: Value) -> *mut Value {
    handle(v)
}

/// Read the `Value` behind a handle by cloning it, WITHOUT freeing the box.
/// The box stays owned by the per-run [`BOXES`] registry and is reclaimed in a
/// single sweep by [`free_run_boxes`] at run end — so handles are never freed
/// at the read site, which keeps box ownership in exactly one place and makes a
/// double-free impossible. Used at the JIT boundary to read the program's
/// result and at each lambda-callback return.
///
/// # Safety
/// `p` must be a live handle (created by [`handle`] and not yet swept).
pub unsafe fn read_handle(p: *mut Value) -> Value {
    unsafe { (*p).clone() }
}

/// Reclaim every value handle allocated during the current run. Call once the
/// run's result has been read out (cloned) — see [`read_handle`] — so the
/// result `Value` and anything reachable from it (held by its own `Rc` clones)
/// survives.
///
/// Each handle's storage lives in the [`ARENA`]; bumpalo doesn't run
/// destructors, so we `drop_in_place` each value exactly once here (releasing
/// the `Rc`-backed array/map/string storage it holds — no other code frees a
/// handle, so there is no double-free), then `reset` the arena to reclaim all
/// its memory at once while retaining capacity for the next run.
pub fn free_run_boxes() {
    BOX_STATE.with(|s| {
        let s = &mut *s.borrow_mut();
        for p in s.boxes.drain(..) {
            // SAFETY: `p` is a unique, still-live value in the arena, produced
            // by `handle` and dropped exactly once (reads clone, never free).
            unsafe { std::ptr::drop_in_place(p) };
        }
        // All values are dropped; release their bump storage in one shot.
        // `reset` keeps the largest chunk so subsequent runs reuse it.
        if let Some(arena) = s.arena.as_mut() {
            arena.reset();
        }
    });
}

/// All shim `(symbol, address)` pairs, for JIT symbol registration.
pub fn runtime_symbols() -> Vec<(&'static str, *const u8)> {
    vec![
        ("leek_dbg_safepoint", leek_dbg_safepoint as *const u8),
        ("leek_dbg_enter", leek_dbg_enter as *const u8),
        ("leek_dbg_leave", leek_dbg_leave as *const u8),
        ("leek_game_builtin", leek_game_builtin as *const u8),
        ("leek_box_int", leek_box_int as *const u8),
        ("leek_box_real", leek_box_real as *const u8),
        ("leek_box_bool", leek_box_bool as *const u8),
        ("leek_box_null", leek_box_null as *const u8),
        ("leek_const_string", leek_const_string as *const u8),
        ("leek_const_bigint", leek_const_bigint as *const u8),
        ("leek_to_bigint", leek_to_bigint as *const u8),
        ("leek_unbox_int", leek_unbox_int as *const u8),
        ("leek_unbox_real", leek_unbox_real as *const u8),
        ("leek_unbox_bool", leek_unbox_bool as *const u8),
        ("leek_array_new", leek_array_new as *const u8),
        ("leek_array_push", leek_array_push as *const u8),
        ("leek_value_index", leek_value_index as *const u8),
        ("leek_field_get", leek_field_get as *const u8),
        ("leek_field_get_int", leek_field_get_int as *const u8),
        ("leek_field_get_real", leek_field_get_real as *const u8),
        ("leek_field_set", leek_field_set as *const u8),
        ("leek_field_get_slot", leek_field_get_slot as *const u8),
        (
            "leek_field_get_slot_int",
            leek_field_get_slot_int as *const u8,
        ),
        (
            "leek_field_get_slot_real",
            leek_field_get_slot_real as *const u8,
        ),
        ("leek_field_set_slot", leek_field_set_slot as *const u8),
        ("leek_set_index_int", leek_set_index_int as *const u8),
        ("leek_index_int", leek_index_int as *const u8),
        ("leek_index_int_raw", leek_index_int_raw as *const u8),
        ("leek_array_get_int", leek_array_get_int as *const u8),
        ("leek_array_get_real", leek_array_get_real as *const u8),
        ("leek_value_set_index", leek_value_set_index as *const u8),
        ("leek_map_new", leek_map_new as *const u8),
        ("leek_map_put", leek_map_put as *const u8),
        ("leek_set_new", leek_set_new as *const u8),
        ("leek_set_add", leek_set_add as *const u8),
        ("leek_set_add_range", leek_set_add_range as *const u8),
        ("leek_object_new", leek_object_new as *const u8),
        ("leek_instance_new", leek_instance_new as *const u8),
        ("leek_global_get", leek_global_get as *const u8),
        ("leek_global_set", leek_global_set as *const u8),
        ("leek_ref_or_builtin", leek_ref_or_builtin as *const u8),
        (
            "leek_call_ref_or_builtin",
            leek_call_ref_or_builtin as *const u8,
        ),
        ("leek_static_get", leek_static_get as *const u8),
        ("leek_static_set", leek_static_set as *const u8),
        ("leek_coerce_scalar", leek_coerce_scalar as *const u8),
        ("leek_slice", leek_slice as *const u8),
        ("leek_interval", leek_interval as *const u8),
        ("leek_count", leek_count as *const u8),
        ("leek_truthy", leek_truthy as *const u8),
        ("leek_value_unary", leek_value_unary as *const u8),
        ("leek_apply_cast", leek_apply_cast as *const u8),
        ("leek_clone_v1", leek_clone_v1 as *const u8),
        ("leek_make_cell", leek_make_cell as *const u8),
        ("leek_cell_get", leek_cell_get as *const u8),
        ("leek_cell_set", leek_cell_set as *const u8),
        ("leek_apply_promotion", leek_apply_promotion as *const u8),
        ("leek_make_lambda", leek_make_lambda as *const u8),
        ("leek_call_value", leek_call_value as *const u8),
        ("leek_call_method", leek_call_method as *const u8),
        ("leek_value_binop", leek_value_binop as *const u8),
        ("leek_value_binop_cir", leek_value_binop_cir as *const u8),
        ("leek_value_binop_cil", leek_value_binop_cil as *const u8),
        ("leek_value_binop_crr", leek_value_binop_crr as *const u8),
        ("leek_value_binop_crl", leek_value_binop_crl as *const u8),
        ("leek_foreach_iter", leek_foreach_iter as *const u8),
        ("leek_foreach_len", leek_foreach_len as *const u8),
        ("leek_class_of", leek_class_of as *const u8),
        ("leek_class_super", leek_class_super as *const u8),
        (
            "leek_construct_builtin",
            leek_construct_builtin as *const u8,
        ),
        ("leek_builtin0", leek_builtin0 as *const u8),
        ("leek_builtin1", leek_builtin1 as *const u8),
        ("leek_builtin2", leek_builtin2 as *const u8),
        ("leek_builtin3", leek_builtin3 as *const u8),
        ("leek_builtin4", leek_builtin4 as *const u8),
        ("leek_charge_ops", leek_charge_ops as *const u8),
        (
            "leek_op_budget_exceeded",
            leek_op_budget_exceeded as *const u8,
        ),
    ]
}
