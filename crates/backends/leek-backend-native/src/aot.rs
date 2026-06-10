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

// ---- runtime entry points the static-runtime glue calls ----

/// Per-run runtime initialization before `leek_main`, mirroring the JIT path.
pub fn aot_setup(strict: bool, op_limit: u64) {
    crate::runtime::set_enforce_budget(op_limit != u64::MAX);
    crate::runtime::reset_ops(op_limit);
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
    crate::runtime::invoke_top_level_string(unsafe { crate::runtime::read_handle(ptr) })
}

/// Compute the scalar return shape of the program's `main` without emitting
/// code — used to declare the C harness's `leek_main` signature.
pub fn main_ret(hir: &HirFile, opts: &NativeOptions) -> Result<MainRet, NativeError> {
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
    // Reject constructs that would bake a compiler-process heap pointer (strings,
    // lambdas, classes, …) into the standalone binary — they segfault at runtime.
    // See [`aot_unsupported_reason`]. (The dispatch-table metadata below is in
    // place for when the backend stops baking pointers; today it only ever
    // carries empty tables for the AOT-able subset.)
    let (program, _) = leek_mir::lower_file(hir);
    if let Some(what) = aot_unsupported_reason(&program) {
        return Err(NativeError::Unsupported(format!(
            "AOT (compile-to-executable) does not yet support {what}; \
             run it on the JIT instead — `miku run` or `leekc --emit native`"
        )));
    }

    let ret = main_ret(hir, opts)?;

    let tmp = std::env::temp_dir().join(format!("leek-aot-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    mkdirs(&tmp)?;

    // Emit the program object (with externally linkable `leek_uniform_{idx}`
    // symbols) and the dispatch-table metadata the harness reinstalls at startup.
    let obj = tmp.join("program.o");
    let meta = crate::compile_object_with_meta(hir, opts, &obj)?;
    let blob = meta.to_blob();

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
        .stderr(Stdio::inherit());
    let status = cmd.status().map_err(|e| {
        NativeError::Compile(format!("running `{cc}` (a C compiler on PATH?): {e}"))
    })?;
    if !status.success() {
        return Err(NativeError::Compile(
            "cc link of the AOT executable failed".into(),
        ));
    }

    let _ = std::fs::remove_dir_all(&tmp);
    if !quiet {
        eprintln!("leek: wrote executable to {}", out.display());
    }
    Ok(())
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

    format!(
        r#"/* Generated by the Leekscript AOT backend. Runs the linked program. */
#include <stdio.h>

extern {c_ty} leek_main(void);
extern void leek_aot_setup(int strict, unsigned long long op_limit);
extern char *leek_aot_error(void);
extern void {printer}({c_ty} value, int version);

/* Reinstall the dispatch tables (lambdas / methods / classes) at startup. */
struct leek_lambda_entry {{ unsigned long long idx; const void *func; unsigned long long arity; }};
extern void leek_aot_install(const unsigned char *blob, unsigned long blob_len,
                             const struct leek_lambda_entry *entries, unsigned long n);

static const unsigned char LEEK_META[] = {{{meta_bytes}
}};
{externs}
static const struct leek_lambda_entry LEEK_LAMBDAS[] = {{{table}
    {{ 0ULL, (const void *)0, 0ULL }}
}};

int main(void) {{
    leek_aot_install(LEEK_META, sizeof(LEEK_META), LEEK_LAMBDAS, {n_lambdas}UL);
    leek_aot_setup({strict}, {op_limit});
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

/// Directory containing `libleek_aot_runtime.a`. Prefers an already-built
/// release archive, then debug; builds it once with `cargo` if neither exists.
fn locate_static_runtime(quiet: bool) -> Result<PathBuf, NativeError> {
    let root = workspace_root()?;
    let target =
        std::env::var_os("CARGO_TARGET_DIR").map_or_else(|| root.join("target"), PathBuf::from);
    let archive = "libleek_aot_runtime.a";
    for profile in ["release", "debug"] {
        let dir = target.join(profile);
        if dir.join(archive).is_file() {
            return Ok(dir);
        }
    }
    // Not built yet — build it once (release).
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
            NativeError::Compile(format!("building static runtime (cargo on PATH?): {e}"))
        })?;
    if !status.success() {
        return Err(NativeError::Compile(
            "building the AOT static runtime failed".into(),
        ));
    }
    let dir = target.join("release");
    if dir.join(archive).is_file() {
        Ok(dir)
    } else {
        Err(NativeError::Compile(format!(
            "static runtime archive not found at {}",
            dir.join(archive).display()
        )))
    }
}

// ---- small fs/path helpers ----

fn workspace_root() -> Result<PathBuf, NativeError> {
    let here = Path::new(env!("CARGO_MANIFEST_DIR")); // crates/backends/leek-backend-native
    std::fs::canonicalize(here.join("../../.."))
        .map_err(|e| NativeError::Compile(format!("locating workspace root: {e}")))
}

fn mkdirs(p: &Path) -> Result<(), NativeError> {
    std::fs::create_dir_all(p)
        .map_err(|e| NativeError::Compile(format!("creating {}: {e}", p.display())))
}

fn write_file(p: &Path, contents: &str) -> Result<(), NativeError> {
    std::fs::write(p, contents)
        .map_err(|e| NativeError::Compile(format!("writing {}: {e}", p.display())))
}

#[cfg(test)]
mod tests {
    use super::aot_unsupported_reason;
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
}
