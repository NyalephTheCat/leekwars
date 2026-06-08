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
use std::rc::Rc;

use leek_hir::DefId;
use leek_mir::ir::BinOp;
use leek_runtime::{
    key_repr, BuiltinFlow, BuiltinHost, Function, Instance, IntervalValue, LambdaCapture, MapData,
    ObjectData, Rng, SetData, Value,
};

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

thread_local! {
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

    /// JIT-finalized lambda bodies, keyed by their `program.functions`
    /// index → (address, total param count incl. captures). Populated after
    /// `finalize_definitions` and before `main` runs, so `call_value` /
    /// indirect calls can invoke lambdas. Methods compiled for use as values
    /// (bound methods) are registered here too — keyed by their function
    /// index, value = (uniform-ABI address, param count incl. `this`).
    static LAMBDA_FNS: RefCell<HashMap<usize, (*const u8, usize)>> = RefCell::new(HashMap::new());

    /// Per-lambda `@`-by-reference mask over its **user** parameters (captures
    /// excluded), keyed by `function_idx`. Lets `param_byref_mask` tell the
    /// higher-order builtins (`arrayMap`/`arrayFilter`/…) which callback args to
    /// wrap in a `Value::Cell` so a `@v` reassignment writes back to the element.
    static LAMBDA_BYREF: RefCell<HashMap<usize, Vec<bool>>> = RefCell::new(HashMap::new());

    /// Method resolution for `instance['name']`: `(class_name, method_name)`
    /// → the method's `program.functions` index. Installed per JIT run so the
    /// index shim can build a `BoundMethod` value just like the interpreter's
    /// `read_index_with_methods`.
    static METHOD_RESOLVE: RefCell<HashMap<(String, String), usize>> = RefCell::new(HashMap::new());

    /// Static-field storage, keyed by `(owning-class def_id, field name)`,
    /// each holding a value handle. Lazily initialised on first read.
    static STATIC_FIELDS: RefCell<HashMap<(u32, String), *mut Value>> = RefCell::new(HashMap::new());

    /// Static-field initialisers: `(class def_id, field name)` → the
    /// nullary init function's `program.functions` index (uniform-ABI,
    /// registered in `LAMBDA_FNS`). Only fields with an initialiser appear.
    static STATIC_INIT: RefCell<HashMap<(u32, String), usize>> = RefCell::new(HashMap::new());

    /// Named-function references (`var f = foo`): `DefId` raw → the
    /// function's `program.functions` index (uniform-ABI, in `LAMBDA_FNS`),
    /// so a `Function::User` value can be invoked indirectly.
    static USER_FN_IDX: RefCell<HashMap<u32, usize>> = RefCell::new(HashMap::new());

    /// `DefId`s of user-function values that are *methods* (their function has
    /// an owning class). An indirect call to one requires the EXACT parameter
    /// count (including the implicit `this`); a mismatch yields null rather
    /// than null-padding — mirroring the interpreter's `call_value`.
    static USER_FN_EXACT_ARITY: RefCell<HashSet<u32>> = RefCell::new(HashSet::new());

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

/// Charge the per-character surcharge for a string concatenation result. The
/// interpreter charges `result.chars().count()` on top of the `Add` op cost
/// when an `Add` produced a string (`exec.rs`); the native `Add` boxed path
/// calls this with the result so a `.ops` count over string concat matches.
/// A non-string result (e.g. `array + array`) charges nothing.
#[unsafe(no_mangle)]
pub extern "C" fn leek_charge_concat(result: *mut Value) {
    if let Value::String(s) = unsafe { val(result) } {
        leek_charge_ops(s.chars().count() as i64);
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
fn charge_builtin_ops(name: &str, args: &[Value], version: i64) -> bool {
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
fn raise_runtime_error(code: &str) {
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

/// Apply a class's `string()` override to the *top-level* program result: if
/// `v` is an instance whose class declares a 0-arg `string()`, invoke it and
/// return its value, setting `DISPLAY_TOP_LEVEL_BARE` so a returned string
/// renders without quotes. Mirrors the interpreter's
/// `invoke_instance_string_method`. Any other value passes through unchanged.
pub fn invoke_top_level_string(v: Value) -> Value {
    let Value::Instance(inst) = &v else {
        return v;
    };
    let def = inst.borrow().class.0;
    let Some(idx) = CLASS_STRING_METHOD.with(|c| c.borrow().get(&def).copied()) else {
        return v;
    };
    let Some((addr, _)) = LAMBDA_FNS.with(|c| c.borrow().get(&idx).copied()) else {
        return v;
    };
    let f: LambdaFn = unsafe { std::mem::transmute::<*const u8, LambdaFn>(addr) };
    let argv = [handle(v.clone())];
    let result = unsafe { read_handle(f(argv.as_ptr(), 1)) };
    leek_runtime::DISPLAY_TOP_LEVEL_BARE.with(|c| c.set(true));
    result
}

/// Install the class-parent table for this run.
pub fn set_class_parent(map: HashMap<u32, Option<(u32, String)>>) {
    CLASS_PARENT.with(|c| *c.borrow_mut() = map);
}

/// Install the user-function-reference table for this run.
pub fn set_user_fn_idx(map: HashMap<u32, usize>) {
    USER_FN_IDX.with(|c| *c.borrow_mut() = map);
}

/// Install the set of method-derived user-fn `DefId`s requiring exact arity.
pub fn set_user_fn_exact_arity(set: HashSet<u32>) {
    USER_FN_EXACT_ARITY.with(|c| *c.borrow_mut() = set);
}

/// Install the method-resolution table for this run (clearing any prior).
pub fn set_method_resolve(map: HashMap<(String, String), usize>) {
    METHOD_RESOLVE.with(|c| *c.borrow_mut() = map);
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

/// `C.staticField` read — returns the stored handle, lazily running the
/// field's initialiser on first access (a null sentinel is stored first to
/// break self-referential init cycles). Mirrors the interpreter's lazy
/// static-field initialisation.
#[unsafe(no_mangle)]
pub extern "C" fn leek_static_get(class_def: i64, name: *mut Value) -> *mut Value {
    let Some(field) = builtin_name(name) else {
        return handle(Value::Null);
    };
    let key = (class_def as u32, field);
    if let Some(h) = STATIC_FIELDS.with(|c| c.borrow().get(&key).copied()) {
        return h;
    }
    // Reserve a null sentinel so a recursive init reads null, not garbage.
    let sentinel = handle(Value::Null);
    STATIC_FIELDS.with(|c| c.borrow_mut().insert(key.clone(), sentinel));
    let init = STATIC_INIT.with(|c| c.borrow().get(&key).copied());
    if let Some(idx) = init
        && let Some((addr, _)) = LAMBDA_FNS.with(|c| c.borrow().get(&idx).copied())
    {
        let f: LambdaFn = unsafe { std::mem::transmute::<*const u8, LambdaFn>(addr) };
        let v = f(std::ptr::null(), 0);
        STATIC_FIELDS.with(|c| c.borrow_mut().insert(key, v));
        return v;
    }
    sentinel
}

/// Coerce a boxed value to a declared scalar kind (`0`=int, `1`=real,
/// `2`=bool), preserving `null` (for nullable declared types). Used to make
/// a typed static field's stored value match its declaration (`real? a = 12`
/// reads back `12.0`), mirroring the interpreter's `coerce_to_type`.
#[unsafe(no_mangle)]
pub extern "C" fn leek_coerce_scalar(h: *mut Value, kind: i64) -> *mut Value {
    let v = unsafe { val(h) };
    if matches!(v, Value::Null) {
        return h;
    }
    let coerced = match kind {
        0 => Value::Int(v.to_long()),
        1 => Value::Real(v.to_real()),
        _ => Value::Bool(v.is_truthy()),
    };
    handle(coerced)
}

/// `base[start:end:step]` — slice an array / string / interval. Each bound
/// is a boxed handle; a `null` handle (absent or null-valued bound) means
/// "use the default for that side", matching the interpreter.
#[unsafe(no_mangle)]
pub extern "C" fn leek_slice(
    base: *mut Value,
    start: *mut Value,
    end: *mut Value,
    step: *mut Value,
) -> *mut Value {
    let opt_int = |h: *mut Value| match unsafe { val(h) } {
        Value::Null => None,
        v => Some(v.to_long()),
    };
    let opt_real = |h: *mut Value| match unsafe { val(h) } {
        Value::Null => None,
        v => Some(v.to_real()),
    };
    handle(leek_runtime::slice(
        unsafe { val(base) },
        opt_int(start),
        opt_int(end),
        opt_real(step),
    ))
}

/// `C.staticField = v` — store the handle.
#[unsafe(no_mangle)]
pub extern "C" fn leek_static_set(class_def: i64, name: *mut Value, val: *mut Value) {
    let Some(field) = builtin_name(name) else {
        return;
    };
    STATIC_FIELDS.with(|c| c.borrow_mut().insert((class_def as u32, field), val));
}

/// Install the JIT-finalized lambda table for this run (clearing any prior).
pub fn set_lambda_fns(map: HashMap<usize, (*const u8, usize)>) {
    LAMBDA_FNS.with(|c| *c.borrow_mut() = map);
}

/// Install the per-lambda user-param `@`-by-ref masks for this run.
pub fn set_lambda_byref(map: HashMap<usize, Vec<bool>>) {
    LAMBDA_BYREF.with(|c| *c.borrow_mut() = map);
}

/// Invoke a function *value* with already-unboxed args, returning a `Value`.
/// Lambdas dispatch through their JIT'd uniform-ABI body (captures prepended
/// to the args); builtin references through the shared catalog; user-function
/// and bound-method values aren't supported (return null — gated at compile).
/// Peel a shared `Value::Cell` to its current value; pass any other value
/// through unchanged. Used to convert a cell arg into a plain value for a
/// by-value parameter.
fn peel_cell(v: Value) -> Value {
    match v {
        Value::Cell(c) => c.borrow().clone(),
        other => other,
    }
}

/// Resolve call args against a callee's `@`-by-ref `mask`: a by-ref param keeps
/// its arg as-is (a shared `Value::Cell` propagates), a by-value param peels a
/// cell to its value and (v1) deep-clones composites so the callee can't mutate
/// the caller's store. Used by the function-value dispatch arms.
fn thread_args(args: Vec<Value>, mask: &[bool], version: u8) -> Vec<Value> {
    args.into_iter()
        .enumerate()
        .map(|(i, a)| {
            if mask.get(i).copied().unwrap_or(false) {
                a
            } else {
                let peeled = peel_cell(a);
                if version == 1 {
                    leek_runtime::deep_clone(&peeled)
                } else {
                    peeled
                }
            }
        })
        .collect()
}

fn dispatch_call_value(host: &mut NativeHost, callee: &Value, args: Vec<Value>) -> Value {
    match callee {
        Value::Function(Function::Lambda(cap)) => {
            let Some((addr, nparams)) =
                LAMBDA_FNS.with(|c| c.borrow().get(&cap.function_idx).copied())
            else {
                return Value::Null;
            };
            let mask = LAMBDA_BYREF
                .with(|c| c.borrow().get(&cap.function_idx).cloned())
                .unwrap_or_default();
            let f: LambdaFn = unsafe { std::mem::transmute::<*const u8, LambdaFn>(addr) };
            // Captures are shared cells — pass them raw so the closure observes
            // (and propagates) enclosing-scope mutations. User args are threaded
            // against the by-ref mask (share cells for `@x`, peel + v1-clone for
            // by-value).
            let mut argv: Vec<*mut Value> = cap
                .captured
                .borrow()
                .iter()
                .map(|c| handle(c.clone()))
                .collect();
            for a in thread_args(args, &mask, host.version()) {
                argv.push(handle(a));
            }
            // The uniform body loads exactly `nparams` handles (`[captures…,
            // user-params…]`). An under-arity call (`(x => x)()`) would
            // otherwise leave `argv` short and the body would read past it —
            // an out-of-bounds load that hard-faults. Pad missing user params
            // with null and drop any surplus, matching the interpreter's lax
            // arity (the missing `x` binds to null).
            argv.resize_with(nparams, || handle(Value::Null));
            unsafe { read_handle(f(argv.as_ptr(), argv.len() as i64)) }
        }
        Value::Function(Function::Builtin(name)) => {
            // A builtin takes its args by value — peel any shared cell first (a
            // cell could reach here from an indirect call whose arg native made a
            // cell for a possible by-ref callee).
            let args: Vec<Value> = args.into_iter().map(peel_cell).collect();
            // Indirect builtin calls (`[cos][0]()`) don't get the
            // compile-time default-arg injection direct calls do. If the
            // builtin needs at least one arg and none was passed, return
            // null — matching the interpreter's `call_value`.
            if args.is_empty() && leek_runtime::needs_at_least_one_arg(name) {
                return Value::Null;
            }
            match leek_runtime::call_builtin(host, name, &args) {
                Ok(v) => v,
                Err(_) => Value::Null,
            }
        }
        // A named-function reference (`var f = foo; f(…)`). The function is
        // uniform-compiled and registered by index in `LAMBDA_FNS`; the
        // `DefId` resolves to that index through `USER_FN_IDX`.
        Value::Function(Function::User(def)) => {
            let Some(idx) = USER_FN_IDX.with(|c| c.borrow().get(&def.0).copied()) else {
                return Value::Null;
            };
            let Some((addr, nparams)) = LAMBDA_FNS.with(|c| c.borrow().get(&idx).copied()) else {
                return Value::Null;
            };
            // A method read as a value (`var f = A.m`) requires the EXACT
            // parameter count including the implicit `this` — the interpreter
            // returns null on a mismatch rather than binding missing params to
            // null. Free-function refs keep null-padding (and surplus drop).
            if USER_FN_EXACT_ARITY.with(|c| c.borrow().contains(&def.0)) && args.len() != nparams {
                return Value::Null;
            }
            let mask = LAMBDA_BYREF.with(|c| c.borrow().get(&idx).cloned()).unwrap_or_default();
            let mut full = thread_args(args, &mask, host.version());
            full.resize(nparams, Value::Null);
            let f: LambdaFn = unsafe { std::mem::transmute::<*const u8, LambdaFn>(addr) };
            let argv: Vec<*mut Value> = full.into_iter().map(handle).collect();
            unsafe { read_handle(f(argv.as_ptr(), argv.len() as i64)) }
        }
        // A bound method (`obj['m']` / `obj.m` as a value). Mirrors the
        // interpreter: prepend the stored receiver, unless the caller passed
        // one extra arg over the method's user-arity (`[a['m']][0](a, 5)`),
        // in which case that first arg is the receiver. The method body is
        // compiled with the uniform ABI and registered in `LAMBDA_FNS`.
        Value::Function(Function::BoundMethod { function_idx, receiver }) => {
            let Some((addr, nparams)) = LAMBDA_FNS.with(|c| c.borrow().get(function_idx).copied())
            else {
                return Value::Null;
            };
            // Peel any shared-cell args to plain values (the receiver is added
            // below, unpeeled).
            let args: Vec<Value> = args.into_iter().map(peel_cell).collect();
            let user_arity = nparams.saturating_sub(1);
            let mut full: Vec<Value> = if args.len() == user_arity + 1 {
                args
            } else {
                let mut v = Vec::with_capacity(args.len() + 1);
                v.push((**receiver).clone());
                v.extend(args);
                v
            };
            // The uniform body loads exactly `nparams` handles from `argv`;
            // pad missing args with null and drop any surplus.
            full.resize(nparams, Value::Null);
            let f: LambdaFn = unsafe { std::mem::transmute::<*const u8, LambdaFn>(addr) };
            let argv: Vec<*mut Value> = full.into_iter().map(handle).collect();
            unsafe { read_handle(f(argv.as_ptr(), argv.len() as i64)) }
        }
        // A builtin class invoked as a value (`var c = Array; c(1, 2)`, or a
        // field/object slot holding `Array`/`Map`/…) is constructor sugar —
        // mirrors the interpreter's `call_value` `BuiltinClass` arm.
        Value::BuiltinClass(name) => leek_runtime::construct_builtin_class(name, args),
        // A *user* class reference invoked as a value (`arrayMap(a, A)`, or an
        // object slot holding `A` that's called) constructs an instance via the
        // class's synthetic constructor thunk (uniform-ABI, in `LAMBDA_FNS`).
        // Args are padded to the thunk's arity so an under-arity call binds the
        // missing constructor params to null (matching `Function::User`).
        Value::ClassRef(def, _) => {
            let Some(idx) = CLASS_CTOR_THUNK.with(|c| c.borrow().get(&def.0).copied()) else {
                return Value::Null;
            };
            let Some((addr, nparams)) = LAMBDA_FNS.with(|c| c.borrow().get(&idx).copied()) else {
                return Value::Null;
            };
            let mut full = args;
            full.resize(nparams, Value::Null);
            let f: LambdaFn = unsafe { std::mem::transmute::<*const u8, LambdaFn>(addr) };
            let argv: Vec<*mut Value> = full.into_iter().map(handle).collect();
            unsafe { read_handle(f(argv.as_ptr(), argv.len() as i64)) }
        }
        _ => Value::Null,
    }
}

/// Construct a lambda value capturing `ncap` already-boxed values (snapshot,
/// matching Leekscript value-capture). `caps` points to `ncap` handles.
#[unsafe(no_mangle)]
pub extern "C" fn leek_make_lambda(function_idx: i64, caps: *const *mut Value, ncap: i64) -> *mut Value {
    let captured: Vec<Value> = (0..ncap as isize)
        .map(|i| unsafe { val(*caps.offset(i)) }.clone())
        .collect();
    handle(Value::Function(Function::Lambda(std::rc::Rc::new(
        LambdaCapture {
            function_idx: function_idx as usize,
            captured: RefCell::new(captured),
        },
    ))))
}

/// Indirect call (`f(args)` where `f` is a value): dispatch `callee` with the
/// `argc` boxed args in `argv`. Returns a boxed result.
/// Dynamic method dispatch on an unknown-class receiver: `receiver.method(args)`
/// where the static class isn't known. Mirrors the interpreter's
/// `dispatch_method_call`: if the receiver is a class instance whose runtime
/// class declares `method` (looked up in `METHOD_RESOLVE`), invoke that user
/// method with `receiver` prepended (padded to its arity); otherwise fall back
/// to a builtin method (`run_builtin(method, [receiver, …args])`). This lets a
/// method call on a captured `this` / an `expr as C` cast value dispatch
/// correctly at runtime.
#[unsafe(no_mangle)]
pub extern "C" fn leek_call_method(
    receiver: *mut Value,
    name: *mut Value,
    argv: *const *mut Value,
    argc: i64,
    version: i64,
) -> *mut Value {
    let Some(method) = builtin_name(name) else {
        return handle(Value::Null);
    };
    let recv = unsafe { val(receiver) }.clone();
    let args: Vec<Value> = (0..argc as isize)
        .map(|i| unsafe { val(*argv.offset(i)) }.clone())
        .collect();
    // Instance method on the receiver's runtime class.
    if let Value::Instance(inst) = &recv {
        let class = inst.borrow().class_name.clone();
        if let Some(idx) =
            METHOD_RESOLVE.with(|m| m.borrow().get(&(class, method.clone())).copied())
            && let Some((addr, nparams)) = LAMBDA_FNS.with(|c| c.borrow().get(&idx).copied())
        {
            let mut full = Vec::with_capacity(args.len() + 1);
            full.push(recv);
            full.extend(args);
            full.resize(nparams, Value::Null);
            let f: LambdaFn = unsafe { std::mem::transmute::<*const u8, LambdaFn>(addr) };
            let handles: Vec<*mut Value> = full.into_iter().map(handle).collect();
            return f(handles.as_ptr(), handles.len() as i64);
        }
    }
    // Builtin method fallback (an unknown name / non-number receiver yields null,
    // exactly as the interpreter's `run_builtin` does).
    let mut all = Vec::with_capacity(args.len() + 1);
    all.push(recv);
    all.extend(args);
    let mut host = NativeHost {
        version: version as u8,
    };
    handle(leek_runtime::call_builtin(&mut host, &method, &all).unwrap_or(Value::Null))
}

#[unsafe(no_mangle)]
pub extern "C" fn leek_call_value(
    callee: *mut Value,
    argv: *const *mut Value,
    argc: i64,
    version: i64,
) -> *mut Value {
    let args: Vec<Value> = (0..argc as isize)
        .map(|i| unsafe { val(*argv.offset(i)) }.clone())
        .collect();
    let mut host = NativeHost {
        version: version as u8,
    };
    let callee_v = unsafe { val(callee) };
    handle(dispatch_call_value(&mut host, callee_v, args))
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
        let p = std::ptr::from_mut(s.arena.get_or_insert_with(bumpalo::Bump::new).alloc(v));
        s.boxes.push(p);
        p
    })
}

/// A [`BuiltinHost`] for the native backend's stdlib-builtin calls. Supplies
/// the language version, and — once the lambda table is installed — invokes
/// JIT-compiled lambda callbacks for higher-order builtins. RNG draws go
/// through the per-run persistent [`NATIVE_RNG`], so the sequence advances
/// across calls (and matches the interpreter's).
struct NativeHost {
    version: u8,
}

impl BuiltinHost for NativeHost {
    fn version(&self) -> u8 {
        self.version
    }
    fn rng_int(&mut self, lo: i64, hi: i64) -> i64 {
        NATIVE_RNG.with(|r| r.borrow_mut().int_in(lo, hi))
    }
    fn rng_real(&mut self, lo: f64, hi: f64) -> f64 {
        NATIVE_RNG.with(|r| r.borrow_mut().real_in(lo, hi))
    }
    fn callback_arity(&self, callee: &Value) -> Option<usize> {
        match callee {
            Value::Function(Function::Lambda(cap)) => {
                let params = LAMBDA_FNS.with(|c| c.borrow().get(&cap.function_idx).map(|&(_, n)| n))?;
                Some(params.saturating_sub(cap.captured.borrow().len()))
            }
            // A named-function value (`arrayMap(a, f)`): its arity is the
            // compiled param count (no captures prepended for a free function).
            Value::Function(Function::User(def)) => {
                let idx = USER_FN_IDX.with(|c| c.borrow().get(&def.0).copied())?;
                LAMBDA_FNS.with(|c| c.borrow().get(&idx).map(|&(_, n)| n))
            }
            Value::Function(Function::Builtin(name)) => leek_runtime::builtin_arity(name),
            // A class reference used as a HOF callback constructs the class;
            // its arity is the constructor thunk's param count, so the higher-
            // order builtin passes the right number of element args.
            Value::ClassRef(def, _) => {
                let idx = CLASS_CTOR_THUNK.with(|c| c.borrow().get(&def.0).copied())?;
                LAMBDA_FNS.with(|c| c.borrow().get(&idx).map(|&(_, n)| n))
            }
            _ => None,
        }
    }
    fn param_byref_mask(&self, callee: &Value) -> Option<Vec<bool>> {
        // For a compiled lambda, return its user-param `@`-by-ref mask (if any
        // param is by-ref) so the higher-order builtins wrap those args in a
        // `Value::Cell` and read the reassigned value back into the element.
        // A mask of all-`false` (or an unknown callee) returns `None` — no
        // wrapping needed, preserving the prior behaviour.
        if let Value::Function(Function::Lambda(cap)) = callee
            && let Some(mask) = LAMBDA_BYREF.with(|c| c.borrow().get(&cap.function_idx).cloned())
            && mask.iter().any(|&b| b)
        {
            return Some(mask);
        }
        // A named function passed as a HOF callback (`arrayMap(a, f)` where
        // `function f(@x){…}`): resolve its `@`-by-ref mask the same way, so the
        // element is wrapped in a `Value::Cell` and the reassigned value is read
        // back into the array.
        if let Value::Function(Function::User(def)) = callee
            && let Some(idx) = USER_FN_IDX.with(|c| c.borrow().get(&def.0).copied())
            && let Some(mask) = LAMBDA_BYREF.with(|c| c.borrow().get(&idx).cloned())
            && mask.iter().any(|&b| b)
        {
            return Some(mask);
        }
        None
    }
    fn call_value(&mut self, callee: &Value, args: Vec<Value>) -> Result<Value, BuiltinFlow> {
        Ok(dispatch_call_value(self, callee, args))
    }
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

/// Per-statement debug safepoint. Emitted by the backend when
/// `debug_hooks` is on; forwards the statement's source byte offset and the
/// current frame's local-variable pointers to the installed
/// [`crate::debug::DebugHook`] (which may pause execution).
#[unsafe(no_mangle)]
pub extern "C" fn leek_dbg_safepoint(offset: i64, desc: i64, values: i64) {
    crate::debug::fire_safepoint(offset as u32, desc as usize, values as usize);
}

/// Function-entry debug hook: pushes a shadow call frame for `desc` (the
/// function's `*const VarTable`).
#[unsafe(no_mangle)]
pub extern "C" fn leek_dbg_enter(desc: i64) {
    crate::debug::fire_enter(desc as usize);
}

/// Function-return debug hook: pops the top shadow call frame.
#[unsafe(no_mangle)]
pub extern "C" fn leek_dbg_leave() {
    crate::debug::fire_leave();
}

#[unsafe(no_mangle)]
pub extern "C" fn leek_box_int(i: i64) -> *mut Value {
    handle(Value::Int(i))
}
#[unsafe(no_mangle)]
pub extern "C" fn leek_box_real(r: f64) -> *mut Value {
    handle(Value::Real(r))
}
#[unsafe(no_mangle)]
pub extern "C" fn leek_box_bool(b: i64) -> *mut Value {
    handle(Value::Bool(b != 0))
}
#[unsafe(no_mangle)]
pub extern "C" fn leek_box_null() -> *mut Value {
    handle(Value::Null)
}

/// Build a `Value::String` from `len` bytes at `ptr`. The generated code
/// materializes the literal's bytes in-binary (immediate stores) and passes a
/// pointer to them, so nothing references the *compiler* process's heap — the
/// AOT-relocatable replacement for baking a `box_string` handle as an immediate.
///
/// # Safety
/// `ptr` must point to `len` readable bytes (or be null when `len <= 0`).
#[unsafe(no_mangle)]
pub extern "C" fn leek_const_string(ptr: *const u8, len: i64) -> *mut Value {
    let s = if len <= 0 || ptr.is_null() {
        String::new()
    } else {
        let bytes = unsafe { std::slice::from_raw_parts(ptr, len as usize) };
        String::from_utf8_lossy(bytes).into_owned()
    };
    handle(Value::String(Rc::new(s)))
}

#[unsafe(no_mangle)]
pub extern "C" fn leek_unbox_int(p: *mut Value) -> i64 {
    unsafe { val(p) }.to_long()
}
#[unsafe(no_mangle)]
pub extern "C" fn leek_unbox_real(p: *mut Value) -> f64 {
    unsafe { val(p) }.to_real()
}
#[unsafe(no_mangle)]
pub extern "C" fn leek_unbox_bool(p: *mut Value) -> i64 {
    i64::from(unsafe { val(p) }.is_truthy())
}

#[unsafe(no_mangle)]
pub extern "C" fn leek_array_new() -> *mut Value {
    handle(Value::Array(Rc::new(RefCell::new(Vec::new()))))
}

/// Append a (clone of the) element to an array, in place. Peels a
/// `Value::Cell` receiver (a `@x`-by-ref array returned from a closure can
/// reach here boxed in its shared cell) — `unbox` clones the inner `Value`,
/// which for an array is a shallow `Rc` clone sharing the same backing `Vec`,
/// so the in-place push still mutates the aliased array.
#[unsafe(no_mangle)]
pub extern "C" fn leek_array_push(arr: *mut Value, elem: *mut Value) {
    if let Value::Array(a) = unsafe { val(arr) }.unbox() {
        a.borrow_mut().push(unsafe { val(elem) }.clone());
    }
}

/// Read `base[idx]` for any indexable value (array / string / map / set /
/// object), delegating to the interpreter's `read_index`. `idx` is itself
/// a handle (so map string keys work too).
#[unsafe(no_mangle)]
pub extern "C" fn leek_value_index(base: *mut Value, idx: *mut Value, version: i64) -> *mut Value {
    let base = unsafe { val(base) };
    let idx = unsafe { val(idx) };
    // `x.class.fields` (and `.methods` / `.static_fields` / …) on a runtime
    // class-reference value: return the registered reflection name array (a
    // fresh `Array<String>` each read). The compile-time `C.fields` form is
    // handled in the translator; this is for a `ClassRef` reached dynamically.
    if let (Value::ClassRef(def, _), Value::String(member)) = (base, idx)
        && let Some(names) = CLASS_REFLECT
            .with(|c| c.borrow().get(&def.0).and_then(|m| m.get(member.as_str()).cloned()))
        {
            return handle(Value::Array(std::rc::Rc::new(std::cell::RefCell::new(
                names.into_iter().map(|n| Value::String(std::rc::Rc::new(n))).collect(),
            ))));
        }
    // `instance['name']` resolves to a stored field first, then (like the
    // interpreter's `read_index_with_methods`) to a bound method.
    if let (Value::Instance(inst), Value::String(name)) = (base, idx) {
        let b = inst.borrow();
        if b.fields.get(name.as_str()).is_none() {
            let cls = b.class_name.clone();
            drop(b);
            let key = (cls, name.as_ref().clone());
            if let Some(fidx) = METHOD_RESOLVE.with(|m| m.borrow().get(&key).copied()) {
                return handle(Value::Function(Function::BoundMethod {
                    function_idx: fidx,
                    receiver: Box::new(base.clone()),
                }));
            }
        }
    }
    handle(leek_runtime::read_index_versioned(base, idx, version as u8))
}

/// Read `base[idx]` with an **unboxed** integer index, returning a boxed
/// handle. Identical to [`leek_value_index`] called with a boxed `Int` index:
/// that shim's class-reference / instance-method special cases only fire for a
/// `String` (or `ClassRef`) key, so for an integer index it falls straight
/// through to `read_index_versioned` — exactly what this does. The backend uses
/// it for `base[i]` when `i` is statically `integer`, saving one heap box (the
/// index) per read. The result is still a handle, so every consumer is
/// unaffected.
#[unsafe(no_mangle)]
pub extern "C" fn leek_index_int(base: *mut Value, idx: i64, version: i64) -> *mut Value {
    let b = unsafe { val(base) };
    handle(leek_runtime::read_index_versioned(b, &Value::Int(idx), version as u8))
}

/// Read `base[idx]` (unboxed integer index) and return the element coerced to
/// an **unboxed `i64`** — equivalent to `leek_unbox_int(leek_index_int(..))`
/// (`read_index_versioned(..).to_long()`), with neither the index nor the
/// result ever boxed. Used when the read flows directly into a scalar
/// `integer`-typed slot, whose assignment would `to_long`-coerce the boxed read
/// anyway; the produced value is byte-identical regardless of the element's
/// actual runtime kind (an out-of-bounds `null` reads as `0`, exactly as the
/// boxed path's `to_long(null)` would).
#[unsafe(no_mangle)]
pub extern "C" fn leek_array_get_int(base: *mut Value, idx: i64, version: i64) -> i64 {
    let b = unsafe { val(base) };
    leek_runtime::read_index_versioned(b, &Value::Int(idx), version as u8).to_long()
}

/// Mirror of [`leek_array_get_int`] returning an unboxed `f64` (the read
/// coerced via `to_real`), for a read flowing directly into a `real`-typed
/// slot. Equivalent to `leek_unbox_real(leek_index_int(..))`.
#[unsafe(no_mangle)]
pub extern "C" fn leek_array_get_real(base: *mut Value, idx: i64, version: i64) -> f64 {
    let b = unsafe { val(base) };
    leek_runtime::read_index_versioned(b, &Value::Int(idx), version as u8).to_real()
}

/// Box an arbitrary compile-time-known `Value` (e.g. a builtin constant
/// like `PI` / `SORT_ASC`) into a leaked handle whose pointer is embedded
/// as a constant in the generated code.
pub fn box_value(v: Value) -> *mut Value {
    handle(v)
}

/// Write `base[idx] = value` for any indexable handle (array / map /
/// object), delegating to the interpreter's `set_index`. Both `idx` and
/// `value` are handles. If `base` had to morph to hold the write, the new
/// value is written back into the handle in place.
#[unsafe(no_mangle)]
pub extern "C" fn leek_value_set_index(
    base: *mut Value,
    idx: *mut Value,
    value: *mut Value,
    version: i64,
) {
    // v4-strict: an out-of-bounds array write is a runtime error
    // (`ARRAY_OUT_OF_BOUND`). Non-strict v4 silently drops the write and
    // v1–v3 promote the array to a sparse map, so the check is gated exactly
    // like the interpreter's (`exec.rs`). The write below then no-ops on the
    // OOB index; `run()` surfaces the recorded error after `main` returns.
    if version >= 4
        && STRICT.with(std::cell::Cell::get)
        && let Value::Array(a) = unsafe { val(base) }
    {
        let len = leek_runtime::len_as_int(a.borrow().len());
        let raw = unsafe { val(idx) }.as_int().unwrap_or(0);
        let i = if raw < 0 { raw + len } else { raw };
        if i < 0 || i >= len {
            raise_runtime_error("ARRAY_OUT_OF_BOUND");
            return;
        }
    }
    let v = unsafe { val(value) }.clone();
    let morphed =
        leek_runtime::set_index(unsafe { val(base) }, unsafe { val(idx) }, v, version as u8);
    if let Some(new_base) = morphed {
        // SAFETY: `base` is a live, owned handle (leaked box).
        unsafe {
            *base = new_base;
        }
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn leek_map_new() -> *mut Value {
    handle(Value::Map(Rc::new(RefCell::new(MapData::new()))))
}

/// Insert `key → value` into a map, with the interpreter's key
/// canonicalization (so collection keys reduce the same way).
#[unsafe(no_mangle)]
pub extern "C" fn leek_map_put(map: *mut Value, key: *mut Value, value: *mut Value) {
    if let Value::Map(m) = unsafe { val(map) } {
        let k = unsafe { val(key) }.clone();
        let v = unsafe { val(value) }.clone();
        let canon = key_repr(&k);
        m.borrow_mut().insert_canonical(canon, k, v);
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn leek_set_new() -> *mut Value {
    handle(Value::Set(Rc::new(RefCell::new(SetData::new()))))
}

#[unsafe(no_mangle)]
pub extern "C" fn leek_object_new() -> *mut Value {
    handle(Value::Object(Rc::new(RefCell::new(ObjectData::new()))))
}

/// Allocate a fresh class instance with no fields set. Reads of unset
/// fields return `null` (matching the interpreter's `read_field`), so the
/// emitted `new` only needs to set fields that have initializers. The
/// field initializers and constructor run as separate emitted calls.
/// `class_def` is the class's `DefId.0`; `name_box` is a boxed-string
/// handle carrying the class name (used by `Display`).
#[unsafe(no_mangle)]
pub extern "C" fn leek_instance_new(class_def: i64, name_box: *mut Value) -> *mut Value {
    let class_name = match unsafe { val(name_box) } {
        Value::String(s) => s.to_string(),
        _ => String::new(),
    };
    handle(Value::Instance(Rc::new(RefCell::new(Instance {
        class: DefId(class_def as u32),
        class_name,
        fields: ObjectData::new(),
    }))))
}

/// Read a global by name (a null handle → a fresh `null`, matching the
/// interpreter's treatment of an unset global).
#[unsafe(no_mangle)]
pub extern "C" fn leek_global_get(name: *mut Value) -> *mut Value {
    let Some(name) = builtin_name(name) else {
        return handle(Value::Null);
    };
    GLOBALS.with(|g| {
        g.borrow()
            .get(&name)
            .copied()
            .unwrap_or_else(|| handle(Value::Null))
    })
}

/// Store a global by name (the handle aliases, matching v4 reference
/// semantics; the previous handle is left to leak).
#[unsafe(no_mangle)]
pub extern "C" fn leek_global_set(name: *mut Value, value: *mut Value) {
    if let Some(name) = builtin_name(name) {
        GLOBALS.with(|g| g.borrow_mut().insert(name, value));
    }
}

/// Read a name that is a builtin shadowed by a same-named global
/// (`abs = 2; return abs`, or `var _c = count; count = …`). Mirrors the
/// interpreter's dynamic resolution: the global's value if one has been
/// assigned, otherwise the builtin handle (constant or `Function::Builtin`).
#[unsafe(no_mangle)]
pub extern "C" fn leek_ref_or_builtin(name: *mut Value) -> *mut Value {
    let Some(n) = builtin_name(name) else {
        return handle(Value::Null);
    };
    if let Some(h) = GLOBALS.with(|g| g.borrow().get(&n).copied()) {
        return h;
    }
    if let Some(v) = leek_runtime::lookup_constant(&n) {
        return handle(v);
    }
    handle(Value::Function(Function::Builtin(n)))
}

/// Call a name that is a builtin shadowed by a same-named global
/// (`cos = function(…){…}; cos(1, 2, 3)`). If a global has been assigned,
/// invoke its value; otherwise dispatch the builtin directly.
#[unsafe(no_mangle)]
pub extern "C" fn leek_call_ref_or_builtin(
    name: *mut Value,
    argv: *const *mut Value,
    argc: i64,
    version: i64,
) -> *mut Value {
    let Some(n) = builtin_name(name) else {
        return handle(Value::Null);
    };
    let args: Vec<Value> = (0..argc as isize)
        .map(|i| unsafe { val(*argv.offset(i)) }.clone())
        .collect();
    let mut host = NativeHost {
        version: version as u8,
    };
    if let Some(g) = GLOBALS.with(|g| g.borrow().get(&n).copied()) {
        let gv = unsafe { val(g) }.clone();
        return handle(dispatch_call_value(&mut host, &gv, args));
    }
    match leek_runtime::call_builtin(&mut host, &n, &args) {
        Ok(v) => handle(v),
        Err(_) => handle(Value::Null),
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn leek_set_add(set: *mut Value, elem: *mut Value) {
    if let Value::Set(s) = unsafe { val(set) } {
        s.borrow_mut().insert(unsafe { val(elem) }.clone());
    }
}

/// Build an interval value `[start..end]` from boxed endpoint handles
/// (a null handle means an unbounded end). `flags` packs inclusivity and
/// the `Infinity`-forces-real bits: bit0 start-inclusive, bit1
/// end-inclusive, bit2 start-forces-real, bit3 end-forces-real. Mirrors
/// the interpreter's `materialize_interval` (step is ignored, as there).
#[unsafe(no_mangle)]
pub extern "C" fn leek_interval(start: *mut Value, end: *mut Value, flags: i64) -> *mut Value {
    let bound = |p: *mut Value| -> (Option<f64>, bool) {
        if p.is_null() {
            (None, false)
        } else {
            let v = unsafe { val(p) };
            (Some(v.to_real()), matches!(v, Value::Int(_)))
        }
    };
    let (s, start_is_int) = bound(start);
    let (e, end_is_int) = bound(end);
    handle(Value::Interval(Rc::new(IntervalValue {
        start: s,
        end: e,
        start_inclusive: flags & 1 != 0,
        end_inclusive: flags & 2 != 0,
        integer_typed: start_is_int && end_is_int,
        start_is_int,
        end_is_int,
        start_forces_real: flags & 4 != 0,
        end_forces_real: flags & 8 != 0,
    })))
}

/// Element count of an array / map / set (0 otherwise). A string counts
/// its characters only in v4 — v1–v3 `count("…")` is 0 (strings aren't
/// collections there).
#[unsafe(no_mangle)]
pub extern "C" fn leek_count(p: *mut Value, version: i64) -> i64 {
    match unsafe { val(p) } {
        Value::Array(a) => a.borrow().len() as i64,
        Value::Map(m) => m.borrow().len() as i64,
        Value::Set(s) => s.borrow().len() as i64,
        Value::String(s) if version >= 4 => s.chars().count() as i64,
        _ => 0,
    }
}

/// Truthiness of a boxed value (for branching / `!` on a dynamic value),
/// using the shared `Value::is_truthy`. Returns 0 or 1.
#[unsafe(no_mangle)]
pub extern "C" fn leek_truthy(p: *mut Value) -> i64 {
    unsafe { val(p) }.is_truthy() as i64
}

/// Deep-clone a boxed value for v1 value semantics (assignment / pass-by-
/// value of a composite copies it). Scalars clone trivially.
#[unsafe(no_mangle)]
pub extern "C" fn leek_clone_v1(p: *mut Value) -> *mut Value {
    handle(leek_runtime::deep_clone(unsafe { val(p) }))
}

/// Give a local stable, shared `Value::Cell` storage so writes from either
/// the enclosing scope or a closure are visible to both. If `inner` is
/// already a cell (e.g. a lambda capture-parameter that arrives holding the
/// enclosing scope's cell handle), it is returned unchanged so the shared
/// `Rc` is preserved; otherwise its value is wrapped in a fresh cell.
#[unsafe(no_mangle)]
pub extern "C" fn leek_make_cell(inner: *mut Value) -> *mut Value {
    match unsafe { val(inner) } {
        Value::Cell(_) => inner,
        v => handle(Value::Cell(std::rc::Rc::new(RefCell::new(v.clone())))),
    }
}

/// Read a cell local: clone the value currently behind the cell (peeled).
/// A non-cell handle (defensive) is returned cloned unchanged.
#[unsafe(no_mangle)]
pub extern "C" fn leek_cell_get(cell: *mut Value) -> *mut Value {
    match unsafe { val(cell) } {
        Value::Cell(rc) => handle(rc.borrow().clone()),
        other => handle(other.clone()),
    }
}

/// Write a cell local: store `v` (peeled) into the shared slot, so any
/// closure sharing the cell's `Rc` observes the new value. A no-op on a
/// non-cell handle.
#[unsafe(no_mangle)]
pub extern "C" fn leek_cell_set(cell: *mut Value, v: *mut Value) {
    if let Value::Cell(rc) = unsafe { val(cell) } {
        *rc.borrow_mut() = unsafe { val(v) }.unbox();
    }
}

/// Consume a pending v1 LegacyArray promotion (stashed by a mutating
/// builtin like `push`) and return the promoted value; if none is pending,
/// return `current` unchanged. Used to lower `Statement::ApplyPromotion`.
#[unsafe(no_mangle)]
pub extern "C" fn leek_apply_promotion(current: *mut Value) -> *mut Value {
    match leek_runtime::take_pending_promotion() {
        Some(v) => handle(v),
        None => current,
    }
}

/// Apply a unary operator to a boxed value, returning a new handle.
/// `code`: 0 = negate (`-x`), 1 = bitwise-not (`~x`). Delegates to the
/// shared `leek_runtime` ops so the result matches the interpreter.
#[unsafe(no_mangle)]
pub extern "C" fn leek_value_unary(code: i64, p: *mut Value) -> *mut Value {
    let v = unsafe { val(p) };
    let r = match code {
        0 => leek_runtime::neg(v),
        1 => leek_runtime::bit_not(v),
        _ => Value::Null,
    };
    handle(r)
}

/// Apply a [`leek_mir::ir::CastKind`] to a boxed value, returning a new
/// handle. `code`: 0 = IntToReal, 1 = RealToInt, 2 = ToBool, 3 = ToString,
/// else = User (identity clone). Mirrors the interpreter's `apply_cast`
/// (same `Value` conversion methods), so the result matches exactly.
#[unsafe(no_mangle)]
pub extern "C" fn leek_apply_cast(code: i64, p: *mut Value) -> *mut Value {
    let v = unsafe { val(p) };
    let r = match code {
        0 => Value::Real(v.to_real()),
        1 => Value::Int(v.to_long()),
        2 => Value::Bool(v.is_truthy()),
        3 => Value::String(std::rc::Rc::new(v.to_string())),
        _ => v.clone(),
    };
    handle(r)
}

/// Apply a binary operator to two boxed values, returning a new handle.
/// Delegates to the interpreter's shared `apply_binary`, so the result
/// matches the interpreter exactly (string concat, array `+`, version-
/// specific division, etc.). `code` is a [`BinOp`] encoded via
/// [`binop_code`].
#[unsafe(no_mangle)]
pub extern "C" fn leek_value_binop(
    code: i64,
    a: *mut Value,
    b: *mut Value,
    version: i64,
) -> *mut Value {
    let Some(op) = binop_from_code(code) else {
        return handle(Value::Null);
    };
    let (l, r) = (unsafe { val(a) }, unsafe { val(b) });
    handle(apply_binop(op, l, r, version as u8))
}

/// Like [`leek_value_binop`] but the RIGHT operand is an integer *constant*
/// passed by value — so the backend never boxes it. Used for `dyn OP <int lit>`
/// (`n - 1`, `n < 2`, …), removing one heap allocation per such operation; the
/// constant `Value::Int` lives on the stack. Identical result to boxing it.
#[unsafe(no_mangle)]
pub extern "C" fn leek_value_binop_cir(code: i64, a: *mut Value, c: i64, version: i64) -> *mut Value {
    let Some(op) = binop_from_code(code) else {
        return handle(Value::Null);
    };
    let l = unsafe { val(a) };
    handle(apply_binop(op, l, &Value::Int(c), version as u8))
}

/// Mirror of [`leek_value_binop_cir`] for a LEFT integer constant
/// (`<int lit> OP dyn`) — order preserved for non-commutative ops.
#[unsafe(no_mangle)]
pub extern "C" fn leek_value_binop_cil(code: i64, c: i64, b: *mut Value, version: i64) -> *mut Value {
    let Some(op) = binop_from_code(code) else {
        return handle(Value::Null);
    };
    let r = unsafe { val(b) };
    handle(apply_binop(op, &Value::Int(c), r, version as u8))
}

/// Pure dispatch of a [`BinOp`] onto the shared `leek_runtime` operators —
/// the single source of truth shared by the JIT `leek_value_binop` shim and
/// the compile-time const-evaluator (`const_eval_default`). Matches the
/// interpreter exactly (string concat, array `+`, version-specific division).
pub fn apply_binop(op: BinOp, l: &Value, r: &Value, v: u8) -> Value {
    use leek_runtime as rt;
    use BinOp::{Add, Sub, Mul, Div, Mod, IntDiv, Pow, Eq, Ne, IdentityEq, IdentityNe, Lt, Le, Gt, Ge, BitAnd, BitOr, BitXor, CompoundXor, Xor, ShiftL, ShiftR, UShiftR, In, NotIn, Is, Instanceof};
    match op {
        Add => rt::add(l, r),
        Sub => rt::sub(l, r),
        Mul => rt::mul(l, r),
        Div => rt::div(l, r, v),
        Mod => rt::rem(l, r),
        IntDiv => rt::int_div(l, r),
        Pow => rt::pow(l, r),
        Eq => rt::eq(l, r, v),
        Ne => rt::ne(l, r, v),
        IdentityEq => rt::identity_eq(l, r),
        IdentityNe => rt::identity_ne(l, r),
        Lt => rt::lt(l, r),
        Le => rt::le(l, r),
        Gt => rt::gt(l, r),
        Ge => rt::ge(l, r),
        BitAnd => rt::bit_and(l, r),
        BitOr => rt::bit_or(l, r),
        BitXor => rt::bit_xor(l, r),
        CompoundXor => rt::compound_xor(l, r, v),
        Xor => rt::xor(l, r),
        ShiftL => rt::shl(l, r),
        ShiftR => rt::shr(l, r),
        UShiftR => rt::ushr(l, r),
        In => rt::in_op(l, r),
        NotIn => rt::not_in(l, r),
        Is => rt::is(l, r),
        Instanceof => rt::instanceof(l, r),
    }
}

/// Build a `foreach` iterator (`[key, value]` pairs) for an iterable.
#[unsafe(no_mangle)]
pub extern "C" fn leek_foreach_iter(iterable: *mut Value) -> *mut Value {
    handle(leek_runtime::make_foreach_iter(unsafe { val(iterable) }))
}

/// Encode a [`BinOp`] as a stable `i64` for the FFI boundary.
pub fn binop_code(op: BinOp) -> i64 {
    op as i64
}

/// Decode a [`binop_code`] back into a [`BinOp`].
fn binop_from_code(c: i64) -> Option<BinOp> {
    use BinOp::{Add, Sub, Mul, Div, Mod, IntDiv, Pow, Eq, Ne, IdentityEq, IdentityNe, Lt, Le, Gt, Ge, BitAnd, BitOr, BitXor, CompoundXor, Xor, ShiftL, ShiftR, UShiftR, In, NotIn, Is, Instanceof};
    let op = match c {
        x if x == Add as i64 => Add,
        x if x == Sub as i64 => Sub,
        x if x == Mul as i64 => Mul,
        x if x == Div as i64 => Div,
        x if x == Mod as i64 => Mod,
        x if x == IntDiv as i64 => IntDiv,
        x if x == Pow as i64 => Pow,
        x if x == Eq as i64 => Eq,
        x if x == Ne as i64 => Ne,
        x if x == IdentityEq as i64 => IdentityEq,
        x if x == IdentityNe as i64 => IdentityNe,
        x if x == Lt as i64 => Lt,
        x if x == Le as i64 => Le,
        x if x == Gt as i64 => Gt,
        x if x == Ge as i64 => Ge,
        x if x == BitAnd as i64 => BitAnd,
        x if x == BitOr as i64 => BitOr,
        x if x == BitXor as i64 => BitXor,
        x if x == CompoundXor as i64 => CompoundXor,
        x if x == Xor as i64 => Xor,
        x if x == ShiftL as i64 => ShiftL,
        x if x == ShiftR as i64 => ShiftR,
        x if x == UShiftR as i64 => UShiftR,
        x if x == In as i64 => In,
        x if x == NotIn as i64 => NotIn,
        x if x == Is as i64 => Is,
        x if x == Instanceof as i64 => Instanceof,
        _ => return None,
    };
    Some(op)
}

/// Read a builtin's name out of a (boxed string) handle.
fn builtin_name(h: *mut Value) -> Option<String> {
    match unsafe { val(h) } {
        Value::String(s) => Some(s.as_ref().clone()),
        _ => None,
    }
}

/// Call a stdlib builtin by name on boxed argument handles, dispatching
/// through the shared `leek_runtime::call_builtin` with a trivial native
/// host. One shim per small arity (most builtins take ≤ 3 args).
/// The `.class` meta-property: the runtime class of a value.
#[unsafe(no_mangle)]
pub extern "C" fn leek_class_of(v: *mut Value) -> *mut Value {
    handle(leek_runtime::class_of(unsafe { val(v) }))
}

/// `.super` on a (runtime) class value: the parent class. A user class with an
/// explicit parent yields that class's ref; one with no explicit parent yields
/// the builtin `Value` base. A non-class value yields null.
#[unsafe(no_mangle)]
pub extern "C" fn leek_class_super(v: *mut Value) -> *mut Value {
    match unsafe { val(v) } {
        Value::ClassRef(def, _) => match CLASS_PARENT.with(|c| c.borrow().get(&def.0).cloned()) {
            // Explicit user parent.
            Some(Some((pdef, pname))) => handle(Value::ClassRef(DefId(pdef), Rc::new(pname))),
            // User class with no explicit parent → the implicit `Value` root.
            Some(None) => handle(Value::BuiltinClass("Value")),
            None => handle(Value::Null),
        },
        // Every builtin class extends the `Value` root; `Value` itself has no
        // super.
        Value::BuiltinClass("Value") => handle(Value::Null),
        Value::BuiltinClass(_) => handle(Value::BuiltinClass("Value")),
        _ => handle(Value::Null),
    }
}

/// `new <BuiltinClass>(args)` — construct an `Array`/`Map`/`Set`/`Object`/
/// boxed scalar from the boxed args. Delegates to the shared
/// `construct_builtin_class`.
#[unsafe(no_mangle)]
pub extern "C" fn leek_construct_builtin(
    name: *mut Value,
    argv: *const *mut Value,
    argc: i64,
) -> *mut Value {
    let Some(name) = builtin_name(name) else {
        return handle(Value::Null);
    };
    let args: Vec<Value> = (0..argc as isize)
        .map(|i| unsafe { val(*argv.offset(i)) }.clone())
        .collect();
    handle(leek_runtime::construct_builtin_class(&name, args))
}

/// Host game builtin (`getCell`, `getLife`, …): unbox the args and forward
/// to the installed [`crate::game::GameRuntime`]. Emitted by the backend when
/// `link_game` is on for a builtin it doesn't otherwise handle.
#[unsafe(no_mangle)]
pub extern "C" fn leek_game_builtin(
    name: *mut Value,
    argv: *const *mut Value,
    argc: i64,
) -> *mut Value {
    let Some(name) = builtin_name(name) else {
        return handle(Value::Null);
    };
    let args: Vec<Value> = (0..argc as isize)
        .map(|i| unsafe { val(*argv.offset(i)) }.clone())
        .collect();
    handle(crate::game::dispatch(&name, &args))
}

#[unsafe(no_mangle)]
pub extern "C" fn leek_builtin0(name: *mut Value, version: i64) -> *mut Value {
    let Some(name) = builtin_name(name) else {
        return handle(Value::Null);
    };
    let mut host = NativeHost {
        version: version as u8,
    };
    if charge_builtin_ops(&name, &[], version) {
        return handle(Value::Null);
    }
    handle(leek_runtime::call_builtin(&mut host, &name, &[]).unwrap_or(Value::Null))
}

#[unsafe(no_mangle)]
pub extern "C" fn leek_builtin1(name: *mut Value, a0: *mut Value, version: i64) -> *mut Value {
    let Some(name) = builtin_name(name) else {
        return handle(Value::Null);
    };
    let mut host = NativeHost {
        version: version as u8,
    };
    let args = [unsafe { val(a0) }.clone()];
    if charge_builtin_ops(&name, &args, version) {
        return handle(Value::Null);
    }
    handle(leek_runtime::call_builtin(&mut host, &name, &args).unwrap_or(Value::Null))
}

#[unsafe(no_mangle)]
pub extern "C" fn leek_builtin2(
    name: *mut Value,
    a0: *mut Value,
    a1: *mut Value,
    version: i64,
) -> *mut Value {
    let Some(name) = builtin_name(name) else {
        return handle(Value::Null);
    };
    let mut host = NativeHost {
        version: version as u8,
    };
    let args = [unsafe { val(a0) }.clone(), unsafe { val(a1) }.clone()];
    if charge_builtin_ops(&name, &args, version) {
        return handle(Value::Null);
    }
    handle(leek_runtime::call_builtin(&mut host, &name, &args).unwrap_or(Value::Null))
}

#[unsafe(no_mangle)]
pub extern "C" fn leek_builtin3(
    name: *mut Value,
    a0: *mut Value,
    a1: *mut Value,
    a2: *mut Value,
    version: i64,
) -> *mut Value {
    let Some(name) = builtin_name(name) else {
        return handle(Value::Null);
    };
    let mut host = NativeHost {
        version: version as u8,
    };
    let args = [
        unsafe { val(a0) }.clone(),
        unsafe { val(a1) }.clone(),
        unsafe { val(a2) }.clone(),
    ];
    if charge_builtin_ops(&name, &args, version) {
        return handle(Value::Null);
    }
    handle(leek_runtime::call_builtin(&mut host, &name, &args).unwrap_or(Value::Null))
}

#[unsafe(no_mangle)]
pub extern "C" fn leek_builtin4(
    name: *mut Value,
    a0: *mut Value,
    a1: *mut Value,
    a2: *mut Value,
    a3: *mut Value,
    version: i64,
) -> *mut Value {
    let Some(name) = builtin_name(name) else {
        return handle(Value::Null);
    };
    let mut host = NativeHost {
        version: version as u8,
    };
    let args = [
        unsafe { val(a0) }.clone(),
        unsafe { val(a1) }.clone(),
        unsafe { val(a2) }.clone(),
        unsafe { val(a3) }.clone(),
    ];
    if charge_builtin_ops(&name, &args, version) {
        return handle(Value::Null);
    }
    handle(leek_runtime::call_builtin(&mut host, &name, &args).unwrap_or(Value::Null))
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
        ("leek_unbox_int", leek_unbox_int as *const u8),
        ("leek_unbox_real", leek_unbox_real as *const u8),
        ("leek_unbox_bool", leek_unbox_bool as *const u8),
        ("leek_array_new", leek_array_new as *const u8),
        ("leek_array_push", leek_array_push as *const u8),
        ("leek_value_index", leek_value_index as *const u8),
        ("leek_index_int", leek_index_int as *const u8),
        ("leek_array_get_int", leek_array_get_int as *const u8),
        ("leek_array_get_real", leek_array_get_real as *const u8),
        ("leek_value_set_index", leek_value_set_index as *const u8),
        ("leek_map_new", leek_map_new as *const u8),
        ("leek_map_put", leek_map_put as *const u8),
        ("leek_set_new", leek_set_new as *const u8),
        ("leek_set_add", leek_set_add as *const u8),
        ("leek_object_new", leek_object_new as *const u8),
        ("leek_instance_new", leek_instance_new as *const u8),
        ("leek_global_get", leek_global_get as *const u8),
        ("leek_global_set", leek_global_set as *const u8),
        ("leek_ref_or_builtin", leek_ref_or_builtin as *const u8),
        ("leek_call_ref_or_builtin", leek_call_ref_or_builtin as *const u8),
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
        ("leek_foreach_iter", leek_foreach_iter as *const u8),
        ("leek_class_of", leek_class_of as *const u8),
        ("leek_class_super", leek_class_super as *const u8),
        ("leek_construct_builtin", leek_construct_builtin as *const u8),
        ("leek_builtin0", leek_builtin0 as *const u8),
        ("leek_builtin1", leek_builtin1 as *const u8),
        ("leek_builtin2", leek_builtin2 as *const u8),
        ("leek_builtin3", leek_builtin3 as *const u8),
        ("leek_builtin4", leek_builtin4 as *const u8),
        ("leek_charge_ops", leek_charge_ops as *const u8),
        ("leek_op_budget_exceeded", leek_op_budget_exceeded as *const u8),
        ("leek_charge_concat", leek_charge_concat as *const u8),
    ]
}
