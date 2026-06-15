//! Cross-optimization-level equivalence: every program must produce the same
//! result at O1/O2/O3 (and under any fuel budget) as it does unoptimized (O0).
//!
//! This exercises the whole HIR optimization pipeline — constant folding /
//! propagation, inlining, desugaring, algebraic simplification, pure-function
//! DCE, and intrinsic recognition — end to end through the native JIT, so a pass
//! that ever changes observable behavior fails here. (MIR-level passes are
//! checked structurally in `leek-mir`'s `opt` unit tests.)

use leek_backend_native::{NativeOptions, run};
use leek_hir::lower_file_versioned;
use leek_hir::transform::optimize_hir_with;
use leek_parser::{ast::AstNode, ast::SourceFile, parse};
use leek_pipeline::{Fuel, OptConfig, OptLevel};
use leek_span::SourceId;
use leek_syntax::{SyntaxNode, Version};

fn lower(src: &str) -> leek_hir::HirFile {
    let source = SourceId::new(1).unwrap();
    let parsed = parse(src, source, Version::V4);
    let file = SourceFile::cast(SyntaxNode::new_root(parsed.green.clone())).expect("parse");
    lower_file_versioned(&file, source, 4).0
}

fn run_cfg(src: &str, cfg: &OptConfig) -> String {
    let mut hir = lower(src);
    optimize_hir_with(&mut hir, cfg);
    match run(&hir, &NativeOptions::debug()) {
        Ok(v) => v.to_string(),
        Err(e) => format!("ERR: {e}"),
    }
}

/// Programs spanning the features the new passes touch, each with a
/// deterministic result (no randomness).
const PROGRAMS: &[&str] = &[
    // constant folding / propagation chains
    "var a = 2 + 3 * 4\nreturn a\n",
    "var A = 2\nvar B = A + 1\nvar C = B * 2\nreturn C\n",
    // inlining (trivial + variable args) then folding
    "function dbl(x) { return x * 2 }\nreturn dbl(21)\n",
    "function add(a, b) { return a + b }\nvar k = 4\nreturn add(3, k)\n",
    // recursion (must NOT be inlined; still correct)
    "function fib(n) { return n < 2 ? n : fib(n - 1) + fib(n - 2) }\nreturn fib(12)\n",
    // compound assignment (desugaring) + loops
    "var x = 10\nx *= 3\nx -= 5\nreturn x\n",
    "var s = 0\nfor (var i = 0; i < 10; i++) { s += i }\nreturn s\n",
    "var r = 1\nvar n = 6\nfor (var i = 1; i <= n; i++) { r *= i }\nreturn r\n",
    "var s = 0\nvar i = 0\nwhile (i < 5) { s += i\ni++ }\nreturn s\n",
    // constant-condition elimination
    "var D = true\nif (D) { return 1 } else { return 2 }\n",
    "var F = false\nif (F) { return 1 }\nreturn 2\n",
    // short-circuit algebraic identity (false && _, true || _)
    "var t = 5\nvar b = false && (t > 0)\nreturn b ? 7 : 9\n",
    "var t = 5\nvar b = true || (t > 0)\nreturn b ? 7 : 9\n",
    // pure-function call whose result is unused (DCE at O2) — result unchanged
    "function sq(x) { return x * x }\nvar a = 7\nsq(a)\nreturn a\n",
    // strings (folding must not mis-handle concatenation / coercion)
    "var s = \"a\" + \"b\"\nreturn s == \"ab\" ? 1 : 0\n",
    // mixed: nested helpers + arithmetic
    "function inc(n) { return n + 1 }\nfunction twice(n) { return n * 2 }\nreturn twice(inc(20))\n",
    // modulo + conditional counting in a loop
    "var x = 17 % 5\nreturn x\n",
    "var n = 0\nfor (var i = 0; i < 100; i++) { if (i % 2 == 0) { n += 1 } }\nreturn n\n",
    // nested ternaries
    "var x = 5\nvar y = x > 3 ? (x < 10 ? 1 : 2) : 3\nreturn y\n",
    // boolean operators (non-constant)
    "var a = 3\nvar b = 4\nreturn a < b && b < 10 ? a + b : 0\n",
    "var p = true\nvar q = false\nreturn (p || q) && !q ? 1 : 0\n",
    // do-while accumulation
    "var sum = 0\nvar i = 1\ndo { sum += i\ni++ } while (i <= 5)\nreturn sum\n",
    // a helper with its own locals + loop (not inlinable) called twice
    "function pow2(n) { var r = 1\nfor (var i = 0; i < n; i++) { r *= 2 }\nreturn r }\nreturn pow2(10) + pow2(4)\n",
    "function max2(a, b) { return a > b ? a : b }\nreturn max2(3, 9) + max2(10, 2)\n",
];

#[test]
fn optimizations_preserve_results_across_levels() {
    for src in PROGRAMS {
        let base = run_cfg(src, &OptConfig::for_level(OptLevel::O0));
        assert!(
            !base.starts_with("ERR"),
            "O0 baseline itself errored:\n{src}\n=> {base}"
        );
        for level in [OptLevel::O1, OptLevel::O2, OptLevel::O3] {
            let got = run_cfg(src, &OptConfig::for_level(level));
            assert_eq!(
                got, base,
                "result diverged at {level:?} for program:\n{src}\n  O0={base}  {level:?}={got}"
            );
        }
    }
}

#[test]
fn fuel_budget_never_changes_the_result() {
    // Optimization is semantics-preserving at *any* budget, so a partially
    // optimized program (small fuel) yields the same value as O0 — the property
    // that makes `--fuel` a safe bisection knob.
    for src in PROGRAMS {
        let base = run_cfg(src, &OptConfig::for_level(OptLevel::O0));
        if base.starts_with("ERR") {
            continue;
        }
        for fuel in [0u64, 1, 2, 4, 8, 64] {
            let budget = if fuel == 0 {
                Fuel::Unlimited
            } else {
                Fuel::Limited(fuel)
            };
            let cfg = OptConfig::for_level(OptLevel::O3).with_fuel(budget);
            let got = run_cfg(src, &cfg);
            assert_eq!(got, base, "fuel={fuel} changed the result for:\n{src}");
        }
    }
}
