//! `compile_program` once, run many times (#37 / GAME-03).
//!
//! The fight generator runs each AI up to `MAX_TURNS` times; before this, every
//! one of those runs re-ran the whole Cranelift pipeline. These tests pin the
//! two properties that makes possible: the module is built exactly once, and a
//! reused module behaves like a freshly compiled one — in particular its
//! *compile-time constants* (boxed `Value` handles whose addresses are baked
//! into the generated code) must survive the per-run box sweep.

use leek_backend_native::{NativeOptions, compile_program, jit_compiles, reset_jit_compiles, run};
use leek_hir::lower_file_versioned;
use leek_parser::{
    ast::{AstNode, SourceFile},
    parse,
};
use leek_span::SourceId;
use leek_syntax::{SyntaxNode, Version};

fn hir(src: &str) -> leek_hir::HirFile {
    let source = SourceId::new(1).unwrap();
    let parsed = parse(src, source, Version::V4);
    let file = SourceFile::cast(SyntaxNode::new_root(parsed.green.clone())).expect("parse");
    lower_file_versioned(&file, source, 4).0
}

/// Run `src` `n` times through one compiled module, returning each result
/// rendered the way the fight runner renders an AI's value.
fn run_n(src: &str, n: usize) -> Vec<String> {
    let opts = NativeOptions::debug();
    let program = compile_program(&hir(src), &opts).expect("compiles");
    (0..n)
        .map(|_| match program.run(&opts) {
            Ok(v) => v.to_string(),
            Err(e) => format!("ERR: {e}"),
        })
        .collect()
}

#[test]
fn one_module_serves_every_run() {
    let opts = NativeOptions::debug();
    let h = hir("var t = 0 for (var i = 0; i < 10; i++) { t += i } return t");

    reset_jit_compiles();
    let program = compile_program(&h, &opts).expect("compiles");
    for _ in 0..64 {
        assert_eq!(program.run(&opts).expect("runs").to_string(), "45");
    }
    assert_eq!(
        jit_compiles(),
        1,
        "64 runs of one program must cost one compile"
    );

    // The single-shot API is unchanged: it still compiles per call, which is
    // exactly the cost the fight turn loop used to pay 64 times over.
    reset_jit_compiles();
    for _ in 0..8 {
        assert_eq!(run(&h, &opts).expect("runs").to_string(), "45");
    }
    assert_eq!(jit_compiles(), 8);
}

/// A failed compile is a compile: it must not be silently counted as a
/// success, and (the caller's job) it is what a cache replays per turn.
#[test]
fn an_unsupported_program_fails_the_same_way_every_time() {
    let opts = NativeOptions::debug();
    // `2 ** b` with a non-constant exponent is outside the native subset.
    let h = hir("var b = 3 return 2 ** b");
    reset_jit_compiles();
    let first = compile_program(&h, &opts).expect_err("unsupported");
    let second = compile_program(&h, &opts).expect_err("unsupported");
    assert!(first.is_unsupported());
    // `Clone`/`PartialEq` are what let a fight hand the same error to every
    // turn instead of recompiling to re-derive it.
    assert_eq!(first, second);
    assert_eq!(first.clone(), first);
    assert_eq!(jit_compiles(), 0, "a failed compile builds no module");
}

/// Regression: compile-time constants used to be allocated in the *per-run*
/// bump arena, which `free_run_boxes` drops and resets at the end of every run.
/// With one run per module that was invisible; reusing a module made run 2 read
/// freed memory. They live in the module's own arena now.
#[test]
fn compile_time_constants_survive_later_runs() {
    // A boxed real constant.
    assert_eq!(run_n("return PI", 3), ["3.141592653589793"; 3]);
    // A boxed `BuiltinClass` handle.
    assert_eq!(run_n("return Number", 3), ["<class Number>"; 3]);
    // A boxed `ClassRef` + its compile-time-known name string.
    assert_eq!(run_n("class C { } return C.name", 3), ["\"C\""; 3]);
    // A boxed `Function::Builtin` handle reached through a higher-order call.
    assert_eq!(run_n("return arrayMap([-1, -2], abs)", 3), ["[1, 2]"; 3]);
    // A compile-time-folded composite default, deep-cloned per call.
    assert_eq!(
        run_n(
            "function f(a = [1, 2]) { a.push(3) return a } f() return f()",
            3
        ),
        ["[1, 2, 3]"; 3]
    );
}

/// The `foreach` snapshot and every element read out of it are per-run
/// allocations, never constants baked into the reused module (#111): a loop
/// must therefore yield the same thing on run 3 as on run 1.
#[test]
fn foreach_snapshots_are_rebuilt_per_run() {
    assert_eq!(
        run_n("var s = 0 for (var x in [1, 2, 3]) { s += x } return s", 3),
        ["6"; 3]
    );
    assert_eq!(
        run_n(
            "var r = '' for (var k : var v in ['a': 1, 'b': 2]) { r = r + k + v } return r",
            3
        ),
        ["\"a1b2\""; 3]
    );
    // Mutating the elements a run iterated must not leak into the next run's
    // snapshot of the same literal.
    assert_eq!(
        run_n("var a = [[1]] for (var x in a) { push(x, 2) } return a", 3),
        ["[[1, 2]]"; 3]
    );
}

/// The reflection tables (`C.fields`, `C.methods`) are compile-time-known
/// arrays. Handing out the constant itself let `C.fields.push(x)` mutate it —
/// harmless while every run rebuilt the module, a leak across turns once the
/// module is reused. Each read is a fresh copy now, like the interpreter's.
#[test]
fn reflection_arrays_are_fresh_per_read() {
    assert_eq!(
        run_n(
            "class C { var a var b } var f = C.fields f.push(\"z\") return f",
            3
        ),
        ["[\"a\", \"b\", \"z\"]"; 3]
    );
    assert_eq!(
        run_n(
            "class C { var a var b } C.fields.push(\"z\") return C['fields']",
            3
        ),
        ["[\"a\", \"b\"]"; 3]
    );
}

/// The per-run state a reused module must NOT carry over: file globals (and,
/// in the same `clear_globals` call, static fields) and the PRNG seed are all
/// (re)armed per run, not per compile.
#[test]
fn each_run_starts_from_a_clean_slate() {
    // A global assigned during the run is not visible to the next one.
    assert_eq!(
        run_n(
            "global g if (g == null) { g = 1 } else { g = g + 1 } return g",
            3
        ),
        ["1"; 3]
    );
    // The PRNG is reseeded per run, so successive runs draw the same sequence
    // (this is what keeps a seeded fight reproducible turn to turn).
    let draws = run_n("return randInt(0, 1000000)", 3);
    assert_eq!(draws[0], draws[1]);
    assert_eq!(draws[1], draws[2]);
}
