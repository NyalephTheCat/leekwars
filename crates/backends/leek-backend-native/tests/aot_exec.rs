//! End-to-end AOT: `compile_to_executable` really links a binary with `cc`, and
//! running that binary prints what the JIT computes for the same program.
//!
//! This is the only test that exercises the generated C harness, the metadata
//! blob's trip through `leek_aot_install`, `leek_aot_setup`/`leek_aot_error`,
//! and the `leek_aot_print_*` helpers — everything between "the object is
//! written" and "the program prints its result".
//!
//! Linux/glibc only: the link hardcodes `-no-pie` and `-lrt`/`-lgcc_s`
//! (`aot::SYS_LIBS`).
#![cfg(target_os = "linux")]

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use leek_backend_native::{NativeOptions, aot, run};
use leek_hir::HirFile;
use leek_parser::{ParseFeatures, ast::AstNode, ast::SourceFile, parse_with_features};
use leek_span::SourceId;
use leek_syntax::{SyntaxNode, Version};

/// Every AOT-able program this test compiles, runs, and diffs against the JIT.
/// Covers each `MainRet` shape: `Int` (through a direct call), `Real`, `Bool`,
/// and `Ref` for both a string and an array.
const PROGRAMS: &[&str] = &[
    "function f(n) { return n * 2 }\nreturn f(21)\n",
    "return 1.5 + 2.25\n",
    "return true\n",
    "var s = \"a\" + \"b\"\nreturn s\n",
    "var a = []\nfor (var i = 0; i < 3; i++) { push(a, i) }\nreturn a\n",
    // A `foreach`: its snapshot shims (`leek_iter_value` / `leek_iter_key`)
    // must be in `runtime_symbols`, or only the AOT link notices (#111).
    "var s = 0\nfor (var k : var v in [1, 2, 3]) { s += k * v }\nreturn s\n",
];

fn hir_v4(src: &str) -> HirFile {
    let source = SourceId::new(1).unwrap();
    let parsed = parse_with_features(src, source, Version::V4, ParseFeatures::default());
    let sf = SourceFile::cast(SyntaxNode::new_root(parsed.green)).expect("parse");
    leek_hir::lower_file_versioned(&sf, source, 4).0
}

/// The directory holding `libleek_aot_runtime.a`, from the backend's own
/// resolution — `aot::prebuilt_runtime_dir` is exactly what the AOT path uses,
/// minus the on-demand `cargo build`. That branch would spawn a nested cargo
/// under `cargo test`, which deadlocks on the target-directory lock, so the
/// test skips instead when no archive is built. A normal workspace build (what
/// CI does before `cargo test`) produces it, so this test really runs there.
///
/// This used to re-implement the search, which meant the test could pass
/// against an archive the compiler would never have chosen.
fn static_runtime_dir() -> Option<PathBuf> {
    aot::prebuilt_runtime_dir()
}

/// Whether a C compiler is actually invocable, so a missing one skips instead
/// of failing the run.
fn have_cc() -> bool {
    let cc = std::env::var("CC").unwrap_or_else(|_| "cc".to_string());
    Command::new(cc)
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// The stdout the produced executable must print: the JIT's value formatted the
/// way the AOT harness formats it (`leek_aot_print_*` sets `DISPLAY_VERSION`
/// then `println!("{v}")`).
fn jit_output(hir: &HirFile, opts: &NativeOptions) -> String {
    let v = run(hir, opts).expect("JIT run");
    leek_runtime::DISPLAY_VERSION.with(|c| c.set(4));
    v.to_string()
}

/// Compile, run, and check every program. Kept as ONE test body run on one
/// thread: the compiles share process-level scratch state and each one links a
/// binary, so serializing them keeps the wall time predictable.
fn check_all(dir: &Path) {
    let opts = NativeOptions::release().with_lang(4, false);
    for (i, src) in PROGRAMS.iter().enumerate() {
        let hir = hir_v4(src);
        let want = jit_output(&hir, &opts);
        let exe = dir.join(format!("prog{i}"));
        aot::compile_to_executable(&hir, &opts, &exe, /*quiet=*/ true)
            .unwrap_or_else(|e| panic!("AOT compile of `{src}` failed: {e}"));
        let out = Command::new(&exe)
            .stdin(Stdio::null())
            .output()
            .unwrap_or_else(|e| panic!("running {}: {e}", exe.display()));
        assert!(
            out.status.success(),
            "`{src}` exited {:?}; stderr: {}",
            out.status.code(),
            String::from_utf8_lossy(&out.stderr)
        );
        assert_eq!(
            String::from_utf8_lossy(&out.stdout).trim(),
            want,
            "AOT and JIT disagree on `{src}`"
        );
    }

    // The failure path: a tripped op budget must reach the harness through
    // `leek_aot_error` and exit non-zero, not print a bogus value.
    let hir = hir_v4("var a = 0 for (var i = 0; i < 100000000; ++i) a = a + 1 return a");
    let exe = dir.join("overbudget");
    aot::compile_to_executable(
        &hir,
        &opts.clone().with_op_limit(10_000),
        &exe,
        /*quiet=*/ true,
    )
    .expect("AOT compile of the over-budget program");
    let out = Command::new(&exe)
        .stdin(Stdio::null())
        .output()
        .expect("running the over-budget program");
    assert!(!out.status.success(), "the over-budget program exited 0");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.starts_with("error: TOO_MUCH_OPERATIONS"),
        "expected a TOO_MUCH_OPERATIONS report, got: {stderr}"
    );
    assert!(
        out.stdout.is_empty(),
        "a faulting program must print no value, got: {}",
        String::from_utf8_lossy(&out.stdout)
    );
}

/// The ABI anchor must really be **in the archive**, not merely defined in the
/// backend crate. Nothing else notices if it is dropped: the generated C would
/// still call it, the link would still succeed against a matching archive, and
/// the guard would simply stop guarding. `leek_aot_force_link` is what retains
/// it, and that is one reference away from being deleted as dead code.
#[test]
fn the_static_runtime_archive_exports_this_builds_abi_anchor() {
    let Some(dir) = static_runtime_dir() else {
        eprintln!("skipping the ABI-anchor check: libleek_aot_runtime.a is not built");
        return;
    };
    let archive = dir.join("libleek_aot_runtime.a");
    let out = match Command::new("nm")
        .arg("--defined-only")
        .arg(&archive)
        .stdin(Stdio::null())
        .output()
    {
        Ok(out) if out.status.success() => out,
        _ => {
            eprintln!("skipping the ABI-anchor check: no working `nm`");
            return;
        }
    };
    let symbols = String::from_utf8_lossy(&out.stdout);
    let sym = aot::abi_symbol_name();
    assert!(
        symbols.lines().any(|l| l.ends_with(&format!(" {sym}"))),
        "{} does not export the ABI anchor {sym}; the guard in the generated C \
         would fail every link (or, if it were dropped, guard nothing)",
        archive.display()
    );
}

#[test]
fn aot_executables_print_what_the_jit_computes() {
    if static_runtime_dir().is_none() {
        eprintln!(
            "skipping the AOT executable test: libleek_aot_runtime.a is not built \
             (a plain `cargo build` produces it; CI builds the workspace first)"
        );
        return;
    }
    if !have_cc() {
        eprintln!("skipping the AOT executable test: no working C compiler ($CC or `cc`)");
        return;
    }

    let dir = std::env::temp_dir().join(format!("leek-aot-exec-test-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create the test scratch dir");

    // `cc` and the produced binaries are external processes: a hang in either
    // would otherwise hang the whole test run until CI's job timeout. Bound it.
    let (tx, rx) = std::sync::mpsc::channel();
    let work_dir = dir.clone();
    let worker = std::thread::spawn(move || {
        let r = std::panic::catch_unwind(|| check_all(&work_dir));
        let _ = tx.send(());
        r
    });
    let finished = rx.recv_timeout(std::time::Duration::from_secs(600)).is_ok();
    assert!(
        finished,
        "the AOT compile/run loop never finished: `cc` or a produced binary hung"
    );
    let result = worker.join().expect("the AOT worker thread");
    let _ = std::fs::remove_dir_all(&dir);
    if let Err(panic) = result {
        std::panic::resume_unwind(panic);
    }
}
