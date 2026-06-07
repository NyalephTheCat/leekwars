//! Cross-function `Value::Cell` threading for `@x` by-reference parameters
//! (v2+). A reassigned (or aliased-onward) by-ref param now propagates the
//! rebinding back to the caller via a shared cell. The invariant under test is
//! skip-don't-miscompile: native must produce the correct value *or* skip
//! (`ERR`), never a wrong value.

use leek_backend_native::{run, NativeOptions};
use leek_hir::lower_file_versioned;
use leek_parser::{ast::AstNode, ast::SourceFile, parse};
use leek_span::SourceId;
use leek_syntax::{SyntaxNode, Version};

fn native_v(src: &str, v: u8) -> String {
    let source = SourceId::new(1).unwrap();
    let ver = match v { 1=>Version::V1,2=>Version::V2,3=>Version::V3,_=>Version::V4 };
    let parsed = parse(src, source, ver);
    let sf = SourceFile::cast(SyntaxNode::new_root(parsed.green)).unwrap();
    let hir = lower_file_versioned(&sf, source, v).0;
    leek_runtime::DISPLAY_VERSION.with(|c| c.set(v));
    let opts = NativeOptions::debug().with_lang(v, false);
    match run(&hir, &opts) { Ok(x)=>x.to_string(), Err(e)=>format!("ERR: {e}") }
}

#[test]
fn byref_reassignment_correct_or_skip() {
    // native must equal expected OR skip (ERR) — never a wrong value.
    let cases: &[(&str, &str)] = &[
        ("function f(@x) { x = 2 } var a = 1 f(a) return a", "2"),
        ("function f(@x) { x = x + 10 } var a = 5 f(a) return a", "15"),
        ("function swap(@a, @b) { var t = a a = b b = t } var x = 1 var y = 2 swap(x, y) return [x, y]", "[2, 1]"),
        ("function f(@arr) { arr = [9, 9] } var a = [1, 2] f(a) return a", "[9, 9]"),
        ("function inc(@n) { n = n + 1 } var c = 0 inc(c) inc(c) inc(c) return c", "3"),
        ("function r(@x) { return x + 1 } var a = 7 return r(a)", "8"),
        ("function g(@y) { y = 99 } function f(@x) { g(x) } var a = 1 f(a) return a", "99"),
        // conditional reassignment
        ("function f(@x) { if (x > 0) { x = 100 } } var a = 5 f(a) return a", "100"),
        ("function f(@x) { if (x > 0) { x = 100 } } var a = -5 f(a) return a", "-5"),
        // reassign twice across two calls
        ("function f(@x) { x = x * 2 } var a = 3 f(a) f(a) return a", "12"),
        // in-place mutate THEN reassign (reassign wins)
        ("function f(@arr) { push(arr, 9) arr = [0] } var a = [1] f(a) return a", "[0]"),
        // by-ref to array, mutate in place only (no reassign) — should still work
        ("function f(@arr) { push(arr, 9) } var a = [1, 2] f(a) return a", "[1, 2, 9]"),
        // deep chain f->g->h, h reassigns
        ("function h(@z) { z = 7 } function g(@y) { h(y) } function f(@x) { g(x) } var a = 0 f(a) return a", "7"),
    ];
    for (i, (src, expected)) in cases.iter().enumerate() {
        let n = native_v(src, 4);
        let ok = n == *expected || n.starts_with("ERR");
        eprintln!("case {i}: native={n:?} expected={expected:?} {}  {src}", if ok {"OK"} else {"MISCOMPILE"});
        assert!(ok, "MISCOMPILE case {i}: native={n} expected={expected}\n  {src}");
    }
}

#[test]
fn hof_callback_byref_correct_or_skip() {
    // arrayMap/arrayFilter/arrayIter with a `@v` by-ref callback param mutate
    // the source array in place. Tested at v2 (callback args are
    // `(index, value)`, so `@v` is the value). native must equal expected OR
    // skip (ERR), never wrong.
    let cases: &[(&str, &str)] = &[
        // 1-arg @v: double each element in place
        ("var t = [1, 2, 3] arrayMap(t, function(@v) { v = v * 2 }) return t", "[2, 4, 6]"),
        // 2-arg (index, @value): set every value to 10
        ("var t = [1, 2, 3] arrayFilter(t, function(k, @v) { v = 10 return true }) return t", "[10, 10, 10]"),
        // arrayIter: write the index into each value
        ("var t = [5, 6, 7] arrayIter(t, function(k, @v) { v = k }) return t", "[0, 1, 2]"),
        // read-only callback must be unaffected (no mutation)
        ("var t = [1, 2, 3] var s = arrayMap(t, function(v) { return v + 1 }) return [t, s]", "[[1, 2, 3], [2, 3, 4]]"),
    ];
    for (i, (src, expected)) in cases.iter().enumerate() {
        let n = native_v(src, 2);
        let ok = n == *expected || n.starts_with("ERR");
        eprintln!("hof {i}: native={n:?} expected={expected:?} {}  {src}", if ok {"OK"} else {"MISCOMPILE"});
        assert!(ok, "MISCOMPILE hof {i}: native={n} expected={expected}\n  {src}");
    }
}

#[test]
fn hof_callback_byref_v1_correct_or_skip() {
    // The corpus by-ref/HOF cases are v1. The HOF machinery lives in the shared
    // runtime (handled by the interpreter at v1), so native must match it (or
    // skip) — never a wrong value.
    let cases: &[(&str, &str)] = &[
        ("var t = [1,2,3] arrayFilter(t, function(k, @v) { v = 4 return true }) return t", "[4, 4, 4]"),
        // the exact upstream IIFE shape
        ("return function() { var t = ['a', 'b', 'c', 'd']; arrayFilter(t, function(k, @v) { v = 4; return k == 3; }); return t; }();", "[4, 4, 4, 4]"),
        ("var t = [10, 20, 30] arrayMap(t, function(k, @v) { v = v + 1 }) return t", "[11, 21, 31]"),
    ];
    for (i, (src, expected)) in cases.iter().enumerate() {
        let n = native_v(src, 1);
        let ok = n == *expected || n.starts_with("ERR");
        eprintln!("hofv1 {i}: native={n:?} expected={expected:?} {}  {src}", if ok {"OK"} else {"MISCOMPILE"});
        assert!(ok, "MISCOMPILE hofv1 {i}: native={n} expected={expected}\n  {src}");
    }
}
