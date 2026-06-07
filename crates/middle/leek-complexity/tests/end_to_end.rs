//! End-to-end complexity analysis: source → HIR → CostExpr → BigO.
//!
//! Each test parses a small Leekscript snippet, runs `analyze_file`,
//! and asserts on:
//!  - The big-O class for the function under test.
//!  - That the formula contains the expected size variables (or
//!    is a constant when expected).
//!
//! Constant terms aren't pinned to exact values — they shift any
//! time the per-stmt tariff is retuned. The big-O class is the
//! load-bearing invariant.

use leek_complexity::{BigO, Complexity, analyze_file};
use leek_diagnostics::Severity;
use leek_hir::lower_file;
use leek_parser::{ast::AstNode, ast::SourceFile, parse};
use leek_span::SourceId;
use leek_syntax::{SyntaxNode, Version};

fn analyze(src: &str) -> Vec<Complexity> {
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
    let file = SourceFile::cast(root).expect("source file");
    let (hir, _diags) = lower_file(&file, source);
    analyze_file(&hir)
}

fn find<'a>(results: &'a [Complexity], name: &str) -> &'a Complexity {
    results
        .iter()
        .find(|c| c.name == name)
        .unwrap_or_else(|| panic!("no complexity result for `{name}`"))
}

// ─── slice 1: constant cost ─────────────────────────────────────────

#[test]
fn straight_line_function_is_constant() {
    let r = analyze(
        "\
function add(integer a, integer b) {\n\
    var c = a + b\n\
    return c\n\
}\n",
    );
    let add = find(&r, "add");
    assert!(matches!(add.big_o, BigO::Constant), "got {:?}", add.big_o);
}

#[test]
fn nested_if_remains_constant() {
    let r = analyze(
        "\
function classify(integer x) {\n\
    if (x < 0) { return -1 }\n\
    else if (x == 0) { return 0 }\n\
    else { return 1 }\n\
}\n",
    );
    let c = find(&r, "classify");
    assert!(matches!(c.big_o, BigO::Constant), "got {:?}", c.big_o);
}

// ─── slice 2: loops ────────────────────────────────────────────────

#[test]
fn for_loop_over_count_is_linear() {
    let r = analyze(
        "\
function sum(arr) {\n\
    var total = 0\n\
    for (var i = 0; i < count(arr); i++) {\n\
        total = total + arr[i]\n\
    }\n\
    return total\n\
}\n",
    );
    let sum = find(&r, "sum");
    match &sum.big_o {
        BigO::Linear(v) => assert_eq!(v.name, "arr"),
        other => panic!("expected O(arr), got {other:?}\nformula = {}", sum.formula),
    }
}

#[test]
fn foreach_is_linear() {
    let r = analyze(
        "\
function totalize(arr) {\n\
    var t = 0\n\
    for (var x in arr) {\n\
        t = t + x\n\
    }\n\
    return t\n\
}\n",
    );
    let f = find(&r, "totalize");
    match &f.big_o {
        BigO::Linear(v) => assert_eq!(v.name, "arr"),
        other => panic!("expected O(arr), got {other:?}\nformula = {}", f.formula),
    }
}

#[test]
fn for_loop_with_integer_param_bound_is_linear_in_n() {
    let r = analyze(
        "\
function repeat(integer n) {\n\
    var x = 0\n\
    for (var i = 0; i < n; i++) {\n\
        x = x + 1\n\
    }\n\
    return x\n\
}\n",
    );
    let f = find(&r, "repeat");
    match &f.big_o {
        BigO::Linear(v) => assert_eq!(v.name, "n"),
        other => panic!("expected O(n), got {other:?}\nformula = {}", f.formula),
    }
}

#[test]
fn nested_foreach_is_quadratic() {
    let r = analyze(
        "\
function cross(arr) {\n\
    var k = 0\n\
    for (var a in arr) {\n\
        for (var b in arr) {\n\
            k = k + 1\n\
        }\n\
    }\n\
    return k\n\
}\n",
    );
    let f = find(&r, "cross");
    match &f.big_o {
        BigO::Quadratic(v) => assert_eq!(v.name, "arr"),
        other => panic!("expected O(arr²), got {other:?}\nformula = {}", f.formula,),
    }
}

#[test]
fn two_independent_loops_over_same_array_remain_linear() {
    let r = analyze(
        "\
function twopass(arr) {\n\
    var a = 0\n\
    for (var x in arr) {\n\
        a = a + 1\n\
    }\n\
    var b = 0\n\
    for (var y in arr) {\n\
        b = b + 2\n\
    }\n\
    return a + b\n\
}\n",
    );
    let f = find(&r, "twopass");
    match &f.big_o {
        BigO::Linear(v) => assert_eq!(v.name, "arr"),
        other => panic!("expected O(arr), got {other:?}\nformula = {}", f.formula),
    }
}

#[test]
fn nested_loops_over_two_distinct_arrays_yield_n_times_m() {
    let r = analyze(
        "\
function pair(a, b) {\n\
    var count = 0\n\
    for (var x in a) {\n\
        for (var y in b) {\n\
            count = count + 1\n\
        }\n\
    }\n\
    return count\n\
}\n",
    );
    let f = find(&r, "pair");
    let label = f.big_o.render();
    assert!(label.contains('a'), "got: {label}\nformula = {}", f.formula);
    assert!(label.contains('b'), "got: {label}");
    // Should NOT be a single-var quadratic.
    assert!(
        !matches!(f.big_o, BigO::Quadratic(_)),
        "expected mixed product, got {:?}",
        f.big_o,
    );
}

#[test]
fn binary_search_style_loop_is_log_n() {
    let r = analyze(
        "\
function findish(integer n) {\n\
    var i = 1\n\
    while (i < n) {\n\
        i *= 2\n\
    }\n\
    return i\n\
}\n",
    );
    let f = find(&r, "findish");
    match &f.big_o {
        BigO::Log(v) => assert_eq!(v.name, "n"),
        other => panic!("expected O(log n), got {other:?}\nformula = {}", f.formula),
    }
}

// ─── builtin growth table ──────────────────────────────────────────

#[test]
fn calling_sort_on_a_parameter_array_is_n_log_n() {
    let r = analyze(
        "\
function ordered(arr) {\n\
    sort(arr)\n\
    return arr\n\
}\n",
    );
    let f = find(&r, "ordered");
    // The builtin contributes n · log n. Even with the constant
    // overhead it should land at n log n.
    match &f.big_o {
        BigO::NLogN(v) => assert_eq!(v.name, "arr"),
        other => panic!(
            "expected O(arr · log arr), got {other:?}\nformula = {}",
            f.formula,
        ),
    }
}

#[test]
fn concat_of_two_parameter_arrays_is_n_plus_m() {
    let r = analyze(
        "\
function combine(a, b) {\n\
    return concat(a, b)\n\
}\n",
    );
    let f = find(&r, "combine");
    // We don't have a "n + m" big-O class — both vars degree 1 →
    // Polynomial with degrees {a:1, b:1, ...}. Or, if dominant
    // happens to be just one of them, Linear. Either way the
    // formula mentions both.
    let label = f.big_o.render();
    assert!(label.contains('a') || label.contains('b'), "got: {label}");
}

#[test]
fn arrayintersect_is_quadratic_in_two_vars() {
    let r = analyze(
        "\
function common(a, b) {\n\
    return arrayIntersect(a, b)\n\
}\n",
    );
    let f = find(&r, "common");
    let label = f.big_o.render();
    // a · b appears in the big-O — the dominant term is the
    // product, not the sum.
    assert!(label.contains('a'), "got: {label}");
    assert!(label.contains('b'), "got: {label}");
    assert!(label.contains("·") || label.contains('*'), "got: {label}");
}

// ─── user-call boundary ────────────────────────────────────────────

#[test]
fn calling_another_constant_function_stays_constant() {
    let r = analyze(
        "\
function helper() { return 1 }\n\
function outer() {\n\
    return helper() + helper()\n\
}\n",
    );
    let f = find(&r, "outer");
    // Both callees are O(1) — outer should also be O(1) after
    // call-graph substitution.
    assert!(matches!(f.big_o, BigO::Constant), "got {:?}", f.big_o);
}

#[test]
fn caller_inherits_callees_complexity() {
    // outer calls a function that's O(n) on its first arg.
    let r = analyze(
        "\
function sum(arr) {\n\
    var t = 0\n\
    for (var x in arr) { t = t + x }\n\
    return t\n\
}\n\
function outer(data) {\n\
    return sum(data)\n\
}\n",
    );
    let f = find(&r, "outer");
    match &f.big_o {
        BigO::Linear(v) => assert_eq!(v.name, "data"),
        other => panic!("expected O(data), got {other:?}\nformula = {}", f.formula,),
    }
}

#[test]
fn caller_wraps_callee_in_loop_for_extra_factor() {
    // outer calls a linear callee inside its own loop → O(n²).
    let r = analyze(
        "\
function inner(arr) {\n\
    for (var x in arr) {}\n\
    return 0\n\
}\n\
function outer(data) {\n\
    for (var y in data) {\n\
        inner(data)\n\
    }\n\
    return 0\n\
}\n",
    );
    let f = find(&r, "outer");
    match &f.big_o {
        BigO::Quadratic(v) => assert_eq!(v.name, "data"),
        other => panic!("expected O(data²), got {other:?}\nformula = {}", f.formula,),
    }
}

#[test]
fn recursive_function_remains_unknown() {
    let r = analyze(
        "\
function fact(integer n) {\n\
    if (n <= 1) { return 1 }\n\
    return n * fact(n - 1)\n\
}\n",
    );
    let f = find(&r, "fact");
    assert!(matches!(f.big_o, BigO::Unknown), "got {:?}", f.big_o);
}

#[test]
fn mutual_recursion_is_unknown_for_both() {
    let r = analyze(
        "\
function odd(integer n) {\n\
    if (n == 0) { return false }\n\
    return even(n - 1)\n\
}\n\
function even(integer n) {\n\
    if (n == 0) { return true }\n\
    return odd(n - 1)\n\
}\n",
    );
    assert!(matches!(find(&r, "odd").big_o, BigO::Unknown));
    assert!(matches!(find(&r, "even").big_o, BigO::Unknown));
}

// ─── HOF lambda analysis ───────────────────────────────────────────

#[test]
fn array_map_with_constant_lambda_is_linear() {
    let r = analyze(
        "\
function doubled(arr) {\n\
    return arrayMap(arr, x -> x * 2)\n\
}\n",
    );
    let f = find(&r, "doubled");
    match &f.big_o {
        BigO::Linear(v) => assert_eq!(v.name, "arr"),
        other => panic!("expected O(arr), got {other:?}\nformula = {}", f.formula,),
    }
}

#[test]
fn array_map_with_inner_loop_lambda_is_quadratic_in_arr() {
    // arrayMap(arr, x -> sum_of(arr, x))  — but the lambda body
    // itself does a foreach on the outer array. That's O(arr) per
    // element × O(arr) elements = O(arr²).
    let r = analyze(
        "\
function compute(arr) {\n\
    return arrayMap(arr, x -> {\n\
        var t = 0\n\
        for (var y in arr) { t = t + y }\n\
        return t\n\
    })\n\
}\n",
    );
    let f = find(&r, "compute");
    match &f.big_o {
        BigO::Quadratic(v) => assert_eq!(v.name, "arr"),
        other => panic!("expected O(arr²), got {other:?}\nformula = {}", f.formula,),
    }
}

#[test]
fn array_filter_with_constant_predicate_is_linear() {
    let r = analyze(
        "\
function evens(arr) {\n\
    return arrayFilter(arr, x -> x % 2 == 0)\n\
}\n",
    );
    let f = find(&r, "evens");
    match &f.big_o {
        BigO::Linear(v) => assert_eq!(v.name, "arr"),
        other => panic!("expected O(arr), got {other:?}\nformula = {}", f.formula,),
    }
}

#[test]
fn array_reduce_with_constant_reducer_is_linear() {
    let r = analyze(
        "\
function total(arr) {\n\
    return arrayReduce(arr, function(acc, x) { return acc + x }, 0)\n\
}\n",
    );
    let f = find(&r, "total");
    match &f.big_o {
        BigO::Linear(v) => assert_eq!(v.name, "arr"),
        other => panic!("expected O(arr), got {other:?}\nformula = {}", f.formula,),
    }
}

#[test]
fn non_recursive_caller_of_recursive_callee_is_unknown_too() {
    // outer calls fact; fact is recursive. outer inherits Unknown.
    let r = analyze(
        "\
function fact(integer n) {\n\
    if (n <= 1) { return 1 }\n\
    return n * fact(n - 1)\n\
}\n\
function outer(integer k) {\n\
    return fact(k)\n\
}\n",
    );
    assert!(matches!(find(&r, "outer").big_o, BigO::Unknown));
}

// ─── formula sanity ────────────────────────────────────────────────

#[test]
fn formula_mentions_the_loop_size_variable() {
    let r = analyze(
        "\
function go(arr) {\n\
    for (var x in arr) {}\n\
    return 1\n\
}\n",
    );
    let f = find(&r, "go");
    let formula = f.formula.render();
    assert!(formula.contains("arr"), "formula = {formula}");
}

#[test]
fn main_block_is_analysed_even_without_a_function() {
    let r = analyze(
        "\
var x = 1\n\
var y = 2\n\
return x + y\n",
    );
    let main = find(&r, "<main>");
    assert!(matches!(main.big_o, BigO::Constant));
}
