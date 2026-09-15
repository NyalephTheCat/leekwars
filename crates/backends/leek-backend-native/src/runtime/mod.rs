//! C-ABI runtime shims for composite values (arrays first).
//!
//! Native code keeps composite — and boxed scalar — values as opaque
//! `*mut Value` *handles*. A handle points into a per-run bump arena
//! ([`handle`]): there is no garbage collector, and none is needed,
//! because [`free_run_boxes`] drops every value the run allocated and
//! resets the arena in one sweep once the result has been read out. The
//! arena keeps its capacity between runs, so a program run repeatedly —
//! a fight turn loop — reaches a steady state rather than growing.
//!
//! The shims box and unbox scalars and implement composite operations by
//! delegating to the shared `leek_runtime` value logic, so native and
//! the Java backend compute the same answers from one implementation.
//!
//! # Handle safety contract
//!
//! A shim that takes a handle should be an `unsafe extern "C" fn` whose
//! `# Safety` section defers to this one contract (values.rs is converted;
//! calls.rs / collections.rs / objects.rs are not yet — see #114). For each
//! handle parameter the caller — JIT'd or AOT'd code, or another shim —
//! promises:
//!
//! 1. **Provenance.** The pointer was produced by [`handle`] (live until
//!    [`free_run_boxes`] ends the run) or by [`box_value`] / `const_handle`
//!    (live until the owning module's `ConstArena` is dropped). Null is never a
//!    handle, except where a shim documents it (`leek_interval`'s open bounds).
//! 2. **Alignment and initialisation.** It points at a fully initialised
//!    `Value` — guaranteed by (1), since both allocators bump-allocate a
//!    `Value` and never hand back the storage.
//! 3. **No write while borrowed.** The runtime is single-threaded per run, and
//!    no write through the handle (or through an aliasing handle) happens while
//!    a borrow taken by [`val`] is alive. Two parameters CAN be the same handle
//!    — `a[a] = x` passes one as both base and index — which is why
//!    [`objects::set_member`] takes its index raw and keeps every borrow
//!    derived from it inside a statement that writes nothing.
//!
//! Shims that take only scalars (`leek_box_int`, `leek_map_new`, …) promise
//! nothing and stay safe.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};

use leek_runtime::{Rng, Value};

// First: defines the `shim!` macro the shim modules below declare their
// `extern "C"` functions with (`macro_rules!` scoping is textual).
#[macro_use]
mod guard;
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
///
/// `unsafe`, because calling one runs machine code the borrow checker has never
/// seen: the address must come from the `lambda_fns` table of the module
/// currently executing, and `argv` must point at `argc` live handles.
type LambdaFn = unsafe extern "C" fn(*const *mut Value, i64) -> *mut Value;

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
    /// The same for *static* methods, flattened over inheritance, so a
    /// `ClassRef` reached at runtime (`class.m()` in an instance method, where
    /// `class` is the receiver's class) can dispatch one.
    static_method_resolve: HashMap<u32, HashMap<String, usize>>,
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

    /// Storage for the handles baked into *generated code* as constants (see
    /// [`box_value`]). Kept apart from [`BOX_STATE`] because their lifetime is
    /// the compiled module's, not the run's: a module that is reused across
    /// turns ([`crate::CompiledProgram`]) would read freed memory on its second
    /// run if its constants were swept by [`free_run_boxes`]. The compiler
    /// hands the accumulated arena to the module it just built with
    /// [`take_const_arena`], and the module frees it when it is dropped.
    static CONST_STATE: RefCell<BoxState> =
        const { RefCell::new(BoxState { arena: None, boxes: Vec::new() }) };

    /// File-level globals, keyed by name (matching upstream), each
    /// holding a value handle. Cleared by [`clear_globals`] before every
    /// JIT run so programs don't see a previous run's globals.
    static GLOBALS: RefCell<HashMap<String, *mut Value>> = RefCell::new(HashMap::new());

    /// The program's PRNG — ONE generator persisted across the whole run
    /// (one RNG per run, as upstream has), so successive `rand`/`randInt`
    /// calls advance a single xorshift sequence. (Constructing a fresh
    /// `Rng::new()` per builtin shim, as native used to, reset the
    /// sequence on every call.) Default-seeded and reset per run in
    /// [`clear_globals`]; the same seed + sequence as upstream, so
    /// native reproduces upstream's RNG-dependent results exactly.
    static NATIVE_RNG: RefCell<Rng> = RefCell::new(Rng::new());

    /// Static-field storage, keyed by `(owning-class def_id, field name)`,
    /// each holding a value handle. Lazily initialised on first read.
    static STATIC_FIELDS: RefCell<HashMap<(u32, String), *mut Value>> = RefCell::new(HashMap::new());

    /// Static-field initialisers: `(class def_id, field name)` → the
    /// nullary init function's `program.functions` index (uniform-ABI,
    /// registered in `LAMBDA_FNS`). Only fields with an initialiser appear.
    static STATIC_INIT: RefCell<HashMap<(u32, String), usize>> = RefCell::new(HashMap::new());

    /// Which class *declares* each static field reachable from a class:
    /// class `DefId` raw → field name → owning class `DefId` raw. Flattened
    /// over inheritance, since a subclass reads and writes its parent's
    /// storage. Lets a `ClassRef` reached at runtime — `class.x` inside an
    /// instance method — find the same box the compile-time `C.x` path uses.
    static STATIC_FIELD_OWNER: RefCell<HashMap<u32, HashMap<String, u32>>> =
        RefCell::new(HashMap::new());

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
    /// goes through `string()` (mirroring upstream's
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

    /// Set the moment [`RUNTIME_ERROR`] is first recorded (op budget spent,
    /// out-of-bounds strict write, internal panic, …). Upstream Java throws at
    /// that point, so nothing after it may take effect: loop back-edges poll
    /// this flag to leave the JIT'd code, and every side-effecting shim (game
    /// actions, builtins, stores) becomes a no-op once it is set. A plain
    /// `Cell<bool>` so the hot polls skip the `RefCell` borrow.
    static ABORT: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };

    /// User-function frames currently active in this run: bumped by every
    /// compiled function's entry prologue ([`leek_enter_frame`]) and dropped on
    /// each return ([`leek_leave_frame`]). `main` is not counted.
    static CALL_DEPTH: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };

    /// Call-depth limit for the current run; entering a frame beyond it raises
    /// `STACKOVERFLOW` instead of recursing until the native stack overflows.
    static MAX_CALL_DEPTH: std::cell::Cell<u32> =
        const { std::cell::Cell::new(crate::options::DEFAULT_MAX_CALL_DEPTH) };

    /// Lowest stack address a user frame may start at this run (0 = no
    /// check). A backstop for the depth counter: frames reached through the
    /// Rust dispatch shims (function values, callbacks) cost far more stack
    /// than direct JIT calls, so the counter alone can't keep every call path
    /// inside a small thread stack.
    static STACK_FLOOR: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };

    /// Whether the current run uses strict typing. Mirrors upstream's
    /// `strict` flag, which some runtime-fault rules depend on (e.g. an
    /// out-of-bounds array write only errors under v4 *strict*).
    static STRICT: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };

    /// Operations charged during the current run. The JIT'd code calls
    /// [`leek_charge_ops`] at the same MIR sites upstream charges, so
    /// the two backends produce identical op counts. Read after `main` returns.
    static OP_COUNT: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };

    /// Operation budget for the current run. When [`OP_COUNT`] exceeds it,
    /// `leek_charge_ops` records `TOO_MUCH_OPERATIONS` (mirroring the
    /// upstream's op charging). `u64::MAX` ≈ unlimited (for op-count
    /// verification of small programs that must run to completion).
    static OP_LIMIT: std::cell::Cell<u64> = const { std::cell::Cell::new(u64::MAX) };
}

thread_local! {}

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
/// The borrow is tied to the *handle variable*, not to an inferred (and
/// therefore unbounded) lifetime: `val(&h)` reborrows through `h`, so the
/// resulting `&Value` cannot outlive the shim frame that received `h`, and the
/// borrow checker rejects a write through `h` while it is live.
///
/// # Safety
/// `*p` must satisfy the [module-level handle contract](self#handle-safety-contract):
/// a live handle, not written through (directly or via an aliasing handle)
/// while the returned borrow is alive.
#[inline]
unsafe fn val(p: &*mut Value) -> &Value {
    // SAFETY: caller's contract — `*p` is a live handle, and the reborrow the
    // signature ties to `p` keeps the result inside the caller's frame.
    unsafe { &**p }
}

/// Box an arbitrary compile-time-known `Value` (e.g. a builtin constant
/// like `PI` / `SORT_ASC`) into a handle whose pointer is embedded as a
/// constant in the generated code.
///
/// Called from `translate/` *during codegen* only, so the handle must outlive
/// every run of the module being built — not just the current one. It is
/// allocated from [`CONST_STATE`], which [`take_const_arena`] transfers to the
/// finished module; [`free_run_boxes`] never touches it.
pub fn box_value(v: Value) -> *mut Value {
    const_handle(v)
}

/// [`handle`], but allocating from the compile-time arena. Same drop-list rule:
/// only values that own heap are recorded, the rest ride the arena.
fn const_handle(v: Value) -> *mut Value {
    CONST_STATE.with(|s| {
        let s = &mut *s.borrow_mut();
        let r = s.arena.get_or_insert_with(bumpalo::Bump::new).alloc(v);
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

/// The constant handles allocated since the last [`take_const_arena`] — i.e.
/// every constant baked into the module currently being compiled. Owned by
/// that module from here on: dropping it drops the values and releases the
/// arena, which invalidates the pointers the module's code holds, so it must
/// not outlive the machine code that reads them.
pub struct ConstArena {
    /// Held only to be dropped: releasing it reclaims the bump storage every
    /// `boxes` pointer below lives in.
    _arena: Option<bumpalo::Bump>,
    boxes: Vec<*mut Value>,
}

impl Drop for ConstArena {
    fn drop(&mut self) {
        for p in self.boxes.drain(..) {
            // SAFETY: `p` is a unique, still-live value in `arena`, produced by
            // `const_handle` and dropped exactly once (reads clone, never free),
            // and `arena` is dropped only after this loop.
            unsafe { std::ptr::drop_in_place(p) };
        }
    }
}

impl std::fmt::Debug for ConstArena {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ConstArena")
            .field("boxes", &self.boxes.len())
            .finish_non_exhaustive()
    }
}

/// Hand the constants accumulated during the compile just finished to their
/// module, leaving a fresh arena for the next compile.
pub fn take_const_arena() -> ConstArena {
    CONST_STATE.with(|s| {
        let s = &mut *s.borrow_mut();
        ConstArena {
            _arena: s.arena.take(),
            boxes: std::mem::take(&mut s.boxes),
        }
    })
}

/// Read the `Value` behind a handle by cloning it, WITHOUT freeing the box.
/// The box stays owned by the per-run registry in [`BOX_STATE`] and is
/// reclaimed in a single sweep by [`free_run_boxes`] at run end — so handles
/// are never freed
/// at the read site, which keeps box ownership in exactly one place and makes a
/// double-free impossible. Used at the JIT boundary to read the program's
/// result and at each lambda-callback return.
///
/// # Safety
/// `p` must be a live handle (created by [`handle`] and not yet swept).
pub unsafe fn read_handle(p: *mut Value) -> Value {
    // SAFETY: caller's contract — `p` is a live, un-swept handle.
    unsafe { (*p).clone() }
}

/// Bytes of chunk capacity the per-run value arena currently holds (NOT live
/// bytes — [`free_run_boxes`]'s `reset` keeps the largest chunk). Crate-internal
/// probe for the leak tests: a program run repeatedly must reach a steady state
/// where this stops growing, otherwise boxes are outliving their run.
#[cfg(test)]
pub(crate) fn arena_allocated_bytes() -> usize {
    BOX_STATE.with(|s| {
        s.borrow()
            .arena
            .as_ref()
            .map_or(0, bumpalo::Bump::allocated_bytes)
    })
}

/// Reclaim every value handle allocated during the current run. Call once the
/// run's result has been read out (cloned) — see [`read_handle`] — so the
/// result `Value` and anything reachable from it (held by its own `Rc` clones)
/// survives.
///
/// Each handle's storage lives in the bump arena held by [`BOX_STATE`];
/// bumpalo doesn't run
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
        (
            "leek_value_set_index_raw",
            leek_value_set_index_raw as *const u8,
        ),
        (
            "leek_set_index_int_raw",
            leek_set_index_int_raw as *const u8,
        ),
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
        ("leek_value_binop_raw", leek_value_binop_raw as *const u8),
        ("leek_field_convert", leek_field_convert as *const u8),
        ("leek_static_convert", leek_static_convert as *const u8),
        ("leek_global_convert", leek_global_convert as *const u8),
        ("leek_convert_slot", leek_convert_slot as *const u8),
        ("leek_check_param", leek_check_param as *const u8),
        ("leek_check_container", leek_check_container as *const u8),
        ("leek_value_binop_cir", leek_value_binop_cir as *const u8),
        ("leek_value_binop_cil", leek_value_binop_cil as *const u8),
        ("leek_value_binop_crr", leek_value_binop_crr as *const u8),
        ("leek_value_binop_crl", leek_value_binop_crl as *const u8),
        ("leek_foreach_iter", leek_foreach_iter as *const u8),
        ("leek_foreach_len", leek_foreach_len as *const u8),
        ("leek_iter_value", leek_iter_value as *const u8),
        ("leek_iter_key", leek_iter_key as *const u8),
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
        ("leek_enter_frame", leek_enter_frame as *const u8),
        ("leek_leave_frame", leek_leave_frame as *const u8),
    ]
}

#[cfg(test)]
mod tests {
    //! The per-run box invariant: every handle a run allocates is dropped by
    //! [`free_run_boxes`], and the arena backing them reaches a steady size
    //! instead of growing run after run.

    use std::rc::Rc;

    use leek_runtime::Value;

    use super::{BOX_STATE, arena_allocated_bytes, free_run_boxes, handle};

    #[test]
    fn free_run_boxes_empties_the_drop_list_and_the_arena_stops_growing() {
        // Heap-owning values only: `handle` deliberately does not register
        // trivially-droppable scalars for the sweep (their bump storage is
        // reclaimed by `reset` regardless).
        let mut steady = None;
        for round in 0..200 {
            for _ in 0..500 {
                let _ = handle(Value::String(Rc::new("x".repeat(64))));
            }
            free_run_boxes();
            assert_eq!(
                BOX_STATE.with(|s| s.borrow().boxes.len()),
                0,
                "round {round}: handles survived the sweep"
            );
            // `reset` retains only the LARGEST chunk, so the first rounds
            // legitimately grow the arena as bumpalo doubles up to the round's
            // working set. Steady state is reached well before round 2; compare
            // against that, never against round 0.
            if round == 2 {
                steady = Some(arena_allocated_bytes());
            }
        }
        let steady = steady.expect("round 2 ran");
        assert_eq!(
            arena_allocated_bytes(),
            steady,
            "the value arena kept growing across rounds: boxes are outliving `free_run_boxes`"
        );
    }
}
