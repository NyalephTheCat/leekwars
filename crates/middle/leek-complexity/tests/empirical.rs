//! Empirical scaling tests.
//!
//! For each benchmark we:
//! 1. Build a self-contained Leekscript program that calls a
//!    target function with an array literal of size `n`.
//! 2. Run that program through the interpreter and capture the
//!    `ops_used` counter — the same number `getOperations()`
//!    exposes at runtime.
//! 3. Repeat at several sizes to measure the empirical scaling.
//! 4. Cross-check against the static analyser's prediction in
//!    two ways:
//!    - **Scaling check.** The ratio `ops(n_big) / ops(n_small)`
//!      should match what the static formula predicts at those
//!      sizes (within a slack — the formula's constants don't
//!      have to match the interpreter's exactly).
//!    - **Big-O class check.** The empirical scaling exponent
//!      should match the static `BigO` classification.
//!
//! Aligning with [`leek_charge`]'s default tariffs
//! (`per_stmt = per_expr = 1`) means the absolute numbers should
//! be in the same ballpark, but per-builtin dynamic costs aren't
//! mirrored in the static walker, so we focus on RATIOS for the
//! tight asserts.
//!
//! [`leek_charge`]: ../../leek-charge/index.html

use std::collections::HashMap;

use leek_complexity::{BigO, analyze_file};
use leek_diagnostics::Severity;
use leek_hir::lower_file;
use leek_parser::{ast::AstNode, ast::SourceFile, parse};
use leek_span::SourceId;
use leek_syntax::{SyntaxNode, Version};

/// Lex + parse + lower a fresh source string, panicking loudly on
/// parse errors so a malformed fixture surfaces fast.
fn to_hir(src: &str) -> leek_hir::HirFile {
    let source = SourceId::new(1).unwrap();
    let parsed = parse(src, source, Version::V4);
    assert!(
        !parsed
            .diagnostics
            .iter()
            .any(|d| d.severity == Severity::Error),
        "parse errors in fixture:\n{src}\n{:?}",
        parsed.diagnostics,
    );
    let root = SyntaxNode::new_root(parsed.green.clone());
    let file = SourceFile::cast(root).expect("source file root");
    let (hir, _diags) = lower_file(&file, source);
    hir
}

/// Run the program through the native JIT and return the `ops_used`
/// counter (native charges ops at the same MIR sites the interpreter did).
/// We use a generous limit (50M) because quadratic fixtures at n=200
/// already hit single millions of ops.
fn run_ops(src: &str) -> u64 {
    let hir = to_hir(src);
    let mut opts = leek_backend_native::NativeOptions::release();
    opts.version = 4;
    opts.op_limit = 50_000_000;
    opts.emit = leek_backend_native::NativeEmit::Jit;
    let _ = leek_backend_native::compile(&hir, &opts);
    leek_backend_native::ops_used()
}

/// Static prediction for the top-level main block. Returns the
/// folded scalar if every `Size` variable resolves to a literal
/// (which it does for our `[0..n]` array-literal call sites).
fn predict_ops(src: &str) -> Option<u64> {
    let hir = to_hir(src);
    let report = analyze_file(&hir);
    let main = report.iter().find(|c| c.name == "<main>")?;
    // No size variables expected in main (everything substituted
    // via array literals). evaluate_at with an empty map.
    main.formula.evaluate_at(&HashMap::new())
}

/// `[0, 1, ..., n - 1]` literal.
fn array_literal(n: u64) -> String {
    let mut s = String::from("[");
    for i in 0..n {
        if i > 0 {
            s.push_str(", ");
        }
        s.push_str(&i.to_string());
    }
    s.push(']');
    s
}

/// Build a full program: definitions + a main that calls
/// `entry` with `[0..n]`.
fn program(defs: &str, entry: &str, n: u64) -> String {
    format!(
        "// @version:4\n{defs}\nreturn {entry}({arr})\n",
        defs = defs,
        entry = entry,
        arr = array_literal(n),
    )
}

/// Assert that `actual / a_at_small` matches `expected / e_at_small`
/// within a relative tolerance. Used to compare the predicted vs
/// observed scaling without pinning absolute constants.
// Op counts in these tests are far below 2^52, so the `u64 → f64` casts in
// the ratio math are exact.
#[allow(clippy::cast_precision_loss)]
fn assert_scaling_close(
    actual_big: u64,
    actual_small: u64,
    expected_big: u64,
    expected_small: u64,
    label: &str,
) {
    let a_ratio = actual_big as f64 / actual_small.max(1) as f64;
    let e_ratio = expected_big as f64 / expected_small.max(1) as f64;
    let drift = (a_ratio - e_ratio).abs() / e_ratio.max(1.0);
    assert!(
        drift < 0.25,
        "{label}: observed scaling {a_ratio:.2} vs predicted \
         {e_ratio:.2} (drift {drift:.2})\n\
         actual: {actual_small} → {actual_big}\n\
         predicted: {expected_small} → {expected_big}",
    );
}

/// Confirm the empirical exponent of growth matches the big-O.
/// `log_ratio = log(actual_big / actual_small) / log(n_big / n_small)`
/// should land near 0 for `O(1)`, 1 for `O(n)`, 2 for `O(n²)`.
#[allow(clippy::cast_precision_loss)] // op counts ≪ 2^52 — casts are exact
fn empirical_exponent(actual_big: u64, actual_small: u64, n_big: u64, n_small: u64) -> f64 {
    let ratio = (actual_big as f64 / actual_small.max(1) as f64).ln();
    let n_ratio = (n_big as f64 / n_small.max(1) as f64).ln();
    ratio / n_ratio.max(0.001)
}

// ─── benchmarks ────────────────────────────────────────────────────

#[test]
fn linear_sum_scales_linearly() {
    let defs = "\
function sum(arr) {\n\
    var t = 0\n\
    for (var x in arr) { t = t + x }\n\
    return t\n\
}\n";
    let (n_small, n_big) = (40u64, 200u64);
    let src_small = program(defs, "sum", n_small);
    let src_big = program(defs, "sum", n_big);

    let actual_small = run_ops(&src_small);
    let actual_big = run_ops(&src_big);
    let predicted_small = predict_ops(&src_small).expect("static formula resolved");
    let predicted_big = predict_ops(&src_big).expect("static formula resolved");

    // Empirical exponent ≈ 1 for linear scaling.
    let exp = empirical_exponent(actual_big, actual_small, n_big, n_small);
    assert!(
        (exp - 1.0).abs() < 0.20,
        "expected linear (exponent ~1), got {exp:.2}\n\
         ops {n_small} = {actual_small}, ops {n_big} = {actual_big}",
    );

    // The static prediction's scaling should match too.
    assert_scaling_close(
        actual_big,
        actual_small,
        predicted_big,
        predicted_small,
        "linear sum",
    );
}

#[test]
fn quadratic_nested_loop_scales_quadratically() {
    let defs = "\
function pairs(arr) {\n\
    var k = 0\n\
    for (var a in arr) {\n\
        for (var b in arr) {\n\
            k = k + 1\n\
        }\n\
    }\n\
    return k\n\
}\n";
    let (n_small, n_big) = (20u64, 60u64);
    let src_small = program(defs, "pairs", n_small);
    let src_big = program(defs, "pairs", n_big);
    let actual_small = run_ops(&src_small);
    let actual_big = run_ops(&src_big);

    // Empirical exponent ≈ 2 for quadratic.
    let exp = empirical_exponent(actual_big, actual_small, n_big, n_small);
    assert!(
        (exp - 2.0).abs() < 0.30,
        "expected quadratic (exponent ~2), got {exp:.2}\n\
         ops {n_small} = {actual_small}, ops {n_big} = {actual_big}",
    );

    let predicted_small = predict_ops(&src_small).expect("static formula resolved");
    let predicted_big = predict_ops(&src_big).expect("static formula resolved");
    assert_scaling_close(
        actual_big,
        actual_small,
        predicted_big,
        predicted_small,
        "quadratic pairs",
    );
}

#[test]
fn constant_function_does_not_scale_with_input() {
    let defs = "\
function pick(arr) {\n\
    return arr[0]\n\
}\n";
    // For a constant-time function, the only n-dependence comes
    // from main building the literal — and even that depends on
    // the array literal, which IS O(n). So total ops will scale
    // linearly with n (main's array literal eval cost) but the
    // function itself is O(1). We just check the function's
    // static big-O.
    let src = program(defs, "pick", 10);
    let hir = to_hir(&src);
    let report = analyze_file(&hir);
    let pick = report.iter().find(|c| c.name == "pick").unwrap();
    assert!(
        matches!(pick.big_o, BigO::Constant),
        "expected O(1), got {:?}",
        pick.big_o,
    );
}

#[test]
fn halving_while_loop_scales_logarithmically() {
    // `while (i < n) { i *= 2 }` runs ~log₂(n) times. We verify
    // the empirical exponent against log-linear scaling.
    let defs = "\
function climb(arr) {\n\
    var i = 1\n\
    while (i < count(arr)) {\n\
        i *= 2\n\
    }\n\
    return i\n\
}\n";
    let (n_small, n_big) = (16u64, 1024u64);
    let src_small = program(defs, "climb", n_small);
    let src_big = program(defs, "climb", n_big);
    let actual_small = run_ops(&src_small);
    let actual_big = run_ops(&src_big);
    // ops should be dominated by main's `[0..n]` literal cost
    // (linear in n) plus a log component from climb. We sanity-
    // check that climb itself isn't quadratic by confirming the
    // exponent doesn't exceed ~1.2.
    let exp = empirical_exponent(actual_big, actual_small, n_big, n_small);
    assert!(
        exp <= 1.3,
        "halving-loop wrapper exponent {exp:.2} suggests super-linear scaling\n\
         ops {n_small} = {actual_small}, ops {n_big} = {actual_big}",
    );

    // Static check: climb itself is O(log n).
    let hir = to_hir(&src_small);
    let report = analyze_file(&hir);
    let climb = report.iter().find(|c| c.name == "climb").unwrap();
    match &climb.big_o {
        BigO::Log(v) => assert_eq!(v.name, "arr"),
        other => panic!(
            "expected O(log arr), got {other:?}\nformula = {}",
            climb.formula,
        ),
    }
}

#[test]
fn array_map_with_lambda_scales_linearly() {
    let defs = "\
function doubled(arr) {\n\
    return arrayMap(arr, x -> x * 2)\n\
}\n";
    let (n_small, n_big) = (40u64, 200u64);
    let src_small = program(defs, "doubled", n_small);
    let src_big = program(defs, "doubled", n_big);
    let actual_small = run_ops(&src_small);
    let actual_big = run_ops(&src_big);
    let exp = empirical_exponent(actual_big, actual_small, n_big, n_small);
    assert!(
        (exp - 1.0).abs() < 0.25,
        "expected linear (~1), got {exp:.2}\n\
         ops {n_small} = {actual_small}, ops {n_big} = {actual_big}",
    );
}

#[test]
fn caller_with_substituted_callee_scales_correctly() {
    // outer calls a linear callee inside its own loop → O(n²).
    let defs = "\
function inner(arr) {\n\
    var t = 0\n\
    for (var x in arr) { t = t + x }\n\
    return t\n\
}\n\
function outer(arr) {\n\
    var s = 0\n\
    for (var y in arr) {\n\
        s = s + inner(arr)\n\
    }\n\
    return s\n\
}\n";
    let (n_small, n_big) = (15u64, 45u64);
    let src_small = program(defs, "outer", n_small);
    let src_big = program(defs, "outer", n_big);
    let actual_small = run_ops(&src_small);
    let actual_big = run_ops(&src_big);
    let exp = empirical_exponent(actual_big, actual_small, n_big, n_small);
    assert!(
        (exp - 2.0).abs() < 0.30,
        "expected quadratic via call-graph substitution, got exp {exp:.2}\n\
         ops {n_small} = {actual_small}, ops {n_big} = {actual_big}",
    );

    // The static prediction should also be quadratic.
    let hir = to_hir(&src_small);
    let report = analyze_file(&hir);
    let outer = report.iter().find(|c| c.name == "outer").unwrap();
    match &outer.big_o {
        BigO::Quadratic(_) => {}
        other => panic!(
            "expected O(arr²), got {other:?}\nformula = {}",
            outer.formula
        ),
    }
}
