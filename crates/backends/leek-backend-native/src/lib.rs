//! Native backend: MIR → Cranelift IR → machine code.
//!
//! The JIT is the execution path for `miku run` / `miku test` and
//! `leekc --emit run`, and it covers most of the language: arithmetic and
//! control flow, globals, strings, arrays, maps, sets and intervals,
//! classes and methods, lambdas and first-class functions. What it still
//! cannot translate surfaces as [`NativeErrorKind::Unsupported`] —
//! carrying the construct's name and span — rather than being dropped,
//! so the corpus runner can skip and account for exactly those cases.
//!
//! # The value model
//!
//! Two representations, chosen per local:
//!
//! * **Unboxed** — a local whose type is statically known to be
//!   `integer`, `real` or `boolean` lives in a Cranelift register or
//!   stack slot as a machine `i64`/`f64`. No allocation, no shim call:
//!   `n - 1` is an `isub`.
//! * **A handle** — everything else is a `*mut leek_runtime::Value`
//!   carried as an `i64`. `ValTy::Ref` is the universal fallback: any
//!   value can be a handle, so translation coerces to one wherever a
//!   static kind runs out.
//!
//! Handles are bump-allocated from a per-run arena and reclaimed in one
//! sweep at run end; the `runtime` module owns the allocation and the
//! safety contract that governs passing them across the ABI.
//!
//! # The runtime ABI
//!
//! Composite operations are not open-coded. The generated code calls
//! the `leek_*` C-ABI shims of the `runtime` module, which unbox their
//! arguments and delegate to `leek-runtime` — the same value semantics
//! the Java backend is checked against — so there is one implementation
//! of what `+` means, not one per backend.
//!
//! Whatever cannot be resolved at compile time is resolved through
//! per-run tables the compiler installs before `main` runs: globals,
//! class parents and constructors, method resolution, static fields, the
//! seeded RNG. Dynamic dispatch (`expr.method()` on an unknown class,
//! calling a value) is a lookup in those.
//!
//! # Operation charging
//!
//! Leekscript programs run against an operation budget. The generated
//! code charges ops at the same MIR sites the reference implementation
//! does, so counts match and are comparable; loops poll the budget at
//! their back-edges because JIT'd code cannot unwind. See
//! [`NativeOptions::op_limit`] and [`ops_used`].
//!
//! # Emitting
//!
//! Compilation options live in [`NativeOptions`]: optimization level
//! (debug vs release), the IR verifier, frame-pointer preservation,
//! DWARF debug info, and the emit mode — run via JIT, dump the
//! Cranelift IR / disassembly (for inspecting generated code), or
//! write a relocatable object file.
//!
//! [`aot`] links that object into a standalone executable. It supports
//! less than the JIT does, and for one reason: the JIT may bake a
//! pointer into compiler-process memory, which is valid in-process and
//! dangling in a separate program. See [`aot`] for what that rules out.
//!
//! # A word this crate's comments use
//!
//! **upstream** is the official Java LeekScript implementation. It is the
//! oracle: where a comment says the generated code matches upstream, that
//! is a claim the corpus suite checks against upstream's own expectations.
//!
//! Comments here used to say "the interpreter" instead, meaning
//! `leek-backend-interp` — a Rust interpreter that mirrored upstream and
//! was removed in fd2d97e once the JIT became the execution path. The
//! value logic it shared now lives in `leek-runtime`, which both this
//! backend and the Java backend call, so a comment naming `leek-runtime`
//! is pointing at code in this workspace and one naming upstream is
//! pointing at the reference implementation.

// Printing is an API decision in a library, not a convenience: a crate that
// writes to the terminal behind its caller's back is unusable from a language
// server or a test harness. Every print below is either the tool's *output*
// or a justified exception, and says which.
#![warn(clippy::print_stdout, clippy::print_stderr)]
// A doc link to an item that does not exist is worse than no link: it reads as
// a pointer to somewhere the reader can go and look. This crate accumulated
// several — to `BOXES` and `ARENA`, which never existed, and to
// `NativeError::Unsupported`, which is a variant of `NativeErrorKind`. Note
// that `warn` is as far as this goes on its own: nothing in CI builds docs, so
// these surface to whoever runs `cargo doc`, not to the gate.
#![warn(rustdoc::broken_intra_doc_links)]
// This crate inherits the workspace lint table (see Cargo.toml). Its Cranelift
// JIT path transmutes and calls finalized function pointers, which the
// workspace's `unsafe_code = "deny"` would otherwise block, so re-allow it
// here — scoped to this crate rather than dropping every other workspace lint.
#![allow(unsafe_code)]
// The flip side of that allowance: where this crate does use `unsafe`, every
// block must state its contract, and one block may assert only one thing. The
// FFI surface predates this rule, so the modules that have not been converted
// yet carry a module-level `allow` with `reason = "FFI conversion pending —
// see #114"`; `grep -rn "FFI conversion pending"` is the remaining-work list.
#![deny(
    clippy::undocumented_unsafe_blocks,
    clippy::multiple_unsafe_ops_per_block
)]
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
pub mod ids;
mod options;
mod runtime;
mod translate;

pub use debug::{
    DebugHook, DebugValue, clear_debug_hook, frame_name, read_frame_vars, render_frame_vars,
    set_debug_hook,
};
pub use game::{GameRuntime, set_game_runtime};
pub use options::{
    CodegenKey, DEFAULT_OP_BUDGET, NativeEmit, NativeError, NativeErrorKind, NativeOptions,
    OptLevel,
};
pub use runtime::ops_used;

use std::collections::HashMap;

use cranelift::codegen::{self, Context, settings};
use cranelift::prelude::{AbiParam, Configurable, types};
use cranelift_frontend::FunctionBuilderContext;
use cranelift_module::{Linkage, Module, default_libcall_names};

use leek_diagnostics::Diagnostic;
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

/// Convenience: JIT-compile `hir` with `opts` and run it once, returning the
/// program's value. Always JITs, whatever `opts.emit` says.
///
/// The module is built and thrown away around the single run. A caller that
/// runs the same program repeatedly — a fight turn loop — should keep a
/// [`CompiledProgram`] from [`compile_program`] instead and call
/// [`CompiledProgram::run`] per run.
///
/// # Errors
/// Compile errors (including constructs outside the native subset) and runtime
/// faults alike.
pub fn run(hir: &HirFile, opts: &NativeOptions) -> Result<Value, NativeError> {
    compile_program(hir, opts)?.run(opts)
}

/// JIT-compile `hir` and invoke the stored function *value* `callee` with
/// `args` instead of running `main`. The full per-run setup/teardown of
/// [`run`] applies (tables installed, ops armed, module memory reclaimed) —
/// only the entry differs. See [`CompiledProgram::run_call`] to reuse the
/// module across calls.
///
/// This is how a summon's AI function runs: the `Value::Function` was
/// captured during an earlier run of the *same* `hir` (so its
/// `function_idx` / `DefId` resolve identically in the re-JIT'd module) and
/// is dispatched through `dispatch_call_value` exactly like an indirect call
/// inside the program. Globals are NOT initialized (`main` never runs):
/// matching Java would require the owner's live global state, which the
/// per-turn re-run model doesn't keep.
///
/// # Errors
/// Same failure modes as [`run`] (compile errors, runtime faults).
pub fn run_call(
    hir: &HirFile,
    opts: &NativeOptions,
    callee: &Value,
    args: Vec<Value>,
) -> Result<Value, NativeError> {
    compile_program(hir, opts)?.run_call(opts, callee, args)
}

/// What the [`NativeEmit::Jit`] path invokes once the module is finalized
/// and the runtime tables are installed.
enum JitEntry<'a> {
    /// Call the program's `main` (the normal [`run`] path).
    Main,
    /// Dispatch a stored function value with the given args ([`run_call`]).
    CallValue(&'a Value, Vec<Value>),
}

/// Compile `hir` according to `opts.emit`.
pub fn compile(hir: &HirFile, opts: &NativeOptions) -> Result<NativeArtifact, NativeError> {
    compile_entry(hir, opts, JitEntry::Main)
}

/// The MIR half of a compile: everything between the HIR and the choice of
/// backend target. Shared by the inspection / object paths and by
/// [`compile_program`], so all four see exactly the same program.
struct Lowered {
    program: leek_mir::ir::MirProgram,
    main_idx: usize,
    lang: Lang,
    /// Class `DefId` raw → constructor-thunk `program.functions` index.
    class_thunks: HashMap<u32, usize>,
    fn_rets: translate::FnRets,
    global_tys: HashMap<String, ValTy>,
    native_directives: HashMap<leek_hir::DefId, String>,
}

fn lower(hir: &HirFile, opts: &NativeOptions) -> Result<Lowered, NativeError> {
    let (mut program, errs) = leek_mir::lower_file(hir);
    if let Some(first) = errs.first() {
        // Keep the whole set, not just the first: each lowering diagnostic
        // has its own catalog code and its own span, and reporting one of
        // three reveals the other two only after the user fixes this one.
        let summary = format!("MIR lowering failed: {}", first.message);
        let span = first.span;
        return Err(NativeError::compile(summary)
            .at(span)
            .with_diagnostics(errs));
    }
    let main_idx = program
        .functions
        .iter()
        .position(|f| f.kind == leek_mir::ir::FunctionKind::Main)
        .ok_or_else(|| NativeError::compile("no main function"))?;

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
    // Program-wide function result kinds (drives cross-call typing) and
    // declared scalar kinds of typed globals (drives write coercion).
    let fn_rets = translate::compute_fn_rets(&program, lang);
    let global_tys = translate::global_scalar_tys(&program);
    let native_directives = collect_native_directives(hir);
    Ok(Lowered {
        program,
        main_idx,
        lang,
        class_thunks,
        fn_rets,
        global_tys,
        native_directives,
    })
}

fn compile_entry(
    hir: &HirFile,
    opts: &NativeOptions,
    entry: JitEntry<'_>,
) -> Result<NativeArtifact, NativeError> {
    // The JIT path is just "compile once, run once" over the same two pieces a
    // repeated runner (a fight turn loop) drives separately.
    if matches!(opts.emit, NativeEmit::Jit) {
        let program = compile_program(hir, opts)?;
        return Ok(NativeArtifact::Value(program.run_entry(opts, entry)?));
    }
    let Lowered {
        program,
        main_idx,
        lang,
        class_thunks,
        fn_rets,
        global_tys,
        native_directives,
    } = lower(hir, opts)?;
    let main = &program.functions[main_idx];

    match &opts.emit {
        NativeEmit::Clif => {
            // No ISA / module — render just `main` (calls to user
            // functions aren't lowered in this inspection mode).
            let sig = translate::function_sig(main, lang, &fn_rets, &program)?;
            let mut func = make_func(sig.ret);
            let mut fb_ctx = FunctionBuilderContext::new();
            translate::translate_function(
                &mut func,
                &mut fb_ctx,
                main,
                &sig,
                lang,
                &fn_rets,
                None,
                &HashMap::new(),
                &HashMap::new(),
                &program,
                &global_tys,
                &native_directives,
                &std::collections::HashSet::new(),
                false,
                false,
                false,
            )
            .map_err(|e| e.in_fn(&main.name).or_span(main.span))?;
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
                &mut ctx.func,
                &mut fb_ctx,
                main,
                &sig,
                lang,
                &fn_rets,
                None,
                &HashMap::new(),
                &HashMap::new(),
                &program,
                &global_tys,
                &native_directives,
                &std::collections::HashSet::new(),
                false,
                false,
                false,
            )
            .map_err(|e| e.in_fn(&main.name).or_span(main.span))?;
            ctx.compile(isa.as_ref(), &mut Default::default())
                .map_err(|e| NativeError::compile(format!("{e:?}")))?;
            let text = ctx
                .compiled_code()
                .and_then(|c| c.vcode.clone())
                .unwrap_or_else(|| "<no disassembly>".into());
            Ok(NativeArtifact::Text(text))
        }
        NativeEmit::Object(path) => {
            let isa = build_isa(opts)?;
            let ob = cranelift_object::ObjectBuilder::new(isa, "leek", default_libcall_names())
                .map_err(|e| NativeError::compile(e.to_string()))?;
            let mut module = cranelift_object::ObjectModule::new(ob);
            // Object emit isn't executed, so lambda addresses / the method
            // table aren't needed.
            let _ = define_program(
                &mut module,
                &program,
                main,
                lang,
                &fn_rets,
                &global_tys,
                &native_directives,
                &class_thunks,
                opts.debug_hooks,
                opts.link_game,
                false,
                &opts.hook_roots,
                DefineMode::Emit,
            )?;
            let bytes = module
                .finish()
                .emit()
                .map_err(|e| NativeError::compile(e.to_string()))?;
            std::fs::write(path, bytes).map_err(|e| NativeError::compile(e.to_string()))?;
            Ok(NativeArtifact::Object)
        }
        // Unreachable: handled at the top of this function, where the JIT
        // path splits into `compile_program` + `CompiledProgram::run_entry`.
        NativeEmit::Jit => unreachable!("Jit emit is handled before this match"),
    }
}

/// Everything a run publishes to the runtime's thread-local dispatch tables
/// before entering JIT'd code: this module's finalized addresses plus the
/// program-derived maps the shims resolve through.
///
/// A compile fills it once; every run reinstalls it, unconditionally. With more
/// than one compiled program live on a thread (a fight holds one per AI), the
/// tables in place belong to whichever program ran last, so "install only if it
/// changed" would hand a program another one's dispatch.
struct RuntimeTables {
    /// `function_idx` → (finalized uniform-ABI address, param count).
    lambda_fns: HashMap<usize, (*const u8, usize)>,
    /// `function_idx` → per-lambda `@`-by-ref mask over its user params.
    lambda_byref: HashMap<usize, Vec<bool>>,
    method_resolve: HashMap<u32, HashMap<String, usize>>,
    static_init: HashMap<(u32, String), usize>,
    user_fn_idx: HashMap<u32, usize>,
    user_fn_exact_arity: std::collections::HashSet<u32>,
    class_parent: HashMap<u32, Option<(u32, String)>>,
    class_ctor_thunk: HashMap<u32, usize>,
    class_string_method: HashMap<u32, usize>,
    class_reflect: HashMap<u32, HashMap<String, Vec<String>>>,
}

impl RuntimeTables {
    fn install(&self) {
        runtime::set_lambda_fns(self.lambda_fns.clone());
        runtime::set_lambda_byref(self.lambda_byref.clone());
        runtime::set_method_resolve(self.method_resolve.clone());
        runtime::set_static_init(self.static_init.clone());
        runtime::set_user_fn_idx(self.user_fn_idx.clone());
        runtime::set_user_fn_exact_arity(self.user_fn_exact_arity.clone());
        runtime::set_class_parent(self.class_parent.clone());
        runtime::set_class_ctor_thunk(self.class_ctor_thunk.clone());
        runtime::set_class_string_method(self.class_string_method.clone());
        runtime::set_class_reflect(self.class_reflect.clone());
    }
}

/// A JIT-compiled program, ready to run any number of times.
///
/// Splitting compilation from execution is what lets a fight compile each AI
/// **once** and then run it every turn: Cranelift codegen, module finalize and
/// the constant pool happen in [`compile_program`], while everything a run must
/// not inherit from the previous one — globals, static fields, the PRNG seed,
/// the op counter, the recursion guard, the runtime-fault channel — is (re)armed
/// per call in [`CompiledProgram::run`].
///
/// Deliberately neither `Send` nor `Sync` (it holds raw code addresses, and
/// every table and arena it installs is thread-local): compile and run a
/// program on the same thread.
pub struct CompiledProgram {
    /// `ManuallyDrop` because `JITModule::free_memory` consumes the module
    /// while `Drop::drop` only gets `&mut self`.
    module: std::mem::ManuallyDrop<cranelift_jit::JITModule>,
    main: cranelift_module::FuncId,
    ret_ty: ValTy,
    tables: RuntimeTables,
    /// The compile-time constants whose addresses are baked into this module's
    /// code (see `runtime::box_value`). Owned here, so they stay valid for
    /// every run and are released only when the module is — the per-run sweep
    /// (`free_run_boxes`) never sees them.
    _consts: runtime::ConstArena,
    /// How long the compile took. Reported by the *first* run through
    /// [`last_jit_split`] and zero after that, so a caller summing the split
    /// across a fight gets the one real compile cost, not one copy per turn.
    compile_dur: std::cell::Cell<std::time::Duration>,
}

impl std::fmt::Debug for CompiledProgram {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CompiledProgram")
            .field("ret_ty", &self.ret_ty)
            .finish_non_exhaustive()
    }
}

impl Drop for CompiledProgram {
    fn drop(&mut self) {
        // A plain `JITModule` drop LEAKS its mmap'd executable + data memory,
        // which accumulates across compiles and OOMs a process that JIT-compiles
        // many programs (e.g. the upstream-suite regression test compiles 10k+
        // cases in one process).
        //
        // SAFETY: `free_memory` requires that no function from this module is
        // executing or called afterwards. `run` returns only owned, JIT-
        // independent `Value`s (results are cloned out of their handles), no
        // JIT function is invoked outside a `run`, and this program is gone the
        // moment `drop` returns, so nothing can dispatch into it again. The
        // runtime tables it installed are overwritten by the next run's
        // `RuntimeTables::install` before they could be consulted.
        let module = unsafe { std::mem::ManuallyDrop::take(&mut self.module) };
        // SAFETY: see above — no JIT'd function from this module can run again.
        unsafe { module.free_memory() };
    }
}

impl CompiledProgram {
    /// Run the program's `main`, returning its value. See [`run`] for the
    /// single-shot equivalent.
    ///
    /// # Errors
    /// [`NativeErrorKind::Runtime`] if the program faulted or exhausted its op
    /// budget (`opts.op_limit`, armed here — it is not part of the compile).
    pub fn run(&self, opts: &NativeOptions) -> Result<Value, NativeError> {
        self.run_entry(opts, JitEntry::Main)
    }

    /// Dispatch the stored function value `callee` with `args` instead of
    /// running `main`, with the same per-run setup. See [`run_call`].
    ///
    /// # Errors
    /// Same as [`CompiledProgram::run`].
    pub fn run_call(
        &self,
        opts: &NativeOptions,
        callee: &Value,
        args: Vec<Value>,
    ) -> Result<Value, NativeError> {
        self.run_entry(opts, JitEntry::CallValue(callee, args))
    }

    fn run_entry(&self, opts: &NativeOptions, entry: JitEntry<'_>) -> Result<Value, NativeError> {
        // Publish this module's dispatch tables (another program may have run
        // since — see `RuntimeTables`).
        self.tables.install();
        let ptr = self.module.get_finalized_function(self.main);
        // Reset file-level globals so this run can't observe a previous
        // run's values (the global store is a process-wide thread-local).
        runtime::clear_globals();
        // Arm the runtime-fault channel: shims (e.g. a v4-strict OOB array
        // write) record a fault here instead of unwinding; we surface it
        // after `main` returns. `set_strict` lets those rules match the
        // upstream's strict-gated behavior.
        runtime::reset_runtime_error();
        runtime::set_strict(opts.strict);
        // Set the value-display version BEFORE running, so version-specific
        // string conversions during execution (e.g. a real's `.` vs `,`
        // decimal separator in v1) and the caller's final `value.to_string()`
        // both format correctly. (Was previously a side effect of the interpreter
        // backend removed in fd2d97e.)
        leek_runtime::DISPLAY_VERSION.with(|c| c.set(opts.version));
        // Arm the op counter + budget for this run. The JIT'd body charges
        // ops at the same MIR sites upstream does (so counts match);
        // `ops_used()` reads the total after `main` returns.
        runtime::reset_ops(opts.op_limit);
        // Arm the recursion guard: frames start at zero for this run, and
        // the stack budget is measured from here (just above the entry).
        runtime::arm_call_guard(opts.max_call_depth, opts.max_stack_bytes);
        let t_exec = std::time::Instant::now();
        // SAFETY: `leek_main` was declared with the matching ABI
        // (`() -> i64` or `() -> f64`) and the module finalized; the
        // pointer is a valid host function.
        let value = match entry {
            JitEntry::Main => match self.ret_ty {
                ValTy::Real => {
                    // SAFETY: see above.
                    let f =
                        unsafe { std::mem::transmute::<*const u8, extern "C" fn() -> f64>(ptr) };
                    Value::Real(f())
                }
                ValTy::Bool => {
                    // SAFETY: see above.
                    let f =
                        unsafe { std::mem::transmute::<*const u8, extern "C" fn() -> i64>(ptr) };
                    Value::Bool(f() != 0)
                }
                ValTy::Int => {
                    // SAFETY: see above.
                    let f =
                        unsafe { std::mem::transmute::<*const u8, extern "C" fn() -> i64>(ptr) };
                    Value::Int(f())
                }
                // A composite / boxed result: the function returns a
                // handle; recover the owned `Value` (freeing the box). A
                // top-level instance whose class declares `string()` is routed
                // through it (matching upstream's display).
                ValTy::Ref => {
                    // SAFETY: see above.
                    let f = unsafe {
                        std::mem::transmute::<*const u8, extern "C" fn() -> *mut Value>(ptr)
                    };
                    // Clone the result out of its handle (don't free the box —
                    // `free_run_boxes` below reclaims every handle at once). The
                    // clone keeps the result's `Rc`-backed data alive past the
                    // sweep.
                    // SAFETY: `f` returns a handle from the run that just
                    // finished, so it is live until `free_run_boxes` below.
                    let v = unsafe { runtime::read_handle(f()) };
                    runtime::invoke_top_level_string(v)
                }
            },
            // `run_call`: skip `main` entirely and dispatch the stored
            // function value (its indexes resolve against the tables
            // installed above). The result is already an owned `Value`.
            JitEntry::CallValue(callee, args) => {
                let _ = ptr;
                runtime::call_value_entry(callee, args, opts.version)
            }
        };
        LAST_JIT_SPLIT.with(|c| c.set(Some((self.compile_dur.take(), t_exec.elapsed()))));
        // Reclaim every boxed `Value` handle this run allocated. The result
        // was cloned out above (`read_handle`), so it and its reachable data
        // survive; all intermediate boxes — including the ones the global
        // store held — are freed here instead of leaking until process exit.
        // The module's *constants* live in a separate arena (`_consts`) and are
        // untouched, so the next run still reads live values.
        runtime::free_run_boxes();
        // A runtime fault recorded by a shim during the run (e.g. a
        // v4-strict out-of-bounds array write) takes precedence over the
        // computed value: the program errored.
        if let Some(code) = runtime::take_runtime_error() {
            drop(value);
            return Err(NativeError::runtime(code));
        }
        Ok(value)
    }
}

thread_local! {
    /// How many JIT modules have been built on this thread — the measurement
    /// behind "compile once per fight, not once per turn". Bumped by every
    /// successful [`compile_program`].
    static JIT_COMPILES: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// Number of JIT modules compiled on this thread since the last
/// [`reset_jit_compiles`]. A fight that reuses its modules keeps this at one
/// per (AI, codegen options) pair instead of one per AI per turn.
#[must_use]
pub fn jit_compiles() -> u64 {
    JIT_COMPILES.with(std::cell::Cell::get)
}

/// Zero the [`jit_compiles`] counter.
pub fn reset_jit_compiles() {
    JIT_COMPILES.with(|c| c.set(0));
}

/// JIT-compile `hir` into a reusable [`CompiledProgram`], WITHOUT running it.
///
/// This is the half of [`run`] a caller that executes the same AI many times
/// (the fight turn loop) wants to pay for once. `opts.emit` is ignored — the
/// result is always a JIT module — and so is everything else that only affects
/// execution; see [`CodegenKey`] for exactly which options
/// the produced code depends on, and therefore which ones a cache must key on.
///
/// # Errors
/// [`NativeErrorKind::Unsupported`] if the program uses a construct outside
/// the native subset, [`NativeErrorKind::Compile`] if MIR lowering or
/// Cranelift fails.
pub fn compile_program(
    hir: &HirFile,
    opts: &NativeOptions,
) -> Result<CompiledProgram, NativeError> {
    let built = build_jit_program(hir, opts);
    if built.is_err() {
        // A compile that bailed part-way may already have boxed some constants.
        // They belong to no module, so drop them here rather than let the next
        // compile adopt (and keep alive) another program's constants.
        drop(runtime::take_const_arena());
    }
    built
}

fn build_jit_program(hir: &HirFile, opts: &NativeOptions) -> Result<CompiledProgram, NativeError> {
    let t_compile = std::time::Instant::now();
    let lw = lower(hir, opts)?;
    let main = &lw.program.functions[lw.main_idx];
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
    let (
        id,
        ret_ty,
        lambda_funcs,
        method_resolve,
        static_init,
        user_fn_idx,
        exact_arity,
        class_string_method,
    ) = define_program(
        &mut module,
        &lw.program,
        main,
        lw.lang,
        &lw.fn_rets,
        &lw.global_tys,
        &lw.native_directives,
        &lw.class_thunks,
        opts.debug_hooks,
        opts.link_game,
        false,
        &opts.hook_roots,
        DefineMode::Emit,
    )?;
    module
        .finalize_definitions()
        .map_err(|e| NativeError::compile(e.to_string()))?;
    // Publish each lambda / bound-method's finalized address (+ param
    // count) so `call_value` / indirect calls can invoke them.
    let lambda_fns: HashMap<usize, (*const u8, usize)> = lambda_funcs
        .iter()
        .map(|(&idx, &(fid, nparams))| (idx, (module.get_finalized_function(fid), nparams)))
        .collect();
    // Per-lambda user-param `@`-by-ref masks (captures excluded) for the
    // higher-order builtins. A lambda's MIR params are
    // `[captures…, user-params…]`; the capture count comes from the
    // `MakeLambda` that builds it.
    let lambda_byref: HashMap<usize, Vec<bool>> = {
        use leek_mir::ir::{Rvalue, Statement};
        let mut ncaptures: HashMap<usize, usize> = HashMap::new();
        for f in &lw.program.functions {
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
        lambda_funcs
            .keys()
            .map(|&idx| {
                let f = &lw.program.functions[idx];
                let nc = ncaptures.get(&idx).copied().unwrap_or(0);
                let mask = f
                    .params
                    .iter()
                    .skip(nc)
                    .map(|p| f.locals[p.0 as usize].is_by_ref)
                    .collect();
                (idx, mask)
            })
            .collect()
    };
    // Class hierarchy for runtime `.super` (`x.class.super`): each
    // class → its explicit parent, or `None` for the implicit `Value`.
    let class_parent: HashMap<u32, Option<(u32, std::string::String)>> = lw
        .program
        .classes
        .iter()
        .map(|c| {
            let parent = c
                .parent_def
                .and_then(|pd| lw.program.class(pd).map(|pc| (pd.0, pc.name.clone())));
            (c.def_id.0, parent)
        })
        .collect();
    let tables = RuntimeTables {
        lambda_fns,
        lambda_byref,
        method_resolve,
        static_init,
        user_fn_idx,
        user_fn_exact_arity: exact_arity,
        class_parent,
        // Per-class constructor thunks: class `DefId` → thunk function idx
        // (its finalized address is already in `LAMBDA_FNS`), so a
        // `Value::ClassRef` invoked as a value constructs.
        class_ctor_thunk: lw.class_thunks,
        // Per-class `string()` display overrides (applied to the top-level
        // result). Their addresses are already in `LAMBDA_FNS`.
        class_string_method,
        // Per-class reflection name tables for runtime `x.class.fields` etc.
        class_reflect: translate::reflect_name_tables(&lw.program),
    };
    JIT_COMPILES.with(|c| c.set(c.get().saturating_add(1)));
    Ok(CompiledProgram {
        module: std::mem::ManuallyDrop::new(module),
        main: id,
        ret_ty,
        tables,
        // Everything above is codegen + runtime wiring; the program itself
        // hasn't run yet. Take ownership of the constants it baked in, and
        // stop the clock so the benchmark can report JIT compilation
        // separately from execution.
        _consts: runtime::take_const_arena(),
        compile_dur: std::cell::Cell::new(t_compile.elapsed()),
    })
}

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
                    .map(|(_, body)| {
                        body.split(['(', ' '])
                            .next()
                            .unwrap_or(body)
                            .trim()
                            .to_string()
                    })
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
    let (mut program, errs) = leek_mir::lower_file(hir);
    if let Some(first) = errs.first() {
        return Err(NativeError::compile(format!(
            "MIR lowering failed: {}",
            first.message
        )));
    }
    let main_idx = program
        .functions
        .iter()
        .position(|f| f.kind == leek_mir::ir::FunctionKind::Main)
        .ok_or_else(|| NativeError::compile("no main function"))?;
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
        .map_err(|e| NativeError::compile(e.to_string()))?;
    let mut module = cranelift_object::ObjectModule::new(ob);
    let (
        _main_id,
        _ret,
        lambda_funcs,
        method_resolve,
        static_init,
        user_fn_idx,
        exact_arity,
        class_string_method,
    ) = define_program(
        &mut module,
        &program,
        main,
        lang,
        &fn_rets,
        &global_tys,
        &native_directives,
        &class_thunks,
        opts.debug_hooks,
        opts.link_game,
        /* external_uniform */ true,
        &opts.hook_roots,
        DefineMode::Emit,
    )?;
    let bytes = module
        .finish()
        .emit()
        .map_err(|e| NativeError::compile(e.to_string()))?;
    std::fs::write(obj_path, bytes).map_err(|e| NativeError::compile(e.to_string()))?;

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

/// Whether [`define_program`] emits machine code or only translates.
///
/// One walk, one branch. The check path is the emit path minus
/// `Module::define_function`, so a construct [`check_native_compat`] reports
/// is one a real compile would have reported too, and one it stays silent
/// about really does translate. A second, "cheaper" walk would be a second
/// definition of "the native subset", and the two would drift.
enum DefineMode<'e> {
    /// Define every body; the first failure aborts the walk.
    Emit,
    /// Define nothing; push each function's failure and keep walking.
    CheckOnly(&'e mut Vec<NativeError>),
}

impl DefineMode<'_> {
    fn is_check_only(&self) -> bool {
        matches!(self, Self::CheckOnly(_))
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
    // Top-level zero-arg functions to force-compile as roots and register for
    // indirect invocation (the fight `beforeFight`/`afterFight` hooks). Matched
    // by name; never referenced from `main`, so they need explicit seeding.
    hook_roots: &[String],
    // Emit machine code, or translate every body and define none — see
    // [`DefineMode`].
    mode: DefineMode<'_>,
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

    // Hook roots (`beforeFight`/`afterFight`): top-level zero-arg user functions
    // invoked by the generator, never by the AI body. Pull each (and its
    // callees) into reachability so it compiles; it's wired into `user_fn_idx`
    // below so a `Function::User` value can dispatch to it.
    let hook_idxs: Vec<usize> = hook_roots
        .iter()
        .filter_map(|name| {
            program.functions.iter().position(|f| {
                f.kind == FunctionKind::User
                    && f.owning_class.is_none()
                    && f.params.is_empty()
                    && &f.name == name
            })
        })
        .collect();
    translate::extend_reachable_with(program, &hook_idxs, &mut reachable);

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
            .ok_or_else(|| NativeError::compile("reachable fn missing"))?;
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
            .map_err(|e| NativeError::compile(e.to_string()))?;
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
            .map_err(|e| NativeError::compile(e.to_string()))?;
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
            .map_err(|e| NativeError::compile(e.to_string()))?;
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
            .map_err(|e| NativeError::compile(e.to_string()))?;
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
    if let Some(gate) =
        translate::needs_cell_semantics(program, &reachable_indices, &lambda_set, lang.version)
    {
        return Err(NativeError::unsupported(gate.what)
            .at(gate.span)
            .in_fn(&gate.function));
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
    user_fn_idx.extend(translate::static_method_value_info(
        program,
        &reachable_indices,
    ));
    // Hook roots: register `DefId → idx` so `Function::User(hook)` resolves
    // through `dispatch_call_value` (uniform-compiled below via `value_methods`).
    for &idx in &hook_idxs {
        if let Some(d) = program.functions[idx].def_id {
            user_fn_idx.insert(d.0, idx);
        }
    }
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
            .map_err(|e| NativeError::compile(e.to_string()))?;
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
        .map_err(|e| NativeError::compile(e.to_string()))?;

    // Phase 2: translate + define each function body.
    //
    // `CheckOnly` runs this same translation and stops one line short of
    // `Module::define_function`: the compat check has to see exactly the
    // errors a compile sees, from exactly this code path, without paying
    // Cranelift's instruction selection.
    let check_only = mode.is_check_only();
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
        )
        // Function-level attribution: a site that had no statement span
        // still gets the function's name and its declaration to point at.
        .map_err(|e| e.in_fn(&f.name).or_span(f.span))?;
        if check_only {
            return Ok(());
        }
        module.define_function(fid, &mut ctx).map_err(|e| {
            NativeError::compile(e.to_string())
                .in_fn(&f.name)
                .at(f.span)
        })
    };

    // `Emit` stops at the first failure, as it always has. `CheckOnly` records
    // it and walks on, so one unsupported construct doesn't hide the other
    // four. Per *function* is the honest granularity: once a `Tx` errors
    // mid-body its `FunctionBuilder`'s block and value state is unusable, so a
    // walk can only restart at a function boundary.
    let mut mode = mode;
    let mut define_or_record = |module: &mut M,
                                f: &leek_mir::ir::MirFunction,
                                fid,
                                sig: &translate::FnSig,
                                uniform: bool|
     -> Result<(), NativeError> {
        match define_one(module, f, fid, sig, uniform) {
            Ok(()) => Ok(()),
            Err(e) => match &mut mode {
                DefineMode::Emit => Err(e),
                DefineMode::CheckOnly(errors) => {
                    errors.push(e);
                    Ok(())
                }
            },
        }
    };

    let dummy_sig = translate::FnSig {
        params: vec![],
        ret: ValTy::Ref,
        has_defaults: false,
    };
    for def_id in &reachable.defs {
        let f = by_id[def_id];
        let (fid, sig) = callees[def_id].clone();
        define_or_record(module, f, fid, &sig, false)?;
    }
    for &idx in &field_init_idxs {
        let f = &program.functions[idx];
        let (fid, sig) = field_init_callees[&idx].clone();
        define_or_record(module, f, fid, &sig, false)?;
    }
    for &idx in &lambda_idxs {
        let f = &program.functions[idx];
        let (fid, _) = lambda_funcs[&idx];
        define_or_record(module, f, fid, &dummy_sig, true)?;
    }
    for &idx in &thunk_idxs {
        let f = &program.functions[idx];
        let (fid, _) = lambda_funcs[&idx];
        define_or_record(module, f, fid, &dummy_sig, true)?;
    }
    for &idx in &value_methods {
        let f = &program.functions[idx];
        let (fid, _) = method_funcs[&idx];
        define_or_record(module, f, fid, &dummy_sig, true)?;
    }
    define_or_record(module, main, main_id, &main_sig, false)?;

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

/// Every way `hir` falls outside the native backend's subset, as diagnostics.
///
/// The same walk a compile does — MIR lowering, the prepare passes,
/// reachability, the whole-program cell-semantics gate, and a full
/// translation of every reachable body — minus the codegen: not one function
/// is defined or finished, so it costs Cranelift's IR construction and none
/// of its instruction selection, register allocation or relocation. That is
/// structural rather than careful; see [`CheckModule`].
///
/// An empty result means a native compile of `hir` would get past codegen; a
/// non-empty one carries one diagnostic per function that would fail, each
/// pointing at the construct (`E0600`) or the failure (`E0601`). The two
/// agree on the corpus by test, which is the whole point of reusing the walk:
/// a check that reports a construct a compile accepts is worse than no check,
/// because it puts a caret under working code.
///
/// This does not run the program, so it says nothing about runtime traps.
#[must_use]
pub fn check_native_compat(hir: &HirFile, opts: &NativeOptions) -> Vec<Diagnostic> {
    let lw = match lower(hir, opts) {
        Ok(lw) => lw,
        // A lowering failure is the frontend's diagnostics, each with its own
        // code and span — `diagnostics()` hands them over unflattened.
        Err(e) => return e.diagnostics(),
    };
    let main = &lw.program.functions[lw.main_idx];
    // `build_isa` is host feature detection, not compilation, and
    // `ObjectBuilder` only prepares a symbol table — the object is never
    // emitted, because `CheckModule` does not expose `finish()`.
    let built = build_isa(opts).and_then(|isa| {
        cranelift_object::ObjectBuilder::new(isa, "leek", default_libcall_names())
            .map_err(|e| NativeError::compile(e.to_string()))
    });
    let mut module = match built {
        Ok(ob) => CheckModule(cranelift_object::ObjectModule::new(ob)),
        Err(e) => return e.diagnostics(),
    };
    let mut errors: Vec<NativeError> = Vec::new();
    let walked = define_program(
        &mut module,
        &lw.program,
        main,
        lw.lang,
        &lw.fn_rets,
        &lw.global_tys,
        &lw.native_directives,
        &lw.class_thunks,
        opts.debug_hooks,
        opts.link_game,
        false,
        &opts.hook_roots,
        DefineMode::CheckOnly(&mut errors),
    );
    if let Err(gate) = walked {
        // A whole-program gate (`needs_cell_semantics`) or a failure in the
        // declaration phase, which has no per-function granularity to
        // collect at: one diagnostic, honestly, rather than a list of one.
        return gate.diagnostics();
    }
    errors.into_iter().flat_map(|e| e.diagnostics()).collect()
}

/// An [`ObjectModule`](cranelift_object::ObjectModule) that cannot define a
/// function.
///
/// [`check_native_compat`] needs a *real* module: `declare_function` and
/// `declare_func_in_func` are the bookkeeping every runtime-shim and
/// user-call lookup goes through, and the `module: None` path
/// (`NativeEmit::Clif`) turns each of those ~100 lookups into a fabricated
/// `unsupported` error — "call in text-dump emit mode", "runtime shim
/// leek_… not declared" — for a program that compiles fine.
///
/// What it must not do is pay for codegen. Rather than documenting that, this
/// newtype makes it structural: the two trait methods that compile a body
/// panic, and the inherent `finish()` that emits the object is not exposed at
/// all. An edit that reintroduces codegen into the check fails a test instead
/// of quietly costing a compile per invocation.
struct CheckModule(cranelift_object::ObjectModule);

impl Module for CheckModule {
    fn isa(&self) -> &dyn codegen::isa::TargetIsa {
        self.0.isa()
    }

    fn declarations(&self) -> &cranelift_module::ModuleDeclarations {
        self.0.declarations()
    }

    fn declare_function(
        &mut self,
        name: &str,
        linkage: Linkage,
        signature: &codegen::ir::Signature,
    ) -> cranelift_module::ModuleResult<cranelift_module::FuncId> {
        self.0.declare_function(name, linkage, signature)
    }

    fn declare_anonymous_function(
        &mut self,
        signature: &codegen::ir::Signature,
    ) -> cranelift_module::ModuleResult<cranelift_module::FuncId> {
        self.0.declare_anonymous_function(signature)
    }

    fn declare_data(
        &mut self,
        name: &str,
        linkage: Linkage,
        writable: bool,
        tls: bool,
    ) -> cranelift_module::ModuleResult<cranelift_module::DataId> {
        self.0.declare_data(name, linkage, writable, tls)
    }

    fn declare_anonymous_data(
        &mut self,
        writable: bool,
        tls: bool,
    ) -> cranelift_module::ModuleResult<cranelift_module::DataId> {
        self.0.declare_anonymous_data(writable, tls)
    }

    fn define_function_with_control_plane(
        &mut self,
        _func: cranelift_module::FuncId,
        _ctx: &mut Context,
        _ctrl_plane: &mut codegen::control::ControlPlane,
    ) -> cranelift_module::ModuleResult<()> {
        unreachable!("check_native_compat must never define a function")
    }

    fn define_function_bytes(
        &mut self,
        _func_id: cranelift_module::FuncId,
        _alignment: u64,
        _bytes: &[u8],
        _relocs: &[cranelift_module::ModuleReloc],
    ) -> cranelift_module::ModuleResult<()> {
        unreachable!("check_native_compat must never define a function")
    }

    fn define_data(
        &mut self,
        data_id: cranelift_module::DataId,
        data: &cranelift_module::DataDescription,
    ) -> cranelift_module::ModuleResult<()> {
        // Data is not a function body: declaring and defining a constant costs
        // no codegen, and the translator does neither today.
        self.0.define_data(data_id, data)
    }
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
            .map_err(|e| NativeError::compile(format!("flag {k}={v}: {e}")))
    };
    set(&mut fb, "opt_level", opts.opt_level.cranelift_str())?;
    set(&mut fb, "enable_verifier", bool_str(opts.enable_verifier))?;
    set(
        &mut fb,
        "preserve_frame_pointers",
        bool_str(opts.preserve_frame_pointers),
    )?;
    let flags = settings::Flags::new(fb);
    let builder = cranelift_native::builder().map_err(|e| NativeError::compile(e.to_string()))?;
    builder
        .finish(flags)
        .map_err(|e| NativeError::compile(e.to_string()))
}

fn bool_str(b: bool) -> &'static str {
    if b { "true" } else { "false" }
}

#[cfg(test)]
mod tests {
    //! Crate-internal repeat-run checks. These live here rather than in
    //! `tests/` because the only sound leak probe — the per-run value arena's
    //! retained capacity — is `pub(crate)` (see
    //! [`runtime::arena_allocated_bytes`]); exposing it publicly would make an
    //! implementation detail part of the backend's API.

    use leek_hir::HirFile;
    use leek_parser::{ParseFeatures, ast::AstNode, ast::SourceFile, parse_with_features};
    use leek_span::SourceId;
    use leek_syntax::{SyntaxNode, Version};

    use super::{NativeOptions, run, runtime};

    fn hir_v4(src: &str) -> HirFile {
        let source = SourceId::new(1).unwrap();
        let parsed = parse_with_features(src, source, Version::V4, ParseFeatures::default());
        let sf = SourceFile::cast(SyntaxNode::new_root(parsed.green)).expect("parse");
        leek_hir::lower_file_versioned(&sf, source, 4).0
    }

    #[test]
    fn repeated_runs_of_a_boxing_heavy_program_keep_the_arena_flat() {
        // Every run allocates ~200 string handles. If `free_run_boxes` stopped
        // reclaiming them (or a run leaked its globals' handles), the arena's
        // retained capacity would climb run after run instead of settling.
        let hir = hir_v4(
            "var a = [] for (var i = 0; i < 200; i++) { push(a, \"s\" + i) } return count(a)",
        );
        let opts = NativeOptions::release().with_lang(4, false);
        let mut steady = None;
        for n in 0..300 {
            let v = run(&hir, &opts).expect("run");
            assert_eq!(v.to_string(), "200", "run {n}");
            // The first handful of runs legitimately grow the bump chunk; take
            // the steady-state reading after them, never from run 0.
            if n == 5 {
                steady = Some(runtime::arena_allocated_bytes());
            }
        }
        let steady = steady.expect("run 5 happened");
        assert_eq!(
            runtime::arena_allocated_bytes(),
            steady,
            "the value arena grew between run 5 and run 300: a run is leaking boxes"
        );
    }
}
