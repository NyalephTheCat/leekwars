//! Ahead-of-time (AOT) compilation: turn a `.leek` program into a standalone
//! native executable, compiled once and runnable many times with no per-run JIT.
//!
//! The JIT path ([`crate::run`]) finalizes machine code in-process every run.
//! AOT instead:
//!  1. emits a relocatable object (`leek_main` + the program's functions) via
//!     the existing [`NativeEmit::Object`](crate::NativeEmit) path,
//!  2. generates a tiny C `main` that calls `leek_main` and prints the result,
//!  3. links the two against the prebuilt **static runtime** archive
//!     (`libleek_aot_runtime.a` — the `leek_*` shims + math builtins + Rust std
//!     + harness glue) with `cc`.
//!
//! Linking with `cc` against a prebuilt archive means producing an executable
//! needs no per-program `cargo`/`rustc` run — only a fast C link. The archive is
//! built once by a normal workspace build (it's the `leek-aot-runtime` crate);
//! if it is missing, AOT builds it on demand.
//!
//! Supported subset: scalar / control-flow, arithmetic, globals, strings,
//! numeric arrays, and direct calls. String and null literals are
//! AOT-relocatable — their bytes are materialized in-binary at runtime (see
//! `Tx::const_string`) rather than baked as a compiler-process pointer. The
//! native **JIT** further handles lambdas, first-class functions, classes, and
//! builtin values (`PI`, `var f = abs`), but those still bake *compiler-process*
//! heap pointers (boxed-constant / class handles) as absolute immediates — a
//! dangling pointer in a separate AOT process — so they're rejected here (see
//! [`aot_unsupported_reason`]) rather than compiled into a segfaulting binary.
//! The dispatch-table metadata machinery ([`crate::aot_meta`]) is in place for
//! when the remaining baked pointers are made relocatable.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};

use leek_hir::HirFile;
use leek_runtime::Value;

use crate::translate::{self, Lang};
use crate::{NativeArtifact, NativeError, NativeOptions};

/// The scalar shape of the program's `main`, so the C harness can declare
/// `leek_main`'s FFI signature and convert its result to a [`Value`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MainRet {
    Int,
    Real,
    Bool,
    /// A heap handle (`*mut Value`) — arrays, strings, boxed scalars.
    Ref,
}

/// System libraries the Rust-std static archive needs at the final C link
/// (Linux/glibc). `cc` links libc itself.
const SYS_LIBS: &[&str] = &["-lpthread", "-ldl", "-lm", "-lrt", "-lgcc_s", "-lutil"];

/// Whether this host can produce an AOT executable at all.
///
/// The link hardcodes `-no-pie` (the cranelift object uses absolute
/// relocations) and the glibc-only `-lrt` / `-lgcc_s` / `-lutil`, none of which
/// exist on macOS. `tests/aot_exec.rs` has always declared the path
/// Linux-only; this makes the *code* say so too, so a macOS user gets one
/// sentence pointing at the JIT instead of raw linker noise.
const AOT_EXE_SUPPORTED: bool = cfg!(target_os = "linux");

/// Overrides the directory searched for `libleek_aot_runtime.a`. Set it and the
/// archive must be there — a miss is an error, never a silent fallback.
const RUNTIME_DIR_ENV: &str = "LEEK_AOT_RUNTIME_DIR";

/// Set to keep the per-compile scratch directory (the generated C and the
/// program object) instead of deleting it — the only way to inspect what the
/// harness generator produced.
const KEEP_TEMP_ENV: &str = "LEEK_AOT_KEEP_TEMP";

/// The static runtime archive's file name.
const ARCHIVE: &str = "libleek_aot_runtime.a";

// `AOT_ABI_TAG` + the `leek_aot_abi_<tag>` anchor function this build's
// generated C harness calls. See `build.rs`.
include!(concat!(env!("OUT_DIR"), "/abi.rs"));

/// The ABI anchor symbol for this build. The generated C harness calls it, so a
/// static runtime archive built from a different revision — which exports a
/// different tag — fails the link instead of corrupting shim arguments at run
/// time. Public so a test can check the archive really exports it.
#[must_use]
pub fn abi_symbol_name() -> String {
    format!("leek_aot_abi_{AOT_ABI_TAG}")
}

// ---- runtime entry points the static-runtime glue calls ----

/// Per-run runtime initialization before `leek_main`, mirroring the JIT path.
pub fn aot_setup(strict: bool, op_limit: u64, max_call_depth: u32, max_stack_bytes: usize) {
    crate::runtime::reset_ops(op_limit);
    crate::runtime::arm_call_guard(max_call_depth, max_stack_bytes);
    crate::runtime::clear_globals();
    crate::runtime::reset_runtime_error();
    crate::runtime::set_strict(strict);
}

/// A runtime fault recorded by a shim during the run (e.g. a strict
/// out-of-bounds write), or `None` if the program completed cleanly.
pub fn aot_take_error() -> Option<String> {
    crate::runtime::take_runtime_error()
}

/// All `(symbol, address)` runtime-shim pairs. The AOT static-runtime crate
/// references these so every shim is retained in its archive (and thus
/// available to satisfy the program object's calls at link time).
pub fn runtime_symbols() -> Vec<(&'static str, *const u8)> {
    crate::runtime::runtime_symbols()
}

/// Recover the owned [`Value`] from a `*mut Value` result, routing a top-level
/// instance through its `string()` override like the JIT path does.
///
/// # Safety
/// `ptr` must be the pointer returned by a `Ref`-typed `leek_main`.
pub unsafe fn aot_finish_ref(ptr: *mut Value) -> Value {
    // Clone the result out of its handle (AOT runs once then the process exits,
    // so the box need not be reclaimed); `take` no longer exists — handles are
    // owned by the per-run registry and read by cloning. See `runtime::read_handle`.
    // SAFETY: caller's contract — `ptr` is the `Ref` result of `leek_main`,
    // i.e. a handle from the run that just finished, not yet swept.
    let v = unsafe { crate::runtime::read_handle(ptr) };
    crate::runtime::invoke_top_level_string(v)
}

/// Compute the scalar return shape of the program's `main` without emitting
/// code — used to declare the C harness's `leek_main` signature.
pub fn main_ret(hir: &HirFile, opts: &NativeOptions) -> Result<MainRet, NativeError> {
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
    let _ = translate::append_ctor_thunks(&mut program, opts.version);
    translate::specialize_param_types(&mut program, lang);
    let main = &program.functions[main_idx];
    let fn_rets = translate::compute_fn_rets(&program, lang);
    let sig = translate::function_sig(main, lang, &fn_rets, &program)?;
    Ok(match sig.ret {
        translate::ValTy::Int => MainRet::Int,
        translate::ValTy::Real => MainRet::Real,
        translate::ValTy::Bool => MainRet::Bool,
        translate::ValTy::Ref => MainRet::Ref,
    })
}

/// Emit just the object file — exposed for callers that link it themselves.
pub fn compile_object(hir: &HirFile, opts: &NativeOptions) -> Result<NativeArtifact, NativeError> {
    crate::compile(hir, opts)
}

/// Whether `program` uses a construct AOT can't yet compile into a *standalone*
/// binary. Returns a human description of the first such construct, or `None`.
///
/// Two distinct blockers, both rooted in the native backend being JIT-first:
/// 1. **Compile-time pointer baking** — string literals and other constant
///    `Value`s are boxed in the *compiler* process and the heap pointer is
///    embedded in the code as an absolute immediate. Valid in-process (JIT),
///    but a dangling pointer in a separate AOT process. (Scalar/`bool`/`real`
///    constants and runtime-built int/real arrays are fine — no baked handle.)
/// 2. **Post-finalize dispatch tables** — lambda/method addresses and class
///    metadata the JIT installs after finalize. The AOT metadata machinery
///    ([`crate::aot_meta`]) can reinstall the *tables*, but the functions and
///    their captured constants still hit (1), so these stay rejected too.
///
/// The native **JIT** supports all of these; AOT rejects them up front rather
/// than emit a binary that segfaults.
fn aot_unsupported_reason(program: &leek_mir::ir::MirProgram) -> Option<&'static str> {
    use leek_mir::ir::{Callee, Rvalue, Statement};

    if !program.classes.is_empty() {
        return Some("classes");
    }
    for f in &program.functions {
        for b in &f.blocks {
            for s in &b.statements {
                match s {
                    Statement::Assign(_, rv) => match rv {
                        Rvalue::MakeLambda { .. } => return Some("lambdas / closures"),
                        Rvalue::FunctionRef(_) => return Some("first-class function references"),
                        Rvalue::New { .. } | Rvalue::ClassRef(..) => return Some("classes"),
                        Rvalue::MakeSuper { .. } | Rvalue::Super => return Some("`super`"),
                        // A builtin used as a *value* (`PI`, `var f = abs`,
                        // `var c = Array`) still boxes a constant `Value` into a
                        // baked handle — not yet relocatable.
                        Rvalue::BuiltinRef(_) => return Some("builtin constants / values"),
                        Rvalue::Unsupported(what) => return Some(what),
                        _ => {}
                    },
                    Statement::Call { call, .. } => match &call.callee {
                        Callee::Method { .. } => return Some("method calls"),
                        Callee::Indirect(_) => return Some("indirect / dynamic calls"),
                        Callee::SuperConstructor { .. } => return Some("`super(...)`"),
                        _ => {}
                    },
                    _ => {}
                }
            }
        }
    }
    None
}

/// Compile `hir` to a standalone native executable at `out`.
///
/// Requires a C compiler (`cc`, or `$CC`) and the prebuilt static runtime
/// archive (built on demand via `cargo` if absent).
pub fn compile_to_executable(
    hir: &HirFile,
    opts: &NativeOptions,
    out: &Path,
    quiet: bool,
) -> Result<(), NativeError> {
    if !AOT_EXE_SUPPORTED {
        return Err(NativeError::unsupported(
            "AOT executables (`miku build`, `leekc --emit exe`) are supported on Linux only \
             — the link needs `-no-pie` and the glibc-only `-lrt`/`-lgcc_s`/`-lutil`; \
             run the program on the JIT instead — `miku run`",
        ));
    }

    // Reject constructs that would bake a compiler-process heap pointer (strings,
    // lambdas, classes, …) into the standalone binary — they segfault at runtime.
    // See [`aot_unsupported_reason`]. (The dispatch-table metadata below is in
    // place for when the backend stops baking pointers; today it only ever
    // carries empty tables for the AOT-able subset.)
    let (program, _) = leek_mir::lower_file(hir);
    if let Some(what) = aot_unsupported_reason(&program) {
        return Err(NativeError::unsupported(format!(
            "AOT (compile-to-executable) does not yet support {what}; \
             run it on the JIT instead — `miku run` or `leekc --emit native`"
        )));
    }

    let ret = main_ret(hir, opts)?;

    // Scratch dir for the object + generated C, removed when `tmp` drops — so a
    // failed link (or an error emitting the object) leaves nothing behind.
    let tmp = Scratch::new()?;

    // Emit the program object (with externally linkable `leek_uniform_{idx}`
    // symbols) and the dispatch-table metadata the harness reinstalls at startup.
    let obj = tmp.join("program.o");
    let meta = crate::compile_object_with_meta(hir, opts, &obj)?;
    let blob = meta.to_blob()?;

    // Generate the C entry point and link everything with cc.
    let main_c = tmp.join("leek_entry.c");
    write_file(
        &main_c,
        &main_c_source(ret, opts, &blob, meta.lambda_entries()),
    )?;
    let lib_dir = locate_static_runtime(quiet)?;

    if !quiet {
        eprintln!("leek: linking standalone native executable (cc)…");
    }
    let cc = std::env::var("CC").unwrap_or_else(|_| "cc".to_string());
    let mut cmd = Command::new(&cc);
    cmd.arg("-no-pie") // the cranelift object uses absolute relocations
        .arg("-O2")
        .arg("-o")
        .arg(out)
        .arg(&main_c)
        .arg(&obj)
        .arg(format!("-L{}", lib_dir.display()))
        .arg("-lleek_aot_runtime")
        .args(SYS_LIBS)
        .stdin(Stdio::null())
        .stdout(if quiet {
            Stdio::null()
        } else {
            Stdio::inherit()
        })
        // Captured, not inherited, so the ABI-anchor miss can be recognized in
        // it; it is echoed verbatim below, so the user still sees `cc`'s own
        // diagnosis either way.
        .stderr(Stdio::piped());
    let output = cmd.output().map_err(|e| {
        NativeError::compile(format!("running `{cc}` (a C compiler on PATH?): {e}"))
    })?;
    // Echoed whether or not the link succeeded, so `cc`'s warnings on the
    // generated C reach the user exactly as they did when stderr was inherited.
    let stderr = String::from_utf8_lossy(&output.stderr);
    eprint!("{stderr}");
    if !output.status.success() {
        return Err(link_failure(&stderr, &lib_dir));
    }

    if !quiet {
        eprintln!("leek: wrote executable to {}", out.display());
    }
    Ok(())
}

/// Turn a failed `cc` link into a `NativeError`, recognizing the one failure
/// mode the ABI anchor exists to produce: the archive was built from a
/// different revision, so it exports a different `leek_aot_abi_<tag>` and the
/// harness's call to ours is unresolved.
fn link_failure(stderr: &str, lib_dir: &Path) -> NativeError {
    if stderr.contains(&abi_symbol_name()) {
        return NativeError::compile(format!(
            "the AOT static runtime archive in {} was built from a different revision of \
             this compiler (it does not export the ABI anchor `{}`); rebuild it with \
             `cargo build -p leek-aot-runtime` (add `--release` for the release profile)",
            lib_dir.display(),
            abi_symbol_name()
        ));
    }
    NativeError::compile("cc link of the AOT executable failed")
}

// ---- per-compile scratch directory ----

/// A scratch directory owned by one `compile_to_executable` call, deleted when
/// it drops.
///
/// Created *exclusively* (`create_dir`, retrying the next sequence number on
/// `AlreadyExists`) rather than `remove_dir_all` + `create_dir_all`: the path
/// lives in a world-writable `temp_dir()` and is predictable, so wiping
/// whatever is already there would let a local user hand us a symlink, and
/// would also let two compiles in one process (parallel tests, an LSP) delete
/// each other's `program.o` mid-link.
struct Scratch {
    path: PathBuf,
    keep: bool,
}

impl Scratch {
    fn new() -> Result<Self, NativeError> {
        Self::new_in(
            &std::env::temp_dir(),
            std::env::var_os(KEEP_TEMP_ENV).is_some(),
        )
    }

    fn new_in(base: &Path, keep: bool) -> Result<Self, NativeError> {
        static SCRATCH_SEQ: AtomicUsize = AtomicUsize::new(0);
        let pid = std::process::id();
        let fail = |why: String| {
            NativeError::compile(format!(
                "creating an AOT scratch directory under {}: {why}",
                base.display()
            ))
        };
        for _ in 0..1024 {
            let seq = SCRATCH_SEQ.fetch_add(1, Ordering::Relaxed);
            let path = base.join(format!("leek-aot-{pid}-{seq}"));
            match std::fs::create_dir(&path) {
                Ok(()) => return Ok(Self { path, keep }),
                // Taken already (another process's pid reuse, or a plant):
                // step over it rather than wiping whatever is there.
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
                // Anything else (no such base, no permission) will fail
                // identically for every other name, so stop.
                Err(e) => return Err(fail(e.to_string())),
            }
        }
        Err(fail("every candidate name was already taken".to_string()))
    }

    fn join(&self, name: &str) -> PathBuf {
        self.path.join(name)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        if self.keep {
            eprintln!(
                "leek: {KEEP_TEMP_ENV} is set — keeping the AOT scratch directory at {}",
                self.path.display()
            );
            return;
        }
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

// ---- C entry point ----

fn main_c_source(
    ret: MainRet,
    opts: &NativeOptions,
    blob: &[u8],
    lambda_entries: &[(usize, usize)],
) -> String {
    use std::fmt::Write as _;
    // (C return type, C print-helper name) per result kind.
    let (c_ty, printer) = match ret {
        MainRet::Int => ("long long", "leek_aot_print_int"),
        MainRet::Real => ("double", "leek_aot_print_real"),
        MainRet::Bool => ("long long", "leek_aot_print_bool"),
        MainRet::Ref => ("void *", "leek_aot_print_ref"),
    };

    // The dispatch-table metadata, embedded as a byte array.
    let mut meta_bytes = String::new();
    for (i, b) in blob.iter().enumerate() {
        if i % 20 == 0 {
            meta_bytes.push_str("\n    ");
        }
        let _ = write!(meta_bytes, "{b},");
    }

    // `extern` decls + the address table for each uniform-ABI function. We only
    // take addresses, so the declared signature is irrelevant.
    let mut externs = String::new();
    let mut table = String::new();
    for (idx, arity) in lambda_entries {
        let _ = writeln!(externs, "extern void leek_uniform_{idx}(void);");
        let _ = write!(
            table,
            "\n    {{ {idx}ULL, (const void *)&leek_uniform_{idx}, {arity}ULL }},"
        );
    }
    let n_lambdas = lambda_entries.len();
    let max_call_depth = opts.max_call_depth;
    let max_stack_bytes = opts.max_stack_bytes as u64;
    let abi = abi_symbol_name();

    format!(
        r#"/* Generated by the Leekscript AOT backend. Runs the linked program. */
#include <stdio.h>

extern {c_ty} leek_main(void);
extern void leek_aot_setup(int strict, unsigned long long op_limit, unsigned int max_call_depth,
                           unsigned long long max_stack_bytes);
extern char *leek_aot_error(void);
extern void {printer}({c_ty} value, int version);

/* ABI guard. This symbol's name carries a hash of the shim signatures this
   program object was compiled against; only a static runtime archive built
   from the same revision exports it. Calling it (rather than merely taking
   its address, which -O2 may drop) makes a mismatched archive an undefined
   reference at link time instead of corrupted shim arguments at run time. */
extern void {abi}(void);

/* Reinstall the dispatch tables (lambdas / methods / classes) at startup. */
struct leek_lambda_entry {{ unsigned long long idx; const void *func; unsigned long long arity; }};
extern int leek_aot_install(const unsigned char *blob, unsigned long blob_len,
                            const struct leek_lambda_entry *entries, unsigned long n);

static const unsigned char LEEK_META[] = {{{meta_bytes}
}};
{externs}
static const struct leek_lambda_entry LEEK_LAMBDAS[] = {{{table}
    {{ 0ULL, (const void *)0, 0ULL }}
}};

int main(void) {{
    {abi}();
    if (leek_aot_install(LEEK_META, sizeof(LEEK_META), LEEK_LAMBDAS, {n_lambdas}UL) != 0) {{
        fprintf(stderr, "error: the dispatch-table metadata embedded in this executable "
                        "could not be parsed\n");
        return 1;
    }}
    leek_aot_setup({strict}, {op_limit}, {max_call_depth}U, {max_stack_bytes}ULL);
    {c_ty} value = leek_main();
    char *err = leek_aot_error();
    if (err) {{
        fprintf(stderr, "error: %s\n", err);
        return 1;
    }}
    {printer}(value, {version});
    return 0;
}}
"#,
        c_ty = c_ty,
        printer = printer,
        abi = abi,
        meta_bytes = meta_bytes,
        externs = externs,
        table = table,
        n_lambdas = n_lambdas,
        strict = i32::from(opts.strict),
        op_limit = op_limit_literal(opts.op_limit),
        version = opts.version,
    )
}

/// Render the op limit as a C `unsigned long long` literal.
fn op_limit_literal(limit: u64) -> String {
    if limit == u64::MAX {
        "0xFFFFFFFFFFFFFFFFULL".into()
    } else {
        format!("{limit}ULL")
    }
}

// ---- static runtime archive ----

/// Pick the directory holding `libleek_aot_runtime.a`, or `None` if no built
/// archive exists anywhere we know to look.
///
/// Pure — every path it consults is a parameter — so the policy is unit
/// testable without touching the environment. In search order:
///
/// 1. `$LEEK_AOT_RUNTIME_DIR`, if set. An explicit override that does *not*
///    hold the archive is an **error**, never a silent fall-through to some
///    other archive: the whole point of setting it is to control which one.
/// 2. Next to the running executable, then `../lib` and `../lib/leek`. This is
///    what makes an *installed* or relocated `miku`/`leekc` work, and it also
///    covers the common dev layout, where `target/{profile}/miku` sits beside
///    the archive.
/// 3. `$CARGO_TARGET_DIR` (or `<workspace>/target`), `release` and `debug`,
///    taking the **newest by mtime**. Preferring `release` unconditionally, as
///    this used to, silently linked a stale release archive for anyone
///    iterating in debug.
fn resolve_runtime_dir(
    override_dir: Option<&Path>,
    exe_dir: Option<&Path>,
    target_dir: Option<&Path>,
) -> Result<Option<PathBuf>, NativeError> {
    if let Some(dir) = override_dir {
        if dir.join(ARCHIVE).is_file() {
            return Ok(Some(dir.to_path_buf()));
        }
        return Err(NativeError::compile(format!(
            "{RUNTIME_DIR_ENV} is set to {}, but {ARCHIVE} is not there",
            dir.display()
        )));
    }

    if let Some(exe_dir) = exe_dir {
        for rel in ["", "../lib", "../lib/leek"] {
            let dir = if rel.is_empty() {
                exe_dir.to_path_buf()
            } else {
                exe_dir.join(rel)
            };
            if dir.join(ARCHIVE).is_file() {
                return Ok(Some(dir));
            }
        }
    }

    if let Some(target_dir) = target_dir {
        let newest = ["release", "debug"]
            .into_iter()
            .map(|p| target_dir.join(p))
            .filter_map(|dir| {
                let mtime = std::fs::metadata(dir.join(ARCHIVE))
                    .and_then(|m| m.modified())
                    .ok()?;
                Some((mtime, dir))
            })
            .max_by_key(|(mtime, _)| *mtime)
            .map(|(_, dir)| dir);
        if newest.is_some() {
            return Ok(newest);
        }
    }

    Ok(None)
}

/// The already-built static runtime archive's directory, if there is one.
///
/// The lookup [`compile_to_executable`] does, minus the on-demand `cargo
/// build` — so a test can find the archive (or skip) without risking a nested
/// cargo under `cargo test`, which deadlocks on the target-directory lock.
#[must_use]
pub fn prebuilt_runtime_dir() -> Option<PathBuf> {
    prebuilt_runtime_dir_checked().ok().flatten()
}

/// [`prebuilt_runtime_dir`], keeping the one hard failure it swallows: an
/// explicitly-set `$LEEK_AOT_RUNTIME_DIR` that does not hold the archive.
fn prebuilt_runtime_dir_checked() -> Result<Option<PathBuf>, NativeError> {
    let exe = std::env::current_exe().ok();
    resolve_runtime_dir(
        std::env::var_os(RUNTIME_DIR_ENV)
            .map(PathBuf::from)
            .as_deref(),
        exe.as_deref().and_then(Path::parent),
        target_dir().as_deref(),
    )
}

/// `$CARGO_TARGET_DIR`, else `<workspace>/target` when a source tree is there.
fn target_dir() -> Option<PathBuf> {
    std::env::var_os("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .or_else(|| workspace_root().map(|r| r.join("target")))
}

/// Directory containing `libleek_aot_runtime.a`, building it once with `cargo`
/// if no built archive can be found and a source tree is available.
fn locate_static_runtime(quiet: bool) -> Result<PathBuf, NativeError> {
    if let Some(dir) = prebuilt_runtime_dir_checked()? {
        return Ok(dir);
    }

    // Not built yet — build it once (release), which needs the workspace
    // sources. A relocated install has none, so say what to do instead of
    // running `cargo` in a directory that isn't there.
    let Some(root) = workspace_root() else {
        return Err(NativeError::compile(format!(
            "no {ARCHIVE} found next to this executable, and no workspace source tree to \
             build one from; point {RUNTIME_DIR_ENV} at a directory holding it"
        )));
    };
    if !quiet {
        eprintln!("leek: building the AOT static runtime (one-time; cargo)…");
    }
    let status = Command::new("cargo")
        .arg("build")
        .arg("--release")
        .arg("-p")
        .arg("leek-aot-runtime")
        .current_dir(&root)
        .stdin(Stdio::null())
        .stdout(if quiet {
            Stdio::null()
        } else {
            Stdio::inherit()
        })
        .stderr(Stdio::inherit())
        .status()
        .map_err(|e| {
            NativeError::compile(format!("building static runtime (cargo on PATH?): {e}"))
        })?;
    if !status.success() {
        return Err(NativeError::compile(
            "building the AOT static runtime failed",
        ));
    }
    let dir = target_dir()
        .unwrap_or_else(|| root.join("target"))
        .join("release");
    if dir.join(ARCHIVE).is_file() {
        Ok(dir)
    } else {
        Err(NativeError::compile(format!(
            "static runtime archive not found at {}",
            dir.join(ARCHIVE).display()
        )))
    }
}

// ---- small fs/path helpers ----

/// The workspace root as it was at *compile* time, if that tree still exists.
///
/// `CARGO_MANIFEST_DIR` is baked in, so for an installed or relocated binary
/// this is simply absent. It returns `Option` rather than `Result` for exactly
/// that reason: a missing source tree must skip a candidate, not abort the
/// whole lookup before the other candidates are tried.
fn workspace_root() -> Option<PathBuf> {
    let here = Path::new(env!("CARGO_MANIFEST_DIR")); // crates/backends/leek-backend-native
    std::fs::canonicalize(here.join("../../..")).ok()
}

fn write_file(p: &Path, contents: &str) -> Result<(), NativeError> {
    std::fs::write(p, contents)
        .map_err(|e| NativeError::compile(format!("writing {}: {e}", p.display())))
}

#[cfg(test)]
mod tests {
    use super::{
        ARCHIVE, KEEP_TEMP_ENV, MainRet, RUNTIME_DIR_ENV, Scratch, abi_symbol_name,
        aot_unsupported_reason, link_failure, main_c_source, resolve_runtime_dir,
    };
    use crate::NativeOptions;
    use leek_hir::lower_file_versioned;
    use leek_parser::{ast::AstNode, ast::SourceFile, parse};
    use leek_span::SourceId;
    use leek_syntax::{SyntaxNode, Version};

    fn reason(src: &str) -> Option<&'static str> {
        let source = SourceId::new(1).unwrap();
        let parsed = parse(src, source, Version::V4);
        let sf = SourceFile::cast(SyntaxNode::new_root(parsed.green)).unwrap();
        let hir = lower_file_versioned(&sf, source, 4).0;
        let (program, _) = leek_mir::lower_file(&hir);
        aot_unsupported_reason(&program)
    }

    #[test]
    fn scalar_array_and_string_programs_are_aot_able() {
        assert_eq!(
            reason("function f(n) { return n * 2 }\nreturn f(21)\n"),
            None
        );
        assert_eq!(
            reason("var a = []\nfor (var i = 0; i < 3; i++) { push(a, i) }\nreturn count(a)\n"),
            None
        );
        // Strings are now relocatable (materialized in-binary via const_string).
        assert_eq!(reason("return \"hi\"\n"), None);
        assert_eq!(reason("var s = \"a\" + \"b\"\nreturn s\n"), None);
    }

    #[test]
    fn pointer_baking_constructs_are_rejected() {
        assert_eq!(
            reason("var f = x -> x + 1\nreturn f(5)\n"),
            Some("lambdas / closures")
        );
        assert_eq!(
            reason("function g(x) { return x }\nvar h = g\nreturn h(1)\n"),
            Some("first-class function references")
        );
        assert_eq!(
            reason("class P { integer x\nconstructor(v) { this.x = v } }\nreturn new P(7).x\n"),
            Some("classes")
        );
    }

    // ---- archive resolution ----

    /// A disposable directory tree. Built on [`Scratch`] (so these tests also
    /// exercise its exclusive create) and never on `env::set_var`, which is
    /// unsafe in edition 2024 and racy across test threads — which is why
    /// [`resolve_runtime_dir`] takes every path as a parameter.
    fn scratch() -> Scratch {
        Scratch::new_in(&std::env::temp_dir(), /*keep=*/ false).expect("scratch dir")
    }

    fn with_archive(base: &std::path::Path, rel: &str) -> std::path::PathBuf {
        let dir = base.join(rel);
        std::fs::create_dir_all(&dir).expect("create candidate dir");
        std::fs::write(dir.join(ARCHIVE), b"not really an archive").expect("write archive");
        dir
    }

    #[test]
    fn an_explicit_override_wins_over_every_other_candidate() {
        let tmp = scratch();
        let over = with_archive(&tmp.path, "override");
        let exe = with_archive(&tmp.path, "bin");
        let target = tmp.path.join("target");
        with_archive(&target, "release");
        let got = resolve_runtime_dir(Some(&over), Some(&exe), Some(&target)).expect("resolve");
        assert_eq!(got.as_deref(), Some(over.as_path()));
    }

    #[test]
    fn an_override_without_the_archive_errors_instead_of_falling_through() {
        let tmp = scratch();
        let over = tmp.path.join("empty");
        std::fs::create_dir_all(&over).expect("create the empty override dir");
        // A perfectly good archive sits in the next candidate; the override
        // must still lose, loudly — picking a different one silently is the
        // bug this whole ordering exists to prevent.
        let target = tmp.path.join("target");
        with_archive(&target, "release");
        let err = resolve_runtime_dir(Some(&over), None, Some(&target))
            .expect_err("an override that does not hold the archive must error");
        assert!(
            err.reason().contains(RUNTIME_DIR_ENV)
                && err.reason().contains(&*over.to_string_lossy()),
            "the error must name the variable and the directory: {err}"
        );
    }

    #[test]
    fn the_executables_own_directory_beats_the_target_dir() {
        let tmp = scratch();
        let exe = with_archive(&tmp.path, "bin");
        let target = tmp.path.join("target");
        with_archive(&target, "release");
        let got = resolve_runtime_dir(None, Some(&exe), Some(&target)).expect("resolve");
        assert_eq!(got.as_deref(), Some(exe.as_path()));
    }

    #[test]
    fn an_installed_layout_finds_the_archive_under_lib() {
        let tmp = scratch();
        let root = tmp.path.join("usr");
        let exe = root.join("bin");
        std::fs::create_dir_all(&exe).expect("create bin");
        let lib = with_archive(&root, "lib/leek");
        let got = resolve_runtime_dir(None, Some(&exe), None).expect("resolve");
        assert_eq!(
            got.map(|d| std::fs::canonicalize(d).expect("canonicalize")),
            Some(std::fs::canonicalize(lib).expect("canonicalize"))
        );
    }

    #[test]
    fn the_newest_archive_wins_not_release_unconditionally() {
        let tmp = scratch();
        let target = tmp.path.join("target");
        let release = with_archive(&target, "release");
        // Same second resolution on some filesystems, so make `debug` visibly
        // newer rather than relying on write order.
        let debug = with_archive(&target, "debug");
        let newer = std::time::SystemTime::now() + std::time::Duration::from_secs(60);
        std::fs::File::options()
            .write(true)
            .open(debug.join(ARCHIVE))
            .expect("open debug archive")
            .set_modified(newer)
            .expect("set mtime");
        let got = resolve_runtime_dir(None, None, Some(&target)).expect("resolve");
        assert_eq!(
            got.as_deref(),
            Some(debug.as_path()),
            "a fresher debug archive must beat a stale release one"
        );
        assert!(
            release.join(ARCHIVE).is_file(),
            "the release archive is still there"
        );
    }

    #[test]
    fn no_candidate_anywhere_is_not_an_error() {
        let tmp = scratch();
        let empty = tmp.path.join("nothing");
        std::fs::create_dir_all(&empty).expect("create dir");
        // `Ok(None)` rather than `Err`: the caller's next move is to build the
        // archive, which it can only decide once every candidate has been tried.
        assert_eq!(
            resolve_runtime_dir(None, Some(&empty), Some(&empty)).expect("resolve"),
            None
        );
    }

    // ---- scratch directory ----

    #[test]
    fn the_scratch_directory_is_removed_when_it_drops() {
        let base = scratch();
        let path = {
            let s = Scratch::new_in(&base.path, /*keep=*/ false).expect("scratch");
            std::fs::write(s.join("program.o"), b"x").expect("write a file into it");
            s.path.clone()
        };
        assert!(
            !path.exists(),
            "a failed link must not leak {}",
            path.display()
        );
    }

    #[test]
    fn the_scratch_directory_survives_when_asked_to_be_kept() {
        let base = scratch();
        let path = {
            let s = Scratch::new_in(&base.path, /*keep=*/ true).expect("scratch");
            s.path.clone()
        };
        assert!(
            path.exists(),
            "{KEEP_TEMP_ENV} must keep the generated C around"
        );
        std::fs::remove_dir_all(&path).expect("clean up");
    }

    #[test]
    fn two_scratch_directories_in_one_process_do_not_collide() {
        let base = scratch();
        let a = Scratch::new_in(&base.path, false).expect("first");
        let b = Scratch::new_in(&base.path, false).expect("second");
        assert_ne!(a.path, b.path);
        assert!(a.path.is_dir() && b.path.is_dir());
    }

    #[test]
    fn an_existing_directory_is_stepped_over_not_wiped() {
        let base = scratch();
        // Pre-plant the name the next sequence number would produce, holding a
        // file we must not lose: exclusive create is what stops a predictable
        // path under a world-writable /tmp being a hand-in.
        let planted = Scratch::new_in(&base.path, /*keep=*/ true).expect("planted");
        let planted_path = planted.path.clone();
        std::fs::write(planted_path.join("witness"), b"mine").expect("write witness");
        drop(planted);
        let s = Scratch::new_in(&base.path, false).expect("scratch");
        assert_ne!(s.path, planted_path);
        assert!(
            planted_path.join("witness").is_file(),
            "the pre-existing directory's contents must survive"
        );
        std::fs::remove_dir_all(&planted_path).expect("clean up");
    }

    // ---- the ABI anchor ----

    #[test]
    fn the_generated_c_calls_this_builds_abi_anchor() {
        // The Rust half (`build.rs` defines `leek_aot_abi_<tag>`) and the C
        // half (the harness calls it) have to agree on the name, and nothing
        // else would notice if they drifted apart: the check would simply stop
        // checking, silently.
        let c = main_c_source(MainRet::Int, &NativeOptions::release(), b"{}", &[]);
        let sym = abi_symbol_name();
        assert!(
            c.contains(&format!("extern void {sym}(void);")),
            "the harness must declare {sym}:\n{c}"
        );
        assert!(
            c.contains(&format!("    {sym}();")),
            "the harness must *call* {sym} — taking its address lets -O2 drop it:\n{c}"
        );
    }

    #[test]
    fn the_generated_c_aborts_when_the_metadata_does_not_install() {
        let c = main_c_source(MainRet::Int, &NativeOptions::release(), b"{}", &[]);
        assert!(
            c.contains("if (leek_aot_install("),
            "the harness must check leek_aot_install's return:\n{c}"
        );
    }

    #[test]
    fn a_missing_abi_anchor_is_reported_as_a_stale_archive() {
        let dir = std::path::Path::new("/some/target/release");
        let ld = format!(
            "/usr/bin/ld: /tmp/leek-aot-1-0/leek_entry.c:32: undefined reference to `{}'\n\
             collect2: error: ld returned 1 exit status\n",
            abi_symbol_name()
        );
        let err = link_failure(&ld, dir);
        assert!(
            err.reason().contains("different revision")
                && err.reason().contains("cargo build -p leek-aot-runtime"),
            "a stale archive must say so, and say how to fix it: {err}"
        );
        // Any other link failure keeps the generic message — `cc`'s own output
        // is echoed, and guessing would be worse than saying nothing.
        let other = link_failure("undefined reference to `some_other_symbol'", dir);
        assert_eq!(other.reason(), "cc link of the AOT executable failed");
    }
}
