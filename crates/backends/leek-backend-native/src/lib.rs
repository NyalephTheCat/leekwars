//! Native backend: MIR → Cranelift IR → machine code.
//!
//! First slice: JIT-compiles the scalar (integer / boolean) +
//! control-flow subset of MIR and runs it in-process. Unsupported
//! constructs surface as [`NativeError::Unsupported`] so the corpus
//! runner can skip them while coverage grows.
//!
//! Compilation options live in [`NativeOptions`]: optimization level
//! (debug vs release), the IR verifier, frame-pointer preservation,
//! DWARF debug info, and the emit mode — run via JIT, dump the
//! Cranelift IR / disassembly (for inspecting generated code), or
//! write a relocatable object file.

// This crate inherits the workspace lint table (see Cargo.toml). Its Cranelift
// JIT path transmutes and calls finalized function pointers, which the
// workspace's `unsafe_code = "deny"` would otherwise block, so re-allow it
// here — scoped to this crate rather than dropping every other workspace lint.
#![allow(unsafe_code)]
// MIR → Cranelift IR lowering performs deliberate integer width conversions
// (usize↔i64, i64→u8/u32, bool→i64) at nearly every translation site. The
// `cast_*` pedantic lints fire en masse here and are reviewed per-site as part
// of codegen, so they're allowed crate-wide rather than annotated individually.
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss,
    clippy::cast_lossless
)]
// Codegen-idiomatic style lints — short operand names, exhaustive IR-variant
// matches, and per-op clones are deliberate here. Allowed crate-wide in the
// same spirit the workspace allows other judgment-heavy pedantic lints.
#![allow(
    clippy::many_single_char_names,
    clippy::match_wildcard_for_single_variants,
    clippy::assigning_clones,
    clippy::unused_self,
    clippy::single_match,
    clippy::single_match_else,
    clippy::map_unwrap_or,
    clippy::default_trait_access
)]

pub mod aot;
pub mod aot_meta;
pub mod debug;
pub mod game;
mod options;
mod runtime;
mod translate;

pub use debug::{frame_name, render_frame_vars, set_debug_hook, DebugHook};
pub use game::{set_game_runtime, GameRuntime};
pub use options::{NativeEmit, NativeError, NativeOptions, OptLevel};
pub use runtime::ops_used;

use std::collections::HashMap;

use cranelift::codegen::{self, settings, Context};
use cranelift::prelude::{types, AbiParam, Configurable};
use cranelift_frontend::FunctionBuilderContext;
use cranelift_module::{default_libcall_names, Linkage, Module};

use leek_hir::HirFile;
use leek_runtime::Value;

use translate::{Lang, ValTy};

/// What a native compile produced. Variant depends on [`NativeOptions::emit`].
#[derive(Debug)]
pub enum NativeArtifact {
    /// JIT result (the program's return value). [`NativeEmit::Jit`].
    Value(Value),
    /// Text dump — Cranelift IR or disassembly.
    Text(String),
    /// An object file was written to disk. [`NativeEmit::Object`].
    Object,
}

thread_local! {
    /// (JIT-compile, execute) split of the most recent JIT `run`/`compile`
    /// on this thread. Set at the end of the [`NativeEmit::Jit`] path.
    static LAST_JIT_SPLIT: std::cell::Cell<Option<(std::time::Duration, std::time::Duration)>> =
        const { std::cell::Cell::new(None) };
}

/// The (JIT-compile, execute) durations of the most recent successful JIT run
/// on this thread, or `None` if none has completed. The compile portion covers
/// Cranelift codegen + module finalize + runtime wiring; the execute portion is
/// the JIT'd `main` call. Lets the benchmark harness report the two separately
/// instead of one combined figure.
#[must_use]
pub fn last_jit_split() -> Option<(std::time::Duration, std::time::Duration)> {
    LAST_JIT_SPLIT.with(std::cell::Cell::get)
}

/// Convenience: JIT-compile `hir` with `opts` and run it, returning the
/// program's value. Forces [`NativeEmit::Jit`].
pub fn run(hir: &HirFile, opts: &NativeOptions) -> Result<Value, NativeError> {
    let mut opts = opts.clone();
    opts.emit = NativeEmit::Jit;
    match compile(hir, &opts)? {
        NativeArtifact::Value(v) => Ok(v),
        _ => unreachable!("Jit emit yields a Value"),
    }
}

/// Compile `hir` according to `opts.emit`.
pub fn compile(hir: &HirFile, opts: &NativeOptions) -> Result<NativeArtifact, NativeError> {
    // Emit op-budget back-edge checks (so an unbounded loop stops) only when a
    // finite budget is set — keeps zero-overhead the common unlimited runs.
    runtime::set_enforce_budget(opts.op_limit != u64::MAX);
    let (mut program, errs) = leek_mir::lower_file(hir);
    if let Some(first) = errs.first() {
        return Err(NativeError::Compile(format!(
            "MIR lowering failed: {}",
            first.message
        )));
    }
    let main_idx = program
        .functions
        .iter()
        .position(|f| f.kind == leek_mir::ir::FunctionKind::Main)
        .ok_or_else(|| NativeError::Compile("no main function".into()))?;

    let lang = Lang {
        version: opts.version,
        strict: opts.strict,
    };
    // Append a constructor thunk for each class used as a *value* (so a class
    // reference can be invoked to construct), recording class → thunk index.
    // Done before signature/reachability analysis so the thunks participate.
    let class_thunks = translate::append_ctor_thunks(&mut program, opts.version);
    // Pin provably-integer untyped params to `integer` so they compile unboxed.
    translate::specialize_param_types(&mut program, lang);
    let main = &program.functions[main_idx];
    // Program-wide function result kinds (drives cross-call typing) and
    // declared scalar kinds of typed globals (drives write coercion).
    let fn_rets = translate::compute_fn_rets(&program, lang);
    let global_tys = translate::global_scalar_tys(&program);
    let native_directives = collect_native_directives(hir);

    match &opts.emit {
        NativeEmit::Clif => {
            // No ISA / module — render just `main` (calls to user
            // functions aren't lowered in this inspection mode).
            let sig = translate::function_sig(main, lang, &fn_rets, &program)?;
            let mut func = make_func(sig.ret);
            let mut fb_ctx = FunctionBuilderContext::new();
            translate::translate_function(
                &mut func, &mut fb_ctx, main, &sig, lang, &fn_rets, None, &HashMap::new(),
                &HashMap::new(), &program, &global_tys, &native_directives,
                &std::collections::HashSet::new(), false, false, false,
            )?;
            Ok(NativeArtifact::Text(func.display().to_string()))
        }
        NativeEmit::Disasm => {
            let isa = build_isa(opts)?;
            let sig = translate::function_sig(main, lang, &fn_rets, &program)?;
            let mut ctx = Context::new();
            ctx.func = make_func(sig.ret);
            ctx.set_disasm(true);
            let mut fb_ctx = FunctionBuilderContext::new();
            translate::translate_function(
                &mut ctx.func, &mut fb_ctx, main, &sig, lang, &fn_rets, None, &HashMap::new(),
                &HashMap::new(), &program, &global_tys, &native_directives,
                &std::collections::HashSet::new(), false, false, false,
            )?;
            ctx.compile(isa.as_ref(), &mut Default::default())
                .map_err(|e| NativeError::Compile(format!("{e:?}")))?;
            let text = ctx
                .compiled_code()
                .and_then(|c| c.vcode.clone())
                .unwrap_or_else(|| "<no disassembly>".into());
            Ok(NativeArtifact::Text(text))
        }
        NativeEmit::Object(path) => {
            let isa = build_isa(opts)?;
            let ob = cranelift_object::ObjectBuilder::new(
                isa,
                "leek",
                default_libcall_names(),
            )
            .map_err(|e| NativeError::Compile(e.to_string()))?;
            let mut module = cranelift_object::ObjectModule::new(ob);
            // Object emit isn't executed, so lambda addresses / the method
            // table aren't needed.
            let _ = define_program(&mut module, &program, main, lang, &fn_rets, &global_tys, &native_directives, &class_thunks, opts.debug_hooks, opts.link_game, false)?;
            let bytes = module
                .finish()
                .emit()
                .map_err(|e| NativeError::Compile(e.to_string()))?;
            std::fs::write(path, bytes).map_err(|e| NativeError::Compile(e.to_string()))?;
            Ok(NativeArtifact::Object)
        }
        NativeEmit::Jit => {
            let t_compile = std::time::Instant::now();
            let isa = build_isa(opts)?;
            let mut jb = cranelift_jit::JITBuilder::with_isa(isa, default_libcall_names());
            // Register the shared runtime math builtins (and the `**`
            // integer-power helper) so `call`s to them resolve at finalize.
            for b in leek_runtime::math_builtins() {
                jb.symbol(b.symbol, b.addr);
            }
            let (ipow_sym, ipow_addr) = leek_runtime::ipow_addr();
            jb.symbol(ipow_sym, ipow_addr);
            // Composite-value runtime shims (arrays, box/unbox, …).
            for (sym, addr) in runtime::runtime_symbols() {
                jb.symbol(sym, addr);
            }
            let mut module = cranelift_jit::JITModule::new(jb);
            let (id, ret_ty, lambda_funcs, method_resolve, static_init, user_fn_idx, exact_arity, class_string_method) =
                define_program(&mut module, &program, main, lang, &fn_rets, &global_tys, &native_directives, &class_thunks, opts.debug_hooks, opts.link_game, false)?;
            module
                .finalize_definitions()
                .map_err(|e| NativeError::Compile(e.to_string()))?;
            // Publish each lambda / bound-method's finalized address (+ param
            // count) so `call_value` / indirect calls can invoke them.
            let lambda_addrs: HashMap<usize, (*const u8, usize)> = lambda_funcs
                .iter()
                .map(|(&idx, &(fid, nparams))| {
                    (idx, (module.get_finalized_function(fid), nparams))
                })
                .collect();
            runtime::set_lambda_fns(lambda_addrs);
            // Per-lambda user-param `@`-by-ref masks (captures excluded) for the
            // higher-order builtins. A lambda's MIR params are
            // `[captures…, user-params…]`; the capture count comes from the
            // `MakeLambda` that builds it.
            {
                use leek_mir::ir::{Rvalue, Statement};
                let mut ncaptures: HashMap<usize, usize> = HashMap::new();
                for f in &program.functions {
                    for b in &f.blocks {
                        for s in &b.statements {
                            if let Statement::Assign(
                                _,
                                Rvalue::MakeLambda {
                                    function_idx,
                                    captures,
                                },
                            ) = s
                            {
                                ncaptures.insert(*function_idx, captures.len());
                            }
                        }
                    }
                }
                let masks: HashMap<usize, Vec<bool>> = lambda_funcs
                    .keys()
                    .map(|&idx| {
                        let f = &program.functions[idx];
                        let nc = ncaptures.get(&idx).copied().unwrap_or(0);
                        let mask = f
                            .params
                            .iter()
                            .skip(nc)
                            .map(|p| f.locals[p.0 as usize].is_by_ref)
                            .collect();
                        (idx, mask)
                    })
                    .collect();
                runtime::set_lambda_byref(masks);
            }
            runtime::set_method_resolve(method_resolve);
            runtime::set_static_init(static_init);
            runtime::set_user_fn_idx(user_fn_idx);
            runtime::set_user_fn_exact_arity(exact_arity);
            // Class hierarchy for runtime `.super` (`x.class.super`): each
            // class → its explicit parent, or `None` for the implicit `Value`.
            let class_parent: HashMap<u32, Option<(u32, std::string::String)>> = program
                .classes
                .iter()
                .map(|c| {
                    let parent = c
                        .parent_def
                        .and_then(|pd| program.class(pd).map(|pc| (pd.0, pc.name.clone())));
                    (c.def_id.0, parent)
                })
                .collect();
            runtime::set_class_parent(class_parent);
            // Per-class constructor thunks: class `DefId` → thunk function idx
            // (its finalized address is already in `LAMBDA_FNS`), so a
            // `Value::ClassRef` invoked as a value constructs.
            runtime::set_class_ctor_thunk(class_thunks);
            // Per-class `string()` display overrides (applied to the top-level
            // result below). Their addresses are already in `LAMBDA_FNS`.
            runtime::set_class_string_method(class_string_method);
            // Per-class reflection name tables for runtime `x.class.fields` etc.
            runtime::set_class_reflect(translate::reflect_name_tables(&program));
            let ptr = module.get_finalized_function(id);
            // Reset file-level globals so this run can't observe a previous
            // run's values (the global store is a process-wide thread-local).
            runtime::clear_globals();
            // Arm the runtime-fault channel: shims (e.g. a v4-strict OOB array
            // write) record a fault here instead of unwinding; we surface it
            // after `main` returns. `set_strict` lets those rules match the
            // interpreter's strict-gated behavior.
            runtime::reset_runtime_error();
            runtime::set_strict(opts.strict);
            // Set the value-display version BEFORE running, so version-specific
            // string conversions during execution (e.g. a real's `.` vs `,`
            // decimal separator in v1) and the caller's final `value.to_string()`
            // both format correctly. (Was previously an interpreter side effect.)
            leek_runtime::DISPLAY_VERSION.with(|c| c.set(opts.version));
            // Arm the op counter + budget for this run. The JIT'd body charges
            // ops at the same MIR sites the interpreter does (so counts match);
            // `ops_used()` reads the total after `main` returns.
            runtime::reset_ops(opts.op_limit);
            // Everything above is codegen + runtime wiring; the program itself
            // hasn't run yet. Split the timing here so the benchmark can report
            // JIT compilation separately from execution.
            let compile_dur = t_compile.elapsed();
            let t_exec = std::time::Instant::now();
            // SAFETY: `leek_main` was declared with the matching ABI
            // (`() -> i64` or `() -> f64`) and the module finalized; the
            // pointer is a valid host function.
            let value = match ret_ty {
                ValTy::Real => {
                    let f = unsafe {
                        std::mem::transmute::<*const u8, extern "C" fn() -> f64>(ptr)
                    };
                    Value::Real(f())
                }
                ValTy::Bool => {
                    let f = unsafe {
                        std::mem::transmute::<*const u8, extern "C" fn() -> i64>(ptr)
                    };
                    Value::Bool(f() != 0)
                }
                ValTy::Int => {
                    let f = unsafe {
                        std::mem::transmute::<*const u8, extern "C" fn() -> i64>(ptr)
                    };
                    Value::Int(f())
                }
                // A composite / boxed result: the function returns a
                // handle; recover the owned `Value` (freeing the box). A
                // top-level instance whose class declares `string()` is routed
                // through it (matching the interpreter's display).
                ValTy::Ref => {
                    let f = unsafe {
                        std::mem::transmute::<*const u8, extern "C" fn() -> *mut Value>(ptr)
                    };
                    // Clone the result out of its handle (don't free the box —
                    // `free_run_boxes` below reclaims every handle at once). The
                    // clone keeps the result's `Rc`-backed data alive past the
                    // sweep.
                    runtime::invoke_top_level_string(unsafe { runtime::read_handle(f()) })
                }
            };
            LAST_JIT_SPLIT.with(|c| c.set(Some((compile_dur, t_exec.elapsed()))));
            // The program has finished running and its result is now an owned,
            // JIT-independent `Value` (any class `string()` display override
            // already ran during extraction above; scalars and composites live
            // on the normal heap). Reclaim the module's executable + data
            // memory now — a plain `JITModule` drop LEAKS these mmap'd regions,
            // which accumulates across runs and OOMs a process that JIT-compiles
            // many programs (e.g. the upstream-suite regression test compiles
            // 10k+ cases in one process).
            //
            // SAFETY: `free_memory` requires that no function from this module
            // is executing or called afterward. `main` has returned, no JIT
            // function is invoked past this point (a returned function value
            // stringifies without being called), and the per-run runtime tables
            // of finalized addresses (`LAMBDA_FNS`, …) are overwritten at the
            // start of the next run before they could be consulted again.
            unsafe { module.free_memory() };
            // Reclaim every boxed `Value` handle this run allocated. The result
            // was cloned out above (`read_handle`), so it and its reachable data
            // survive; all intermediate boxes — including the ones the global
            // store held — are freed here instead of leaking until process exit.
            runtime::free_run_boxes();
            // A runtime fault recorded by a shim during the run (e.g. a
            // v4-strict out-of-bounds array write) takes precedence over the
            // computed value: the program errored.
            if let Some(code) = runtime::take_runtime_error() {
                drop(value);
                return Err(NativeError::Runtime(code));
            }
            Ok(NativeArtifact::Value(value))
        }
    }
}

/// Declare and define `main` plus every user function reachable from it
/// in `module`, returning `main`'s `FuncId` and result kind. Bails (so the
/// whole program skips) if any reachable function falls outside the scalar
/// subset.
/// Map each *bodiless* function's HIR `DefId` to the runtime builtin to dispatch
/// when it's called (honoring a `@native-backend:` directive's leading
/// identifier, else the function's own name).
fn collect_native_directives(hir: &HirFile) -> HashMap<leek_hir::DefId, String> {
    hir.defs
        .iter()
        .enumerate()
        .filter_map(|(i, d)| match d {
            leek_hir::Def::Function(f) if f.body.is_none() => {
                let name = f
                    .backend_directives
                    .iter()
                    .find(|(b, _)| b == "native")
                    .map(|(_, body)| body.split(['(', ' ']).next().unwrap_or(body).trim().to_string())
                    .unwrap_or_else(|| f.name.clone());
                Some((leek_hir::DefId(i as u32), name))
            }
            _ => None,
        })
        .collect()
}

/// Emit an AOT object (with externally linkable `leek_uniform_{idx}` symbols)
/// and return the dispatch-table [`AotMeta`](crate::aot_meta::AotMeta) the
/// runtime must reinstall at startup. Used only by the AOT path.
pub fn compile_object_with_meta(
    hir: &HirFile,
    opts: &NativeOptions,
    obj_path: &std::path::Path,
) -> Result<aot_meta::AotMeta, NativeError> {
    runtime::set_enforce_budget(opts.op_limit != u64::MAX);
    let (mut program, errs) = leek_mir::lower_file(hir);
    if let Some(first) = errs.first() {
        return Err(NativeError::Compile(format!(
            "MIR lowering failed: {}",
            first.message
        )));
    }
    let main_idx = program
        .functions
        .iter()
        .position(|f| f.kind == leek_mir::ir::FunctionKind::Main)
        .ok_or_else(|| NativeError::Compile("no main function".into()))?;
    let lang = Lang {
        version: opts.version,
        strict: opts.strict,
    };
    let class_thunks = translate::append_ctor_thunks(&mut program, opts.version);
    translate::specialize_param_types(&mut program, lang);
    let fn_rets = translate::compute_fn_rets(&program, lang);
    let global_tys = translate::global_scalar_tys(&program);
    let native_directives = collect_native_directives(hir);
    let main = &program.functions[main_idx];

    let isa = build_isa(opts)?;
    let ob = cranelift_object::ObjectBuilder::new(isa, "leek", default_libcall_names())
        .map_err(|e| NativeError::Compile(e.to_string()))?;
    let mut module = cranelift_object::ObjectModule::new(ob);
    let (_main_id, _ret, lambda_funcs, method_resolve, static_init, user_fn_idx, exact_arity, class_string_method) =
        define_program(&mut module, &program, main, lang, &fn_rets, &global_tys, &native_directives, &class_thunks, opts.debug_hooks, opts.link_game, /* external_uniform */ true)?;
    let bytes = module
        .finish()
        .emit()
        .map_err(|e| NativeError::Compile(e.to_string()))?;
    std::fs::write(obj_path, bytes).map_err(|e| NativeError::Compile(e.to_string()))?;

    Ok(aot_meta::AotMeta::build(
        &program,
        &lambda_funcs,
        method_resolve,
        static_init,
        user_fn_idx,
        exact_arity,
        class_string_method,
        &class_thunks,
    ))
}

/// Symbol name + linkage for a uniform-ABI function. AOT uses one externally
/// visible `leek_uniform_{idx}` scheme so a generated C harness can take the
/// address; the JIT keeps the descriptive local name (linkage is irrelevant
/// there, since addresses come from the `FuncId`).
fn uniform_symbol(external: bool, jit_prefix: &str, idx: usize) -> (String, Linkage) {
    if external {
        (format!("leek_uniform_{idx}"), Linkage::Export)
    } else {
        (format!("{jit_prefix}_{idx}"), Linkage::Local)
    }
}

fn define_program<M: Module>(
    module: &mut M,
    program: &leek_mir::ir::MirProgram,
    main: &leek_mir::ir::MirFunction,
    lang: Lang,
    fn_rets: &translate::FnRets,
    global_tys: &HashMap<String, ValTy>,
    native_directives: &HashMap<leek_hir::DefId, String>,
    // Class `DefId` raw → constructor-thunk `program.functions` index, for
    // classes used as values. The thunks are compiled (uniform ABI) and their
    // construct callees made reachable here.
    class_thunks: &HashMap<u32, usize>,
    // Emit per-statement debug safepoints in every compiled body.
    debug_hooks: bool,
    // Route unknown builtins to the host game runtime (see `crate::game`).
    link_game: bool,
    // AOT: give every uniform-ABI function (lambda / thunk / value-method) a
    // single externally-linkable `leek_uniform_{idx}` symbol, so a generated C
    // harness can take its address to repopulate the runtime dispatch tables at
    // startup. The JIT passes `false` (addresses come from `FuncId`, names /
    // linkage are irrelevant).
    external_uniform: bool,
) -> Result<
    (
        cranelift_module::FuncId,
        ValTy,
        HashMap<usize, (cranelift_module::FuncId, usize)>,
        HashMap<u32, HashMap<String, usize>>,
        HashMap<(u32, String), usize>,
        HashMap<u32, usize>,
        std::collections::HashSet<u32>,
        // class `DefId` raw → `string()` display method idx (in `LAMBDA_FNS`).
        HashMap<u32, usize>,
    ),
    NativeError,
> {
    use leek_hir::DefId;
    use leek_mir::ir::FunctionKind;

    let by_id: HashMap<DefId, &leek_mir::ir::MirFunction> = program
        .functions
        .iter()
        .filter_map(|f| f.def_id.map(|id| (id, f)))
        .collect();
    let mut reachable = translate::reachable_user_fns(program, main);
    // Constructor thunks (for classes used as values): their `new C(...)`
    // construct edges (field-inits + selected constructor, transitively) must
    // be compiled too. The thunk class set gates the use-as-value sites.
    let ctor_thunk_classes: std::collections::HashSet<u32> = class_thunks.keys().copied().collect();
    let thunk_idxs: Vec<usize> = class_thunks.values().copied().collect();
    translate::extend_reachable_for_thunks(program, &thunk_idxs, &mut reachable);

    // `string()` display overrides: a constructed class whose instance can be
    // the top-level result needs its `string()` method force-compiled +
    // registered so the post-run transform can invoke it. Pull each one (and
    // its callees) into reachability before declaring signatures.
    let main_pos = program
        .functions
        .iter()
        .position(|f| f.kind == FunctionKind::Main)
        .unwrap_or(0);
    let reachable_indices_now: Vec<usize> = std::iter::once(main_pos)
        .chain(
            reachable
                .defs
                .iter()
                .filter_map(|d| program.functions.iter().position(|g| g.def_id == Some(*d))),
        )
        .chain(reachable.field_inits.iter().copied())
        .collect();
    let string_display: Vec<(u32, usize)> =
        translate::string_display_classes(program, &reachable_indices_now, class_thunks);
    let string_method_idxs: Vec<usize> = string_display.iter().map(|(_, i)| *i).collect();
    translate::extend_reachable_with(program, &string_method_idxs, &mut reachable);

    // Phase 1: compute each reachable function's signature and declare its
    // FuncId, so (mutually) recursive calls can resolve. `defs` are keyed
    // by `DefId` (free functions, methods, constructors); `field_inits`
    // (no `DefId`) are keyed by their `program.functions` index.
    let mut callees: HashMap<DefId, (cranelift_module::FuncId, translate::FnSig)> = HashMap::new();
    for (n, def_id) in reachable.defs.iter().enumerate() {
        let f = by_id
            .get(def_id)
            .ok_or_else(|| NativeError::Compile("reachable fn missing".into()))?;
        let sig = translate::function_sig(f, lang, fn_rets, program)?;
        let mut clsig = module.make_signature();
        for p in &sig.params {
            clsig.params.push(AbiParam::new(p.cl_type()));
        }
        // Hidden trailing `argc` for functions with non-constant defaults.
        if sig.has_defaults {
            clsig.params.push(AbiParam::new(types::I64));
        }
        clsig.returns.push(AbiParam::new(sig.ret.cl_type()));
        let id = module
            .declare_function(&format!("leek_fn_{n}"), Linkage::Local, &clsig)
            .map_err(|e| NativeError::Compile(e.to_string()))?;
        callees.insert(*def_id, (id, sig));
    }
    // Reachable `def_id`-less functions split into lambda bodies and field
    // initializers. A lambda is identified by being a `MakeLambda` target
    // (NOT by lacking an `owning_class` — a lambda inside a method inherits
    // that method's class). Field-inits compile with the normal ABI; lambdas
    // with the uniform `(argv, argc)` ABI so they can be invoked dynamically
    // via `call_value`.
    let lambda_set = translate::lambda_body_idxs(program);
    let (lambda_idxs, field_init_idxs): (Vec<usize>, Vec<usize>) = reachable
        .field_inits
        .iter()
        .partition(|&&idx| lambda_set.contains(&idx));

    let mut field_init_callees: HashMap<usize, (cranelift_module::FuncId, translate::FnSig)> =
        HashMap::new();
    for &idx in &field_init_idxs {
        let f = &program.functions[idx];
        let sig = translate::function_sig(f, lang, fn_rets, program)?;
        let mut clsig = module.make_signature();
        for p in &sig.params {
            clsig.params.push(AbiParam::new(p.cl_type()));
        }
        clsig.returns.push(AbiParam::new(sig.ret.cl_type()));
        let id = module
            .declare_function(&format!("leek_finit_{idx}"), Linkage::Local, &clsig)
            .map_err(|e| NativeError::Compile(e.to_string()))?;
        field_init_callees.insert(idx, (id, sig));
    }

    // Lambda bodies: uniform ABI `fn(argv: i64, argc: i64) -> i64`. The map
    // records (FuncId, total param count incl. captures) so the JIT entry
    // can publish addresses + `callback_arity` can compute user-arity.
    let i64t = types::I64;
    let mut lambda_funcs: HashMap<usize, (cranelift_module::FuncId, usize)> = HashMap::new();
    for &idx in &lambda_idxs {
        let f = &program.functions[idx];
        let mut clsig = module.make_signature();
        clsig.params.push(AbiParam::new(i64t));
        clsig.params.push(AbiParam::new(i64t));
        clsig.returns.push(AbiParam::new(i64t));
        let (sym, link) = uniform_symbol(external_uniform, "leek_lambda", idx);
        let id = module
            .declare_function(&sym, link, &clsig)
            .map_err(|e| NativeError::Compile(e.to_string()))?;
        lambda_funcs.insert(idx, (id, f.params.len()));
    }
    // Constructor thunks compile with the same uniform `(argv, argc)` ABI so a
    // class-ref value can be invoked dynamically (their bodies are `new C(...)`).
    for &idx in &thunk_idxs {
        let f = &program.functions[idx];
        let mut clsig = module.make_signature();
        clsig.params.push(AbiParam::new(i64t));
        clsig.params.push(AbiParam::new(i64t));
        clsig.returns.push(AbiParam::new(i64t));
        let (sym, link) = uniform_symbol(external_uniform, "leek_ctorthunk", idx);
        let id = module
            .declare_function(&sym, link, &clsig)
            .map_err(|e| NativeError::Compile(e.to_string()))?;
        lambda_funcs.insert(idx, (id, f.params.len()));
    }

    // Methods read as values (`obj['m']`) need a uniform-ABI copy — distinct
    // from their typed `callees` entry — registered like a lambda so a
    // `BoundMethod` can invoke them. `method_resolve` maps `(class, method)`
    // to the function index for the runtime index shim.
    let main_idx = program
        .functions
        .iter()
        .position(|f| f.kind == FunctionKind::Main)
        .unwrap_or(0);
    let mut reachable_indices: Vec<usize> = vec![main_idx];
    for d in &reachable.defs {
        if let Some(i) = program.functions.iter().position(|g| g.def_id == Some(*d)) {
            reachable_indices.push(i);
        }
    }
    reachable_indices.extend(&reachable.field_inits);
    // `@`-by-ref params and closures that mutate captured variables need
    // `Value::Cell` sharing the handle model can't express — skip the whole
    // program rather than miscompile (read-only captures are unaffected).
    if translate::needs_cell_semantics(program, &reachable_indices, &lambda_set, lang.version) {
        return Err(NativeError::Unsupported(
            "closure shared-mutation / @-by-reference parameter".into(),
        ));
    }
    let (method_resolve, mut value_methods) =
        translate::method_value_info(program, &reachable_indices);
    // Static-field initialisers: a uniform-ABI nullary copy of each accessed
    // field's init function, registered (like a lambda) so `leek_static_get`
    // can run it lazily on first read.
    let static_init = translate::static_field_info(program, &reachable_indices);
    for &idx in static_init.values() {
        value_methods.insert(idx);
    }
    // Named-function references (`var f = foo`) and static methods read as
    // values (`var f = C.staticMethod`): uniform-compile each so a
    // `Function::User` value can be invoked through `dispatch_call_value`.
    let mut user_fn_idx = translate::function_ref_info(program, &reachable_indices);
    user_fn_idx.extend(translate::static_method_value_info(program, &reachable_indices));
    for &idx in user_fn_idx.values() {
        value_methods.insert(idx);
    }
    // `string()` display overrides compile (uniform ABI) like value methods so
    // the post-run transform can invoke them on the top-level result.
    let class_string_method: HashMap<u32, usize> = string_display.iter().copied().collect();
    for &idx in &string_method_idxs {
        value_methods.insert(idx);
    }
    let mut method_funcs: HashMap<usize, (cranelift_module::FuncId, usize)> = HashMap::new();
    for &idx in &value_methods {
        let f = &program.functions[idx];
        let mut clsig = module.make_signature();
        clsig.params.push(AbiParam::new(i64t));
        clsig.params.push(AbiParam::new(i64t));
        clsig.returns.push(AbiParam::new(i64t));
        let (sym, link) = uniform_symbol(external_uniform, "leek_method", idx);
        let id = module
            .declare_function(&sym, link, &clsig)
            .map_err(|e| NativeError::Compile(e.to_string()))?;
        method_funcs.insert(idx, (id, f.params.len()));
    }

    // main's signature + FuncId.
    let main_sig = translate::function_sig(main, lang, fn_rets, program)?;
    let mut main_clsig = module.make_signature();
    main_clsig
        .returns
        .push(AbiParam::new(main_sig.ret.cl_type()));
    let main_id = module
        .declare_function("leek_main", Linkage::Export, &main_clsig)
        .map_err(|e| NativeError::Compile(e.to_string()))?;

    // Phase 2: translate + define each function body.
    let define_one = |module: &mut M,
                      f: &leek_mir::ir::MirFunction,
                      fid,
                      sig: &translate::FnSig,
                      uniform: bool|
     -> Result<(), NativeError> {
        let mut clsig = module.make_signature();
        if uniform {
            clsig.params.push(AbiParam::new(i64t));
            clsig.params.push(AbiParam::new(i64t));
            clsig.returns.push(AbiParam::new(i64t));
        } else {
            for p in &sig.params {
                clsig.params.push(AbiParam::new(p.cl_type()));
            }
            if sig.has_defaults {
                clsig.params.push(AbiParam::new(i64t));
            }
            clsig.returns.push(AbiParam::new(sig.ret.cl_type()));
        }
        let mut ctx = Context::new();
        ctx.func.signature = clsig;
        let mut fb_ctx = FunctionBuilderContext::new();
        translate::translate_function(
            &mut ctx.func,
            &mut fb_ctx,
            f,
            sig,
            lang,
            fn_rets,
            Some(&mut *module),
            &callees,
            &field_init_callees,
            program,
            global_tys,
            native_directives,
            &ctor_thunk_classes,
            uniform,
            debug_hooks,
            link_game,
        )?;
        module
            .define_function(fid, &mut ctx)
            .map_err(|e| NativeError::Compile(e.to_string()))
    };

    let dummy_sig = translate::FnSig {
        params: vec![],
        ret: ValTy::Ref,
        has_defaults: false,
    };
    for def_id in &reachable.defs {
        let f = by_id[def_id];
        let (fid, sig) = callees[def_id].clone();
        define_one(module, f, fid, &sig, false)?;
    }
    for &idx in &field_init_idxs {
        let f = &program.functions[idx];
        let (fid, sig) = field_init_callees[&idx].clone();
        define_one(module, f, fid, &sig, false)?;
    }
    for &idx in &lambda_idxs {
        let f = &program.functions[idx];
        let (fid, _) = lambda_funcs[&idx];
        define_one(module, f, fid, &dummy_sig, true)?;
    }
    for &idx in &thunk_idxs {
        let f = &program.functions[idx];
        let (fid, _) = lambda_funcs[&idx];
        define_one(module, f, fid, &dummy_sig, true)?;
    }
    for &idx in &value_methods {
        let f = &program.functions[idx];
        let (fid, _) = method_funcs[&idx];
        define_one(module, f, fid, &dummy_sig, true)?;
    }
    define_one(module, main, main_id, &main_sig, false)?;

    // Bound-method + static-init bodies join the lambda table — all are
    // uniform-ABI functions invoked dynamically (via `dispatch_call_value`
    // or `leek_static_get`).
    lambda_funcs.extend(method_funcs);
    // User-fn values whose target is a method (has an owning class) require
    // exact arity when invoked indirectly (see `dispatch_call_value`).
    let exact_arity: std::collections::HashSet<u32> = user_fn_idx
        .iter()
        .filter(|(_, idx)| program.functions[**idx].owning_class.is_some())
        .map(|(def, _)| *def)
        .collect();
    Ok((
        main_id,
        main_sig.ret,
        lambda_funcs,
        method_resolve,
        static_init,
        user_fn_idx,
        exact_arity,
        class_string_method,
    ))
}

fn make_func(ret_ty: ValTy) -> codegen::ir::Function {
    let mut sig = codegen::ir::Signature::new(codegen::isa::CallConv::SystemV);
    sig.returns.push(AbiParam::new(ret_ty.cl_type()));
    let mut func = codegen::ir::Function::new();
    func.signature = sig;
    func
}

fn build_isa(opts: &NativeOptions) -> Result<codegen::isa::OwnedTargetIsa, NativeError> {
    let mut fb = settings::builder();
    let set = |fb: &mut settings::Builder, k: &str, v: &str| -> Result<(), NativeError> {
        fb.set(k, v)
            .map_err(|e| NativeError::Compile(format!("flag {k}={v}: {e}")))
    };
    set(&mut fb, "opt_level", opts.opt_level.cranelift_str())?;
    set(&mut fb, "enable_verifier", bool_str(opts.enable_verifier))?;
    set(
        &mut fb,
        "preserve_frame_pointers",
        bool_str(opts.preserve_frame_pointers),
    )?;
    let flags = settings::Flags::new(fb);
    let builder =
        cranelift_native::builder().map_err(|e| NativeError::Compile(e.to_string()))?;
    builder
        .finish(flags)
        .map_err(|e| NativeError::Compile(e.to_string()))
}

fn bool_str(b: bool) -> &'static str {
    if b {
        "true"
    } else {
        "false"
    }
}
