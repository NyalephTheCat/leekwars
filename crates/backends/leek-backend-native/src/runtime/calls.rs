//! Call dispatch: lambdas, methods, indirect calls through values,
//! builtin dispatch (`leek_builtin0..4`, game builtins), and the
//! [`NativeHost`] that adapts the shared builtin machinery to native.

use super::{
    CLASS_CTOR_THUNK, CLASS_STRING_METHOD, DISPATCH, GLOBALS, LambdaFn, NATIVE_RNG,
    charge_builtin_ops, handle, read_handle, val,
};
use leek_runtime::{BuiltinFlow, BuiltinHost, Function, LambdaCapture, Value};
use std::cell::RefCell;

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
    let Some((addr, _)) = DISPATCH.with(|c| c.borrow().lambda_fns.get(&idx).copied()) else {
        return v;
    };
    let f: LambdaFn = unsafe { std::mem::transmute::<*const u8, LambdaFn>(addr) };
    let argv = [handle(v.clone())];
    let result = unsafe { read_handle(f(argv.as_ptr(), 1)) };
    leek_runtime::DISPLAY_TOP_LEVEL_BARE.with(|c| c.set(true));
    result
}

/// Invoke a function *value* from the host with no JIT frame on the stack —
/// the entry [`crate::run_call`] uses to run a stored AI function (a summon's
/// `FunctionLeekValue`) inside a freshly re-JIT'd module. The per-module
/// dispatch tables must already be installed; `function_idx` / `DefId` keys
/// are stable across re-JITs of the same HIR, so a function value captured
/// during an earlier run resolves against the new module's addresses.
#[must_use]
pub fn call_value_entry(callee: &Value, args: Vec<Value>, version: u8) -> Value {
    let mut host = NativeHost { version };
    dispatch_call_value(&mut host, callee, args)
}

/// Peel a shared `Value::Cell` to its current value; pass any other value
/// through unchanged. Used to convert a cell arg into a plain value for a
/// by-value parameter.
pub(super) fn peel_cell(v: Value) -> Value {
    match v {
        Value::Cell(c) => c.borrow().clone(),
        other => other,
    }
}

/// Resolve call args against a callee's `@`-by-ref `mask`: a by-ref param keeps
/// its arg as-is (a shared `Value::Cell` propagates), a by-value param peels a
/// cell to its value and (v1) deep-clones composites so the callee can't mutate
/// the caller's store. Used by the function-value dispatch arms.
pub(super) fn thread_args(args: Vec<Value>, mask: &[bool], version: u8) -> Vec<Value> {
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

pub(super) fn dispatch_call_value(
    host: &mut NativeHost,
    callee: &Value,
    args: Vec<Value>,
) -> Value {
    match callee {
        Value::Function(Function::Lambda(cap)) => {
            let Some((addr, nparams)) =
                DISPATCH.with(|c| c.borrow().lambda_fns.get(&cap.function_idx).copied())
            else {
                return Value::Null;
            };
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
            // Borrow the by-ref mask in place — the map holds an entry for every
            // lambda, so cloning it here allocated a `Vec<bool>` on every call.
            let ver = host.version();
            let threaded = DISPATCH.with(|c| {
                let b = c.borrow();
                thread_args(
                    args,
                    b.lambda_byref
                        .get(&cap.function_idx)
                        .map_or(&[][..], |m| m.as_slice()),
                    ver,
                )
            });
            for a in threaded {
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
            // Charge the builtin's op cost like the direct-call shims
            // (`leek_builtinN`) do; over budget → skip the dispatch.
            if charge_builtin_ops(name, &args, i64::from(host.version())) {
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
            // One TLS access resolves the index, address+arity, and the
            // exact-arity flag together (was three separate thread-locals).
            let Some((idx, addr, nparams, exact)) = DISPATCH.with(|c| {
                let d = c.borrow();
                let idx = d.user_fn_idx.get(&def.0).copied()?;
                let (addr, nparams) = d.lambda_fns.get(&idx).copied()?;
                Some((idx, addr, nparams, d.user_fn_exact_arity.contains(&def.0)))
            }) else {
                return Value::Null;
            };
            // A method read as a value (`var f = A.m`) requires the EXACT
            // parameter count including the implicit `this` — the interpreter
            // returns null on a mismatch rather than binding missing params to
            // null. Free-function refs keep null-padding (and surplus drop).
            if exact && args.len() != nparams {
                return Value::Null;
            }
            // Borrow the by-ref mask in place (no per-call `Vec<bool>` clone).
            let ver = host.version();
            let mut full = DISPATCH.with(|c| {
                let b = c.borrow();
                thread_args(
                    args,
                    b.lambda_byref.get(&idx).map_or(&[][..], |m| m.as_slice()),
                    ver,
                )
            });
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
        Value::Function(Function::BoundMethod {
            function_idx,
            receiver,
        }) => {
            let Some((addr, nparams)) =
                DISPATCH.with(|c| c.borrow().lambda_fns.get(function_idx).copied())
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
            let Some((addr, nparams)) = DISPATCH.with(|c| c.borrow().lambda_fns.get(&idx).copied())
            else {
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
pub extern "C" fn leek_make_lambda(
    function_idx: i64,
    caps: *const *mut Value,
    ncap: i64,
) -> *mut Value {
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
    let Some(method) = builtin_name_ref(name) else {
        return handle(Value::Null);
    };
    // Instance method on the receiver's runtime class — the hot path. Build the
    // uniform-ABI handle vector DIRECTLY from the caller's receiver + arg
    // handles, skipping the clone-to-`Value`-then-rebox round-trip: the callee
    // shares the same boxed values (the prior `.clone()` was an `Rc` clone, not
    // a deep copy — so field mutations were already visible to the caller).
    if let Value::Instance(inst) = unsafe { val(receiver) } {
        let class_def = inst.borrow().class.0;
        // One TLS access resolves the method index AND its address+arity.
        if let Some((addr, nparams)) = DISPATCH.with(|c| {
            let d = c.borrow();
            d.method_resolve
                .get(&class_def)
                .and_then(|mm| mm.get(method))
                .and_then(|&idx| d.lambda_fns.get(&idx).copied())
        }) {
            let mut handles: Vec<*mut Value> = Vec::with_capacity(nparams.max(argc as usize + 1));
            handles.push(receiver);
            for i in 0..argc as isize {
                handles.push(unsafe { *argv.offset(i) });
            }
            // Pad missing params with null; truncate any surplus args.
            handles.resize_with(nparams, || handle(Value::Null));
            let f: LambdaFn = unsafe { std::mem::transmute::<*const u8, LambdaFn>(addr) };
            return f(handles.as_ptr(), handles.len() as i64);
        }
    }
    // Builtin method fallback (an unknown name / non-number receiver yields null,
    // exactly as the interpreter's `run_builtin` does) — needs owned `Value`s.
    let recv = unsafe { val(receiver) }.clone();
    let mut all = Vec::with_capacity(argc as usize + 1);
    all.push(recv);
    for i in 0..argc as isize {
        all.push(unsafe { val(*argv.offset(i)) }.clone());
    }
    let mut host = NativeHost {
        version: version as u8,
    };
    handle(leek_runtime::call_builtin(&mut host, method, &all).unwrap_or(Value::Null))
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

/// A [`BuiltinHost`] for the native backend's stdlib-builtin calls. Supplies
/// the language version, and — once the lambda table is installed — invokes
/// JIT-compiled lambda callbacks for higher-order builtins. RNG draws go
/// through the per-run persistent [`NATIVE_RNG`], so the sequence advances
/// across calls (and matches the interpreter's).
pub(super) struct NativeHost {
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
                let params = DISPATCH.with(|c| {
                    c.borrow()
                        .lambda_fns
                        .get(&cap.function_idx)
                        .map(|&(_, n)| n)
                })?;
                Some(params.saturating_sub(cap.captured.borrow().len()))
            }
            // A named-function value (`arrayMap(a, f)`): its arity is the
            // compiled param count (no captures prepended for a free function).
            Value::Function(Function::User(def)) => {
                let idx = DISPATCH.with(|c| c.borrow().user_fn_idx.get(&def.0).copied())?;
                DISPATCH.with(|c| c.borrow().lambda_fns.get(&idx).map(|&(_, n)| n))
            }
            Value::Function(Function::Builtin(name)) => leek_runtime::builtin_arity(name),
            // A class reference used as a HOF callback constructs the class;
            // its arity is the constructor thunk's param count, so the higher-
            // order builtin passes the right number of element args.
            Value::ClassRef(def, _) => {
                let idx = CLASS_CTOR_THUNK.with(|c| c.borrow().get(&def.0).copied())?;
                DISPATCH.with(|c| c.borrow().lambda_fns.get(&idx).map(|&(_, n)| n))
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
            && let Some(mask) =
                DISPATCH.with(|c| c.borrow().lambda_byref.get(&cap.function_idx).cloned())
            && mask.iter().any(|&b| b)
        {
            return Some(mask);
        }
        // A named function passed as a HOF callback (`arrayMap(a, f)` where
        // `function f(@x){…}`): resolve its `@`-by-ref mask the same way, so the
        // element is wrapped in a `Value::Cell` and the reassigned value is read
        // back into the array.
        if let Value::Function(Function::User(def)) = callee
            && let Some(idx) = DISPATCH.with(|c| c.borrow().user_fn_idx.get(&def.0).copied())
            && let Some(mask) = DISPATCH.with(|c| c.borrow().lambda_byref.get(&idx).cloned())
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
    let Some(n) = builtin_name_ref(name) else {
        return handle(Value::Null);
    };
    let args: Vec<Value> = (0..argc as isize)
        .map(|i| unsafe { val(*argv.offset(i)) }.clone())
        .collect();
    let mut host = NativeHost {
        version: version as u8,
    };
    if let Some(g) = GLOBALS.with(|g| g.borrow().get(n).copied()) {
        let gv = unsafe { val(g) }.clone();
        return handle(dispatch_call_value(&mut host, &gv, args));
    }
    match leek_runtime::call_builtin(&mut host, n, &args) {
        Ok(v) => handle(v),
        Err(_) => handle(Value::Null),
    }
}

/// Read a builtin's name out of a (boxed string) handle, cloned. For sites
/// that need an owned `String` (a `HashMap` key, a `Function::Builtin`); the
/// hot dispatch shims use [`builtin_name_ref`] instead.
pub(super) fn builtin_name(h: *mut Value) -> Option<String> {
    match unsafe { val(h) } {
        Value::String(s) => Some(s.as_ref().clone()),
        _ => None,
    }
}

/// Borrow a builtin's name out of a (boxed string) handle WITHOUT cloning — for
/// the hot dispatch shims, whose name only flows into `&str` consumers
/// (`charge_builtin_ops` / `call_builtin` / `game::dispatch` / a `HashMap<String,
/// _>` lookup, which borrows `str`). The handle is a live arena box that
/// outlives the call and the bump arena never moves an existing allocation, so
/// the borrow stays valid for the whole dispatch (mirrors [`val`]'s unbounded
/// lifetime). Removes a `String` allocation on every `push`/`abs`/`getCell`/… .
pub(super) fn builtin_name_ref<'a>(h: *mut Value) -> Option<&'a str> {
    match unsafe { val(h) } {
        Value::String(s) => Some(s.as_str()),
        _ => None,
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
    let Some(name) = builtin_name_ref(name) else {
        return handle(Value::Null);
    };
    let args: Vec<Value> = (0..argc as isize)
        .map(|i| unsafe { val(*argv.offset(i)) }.clone())
        .collect();
    handle(leek_runtime::construct_builtin_class(name, args))
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
    let Some(name) = builtin_name_ref(name) else {
        return handle(Value::Null);
    };
    let args: Vec<Value> = (0..argc as isize)
        .map(|i| unsafe { val(*argv.offset(i)) }.clone())
        .collect();
    handle(crate::game::dispatch(name, &args))
}

#[unsafe(no_mangle)]
pub extern "C" fn leek_builtin0(name: *mut Value, version: i64) -> *mut Value {
    let Some(name) = builtin_name_ref(name) else {
        return handle(Value::Null);
    };
    let mut host = NativeHost {
        version: version as u8,
    };
    if charge_builtin_ops(name, &[], version) {
        return handle(Value::Null);
    }
    handle(leek_runtime::call_builtin(&mut host, name, &[]).unwrap_or(Value::Null))
}

#[unsafe(no_mangle)]
pub extern "C" fn leek_builtin1(name: *mut Value, a0: *mut Value, version: i64) -> *mut Value {
    let Some(name) = builtin_name_ref(name) else {
        return handle(Value::Null);
    };
    let mut host = NativeHost {
        version: version as u8,
    };
    let args = [unsafe { val(a0) }.clone()];
    if charge_builtin_ops(name, &args, version) {
        return handle(Value::Null);
    }
    handle(leek_runtime::call_builtin(&mut host, name, &args).unwrap_or(Value::Null))
}

#[unsafe(no_mangle)]
pub extern "C" fn leek_builtin2(
    name: *mut Value,
    a0: *mut Value,
    a1: *mut Value,
    version: i64,
) -> *mut Value {
    let Some(name) = builtin_name_ref(name) else {
        return handle(Value::Null);
    };
    let mut host = NativeHost {
        version: version as u8,
    };
    let args = [unsafe { val(a0) }.clone(), unsafe { val(a1) }.clone()];
    if charge_builtin_ops(name, &args, version) {
        return handle(Value::Null);
    }
    handle(leek_runtime::call_builtin(&mut host, name, &args).unwrap_or(Value::Null))
}

#[unsafe(no_mangle)]
pub extern "C" fn leek_builtin3(
    name: *mut Value,
    a0: *mut Value,
    a1: *mut Value,
    a2: *mut Value,
    version: i64,
) -> *mut Value {
    let Some(name) = builtin_name_ref(name) else {
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
    if charge_builtin_ops(name, &args, version) {
        return handle(Value::Null);
    }
    handle(leek_runtime::call_builtin(&mut host, name, &args).unwrap_or(Value::Null))
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
    let Some(name) = builtin_name_ref(name) else {
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
    if charge_builtin_ops(name, &args, version) {
        return handle(Value::Null);
    }
    handle(leek_runtime::call_builtin(&mut host, name, &args).unwrap_or(Value::Null))
}
