//! Native-vs-interpreter parity for `@`-by-ref escape constructs that the
//! native backend now supports beyond the corpus (the corpus has no such
//! cases, so the interpreter is the oracle). Each program is run through both
//! backends; the native result must EQUAL the interpreter's, or — for the
//! skip-don't-miscompile safety cases — native may report `Unsupported`. The
//! `@a`-return / escaping-capture cases assert strict equality (they're the
//! features being added); the currying by-value case is a miscompile guard.

use leek_backend_native::{run, NativeOptions};
use leek_hir::lower_file_versioned;
use leek_parser::{ast::AstNode, ast::SourceFile, parse};
use leek_span::SourceId;
use leek_syntax::{SyntaxNode, Version};

fn lower(src: &str, v: u8) -> leek_hir::HirFile {
    let source = SourceId::new(1).unwrap();
    let ver = match v {
        1 => Version::V1,
        2 => Version::V2,
        3 => Version::V3,
        _ => Version::V4,
    };
    let parsed = parse(src, source, ver);
    let sf = SourceFile::cast(SyntaxNode::new_root(parsed.green)).unwrap();
    lower_file_versioned(&sf, source, v).0
}

fn interp_v(src: &str, v: u8) -> String {
    let hir = lower(src, v);
    leek_backend_interp::value::DISPLAY_VERSION.with(|c| c.set(v));
    let r = leek_backend_interp::run_with_limit_version_strict(&hir, 100_000_000, v, false);
    match r.error {
        Some(e) => format!("ERR: {e}"),
        None => r.value.to_string(),
    }
}

fn native_v(src: &str, v: u8) -> String {
    let hir = lower(src, v);
    leek_backend_interp::value::DISPLAY_VERSION.with(|c| c.set(v));
    let opts = NativeOptions::debug().with_lang(v, false);
    match run(&hir, &opts) {
        Ok(x) => x.to_string(),
        Err(e) => format!("ERR: {e}"),
    }
}

/// Native must match the interpreter exactly (the feature is supported).
fn assert_parity(src: &str, v: u8) {
    let i = interp_v(src, v);
    let n = native_v(src, v);
    assert_eq!(n, i, "native/interp mismatch (v{v})\n  src: {src}\n  interp={i}  native={n}");
}

/// Native must match the interpreter OR skip (Unsupported) — never miscompile.
fn assert_parity_or_skip(src: &str, v: u8) {
    let i = interp_v(src, v);
    let n = native_v(src, v);
    assert!(
        n == i || n.starts_with("ERR"),
        "MISCOMPILE (v{v})\n  src: {src}\n  interp={i}  native={n}"
    );
}

#[test]
fn return_byref_param_aliases_caller() {
    // `return @a` of a by-ref ARRAY param: the caller's binding aliases the
    // argument, so a later in-place mutation propagates back.
    assert_parity(
        "function f(@a) { return @a } var x = [1] var y = f(x) push(y, 2) return x",
        1,
    );
    // Same through an indirect (lambda-value) call.
    assert_parity(
        "var f = function(@a) { return @a } var x = [1] var y = f(x) push(y, 2) return x",
        1,
    );
    // The returned alias itself reads back the mutated contents.
    assert_parity(
        "function f(@a) { return @a } var x = [1] var y = f(x) push(y, 2) return y",
        1,
    );
    // In-place mutation in the callee + return-alias compose.
    assert_parity(
        "function f(@a) { push(a, 9) return @a } var x = [1] var y = f(x) push(y, 2) return x",
        1,
    );
    // A scalar `@a` return: no aliasing to preserve, but must still be correct.
    assert_parity("function f(@a) { return @a } var x = 10 var y = f(x) return y", 1);
}

#[test]
fn reassigned_byref_param_propagates() {
    // Reassigning a `@x` by-ref param's binding propagates to the caller's
    // argument (v1 by-reference semantics) — threaded as a shared cell.
    assert_parity("function f(@x) { x = [9] } var a = [1] f(a) return a", 1);
    assert_parity("function f(@x) { x = 99 } var a = 10 f(a) return a", 1);
    // Reassign to a copy of another composite (v1 deep-clone of the RHS).
    assert_parity("function f(@x) { x = [1, 2, 3] } var a = [0] f(a) return a", 1);
    // Reassign then mutate in place.
    assert_parity("function f(@x) { x = [] push(x, 7) } var a = [1] f(a) return a", 1);
    // A reassigned `@a` indirectly-called lambda value.
    assert_parity("var f = function(@a) { a++ } var b = 10 f(b) return b", 1);
    // Mixed: one by-ref (propagates) + one by-value (cloned, no propagation).
    assert_parity(
        "function f(@x, y) { x = 5 push(y, 9) } var a = 1 var b = [] f(a, b) return [a, b]",
        1,
    );
}

#[test]
fn byref_cell_fn_taken_as_value() {
    // A by-ref cell-param function taken as a *value* (`var g = f`) and invoked
    // indirectly still threads the caller's cell for `@x` (the registered mask
    // drives `thread_args`), so a reassignment / in-place mutation propagates.
    assert_parity("function f(@x) { x = [9] } var g = f var a = [1] g(a) return a", 1);
    assert_parity("function f(@x) { x++ } var g = f var n = 5 g(n) return n", 1);
    assert_parity("function f(@x) { push(x, 9) } var g = f var a = [1] g(a) return a", 1);
    // Stored in an array, then invoked.
    assert_parity("function f(@x) { x = 7 } var t = [f] var n = 1 t[0](n) return n", 1);
}

#[test]
fn return_byvalue_param_does_not_alias() {
    // A by-VALUE param returned (`return @a` where `a` is NOT declared `@a`)
    // returns the v1 clone — no aliasing. Must match the interpreter.
    assert_parity_or_skip(
        "function f(a) { return @a } var x = [1] var y = f(x) push(y, 2) return x",
        1,
    );
    // Currying over a by-value param: the closure captures the *copy*, so the
    // caller's array is untouched (the bug the safety-net caught earlier).
    assert_parity(
        "function push_to(array) { return function(e) { push(array, e) } } var c = [] var g = push_to(c) g(1) return c",
        1,
    );
}

#[test]
fn method_byref_param_matches_interp() {
    // In v2+, a method's `@x` reassignment is a NO-OP for the caller (only a
    // top-level named function propagates `@x`) — interpreter-confirmed. Native
    // compiles it by-value (the rebind is local), so the caller is unchanged;
    // an in-place mutation still propagates through the shared `Rc`.
    for v in [2u8, 3, 4] {
        assert_parity("class A { m(@x) { x = [9] } } var o = new A() var a = [1] o.m(a) return a", v);
        assert_parity("class A { m(@x) { x = 99 } } var o = new A() var n = 5 o.m(n) return n", v);
        assert_parity("class A { m(@x) { push(x, 9) } } var o = new A() var a = [1] o.m(a) return a", v);
    }
    // v1 routes method dispatch through a different (unsupported) path, so it
    // skips — never miscompiles.
    assert_parity_or_skip("class A { m(@x) { x = 99 } } var o = new A() var n = 5 o.m(n) return n", 1);
}

#[test]
fn named_byref_reassign_propagates_all_versions() {
    // A *named* top-level function's `@x` reassignment propagates to the
    // caller's argument in EVERY version (unlike a method/lambda) — the
    // distinguishing case for the cell-threading.
    for v in [1u8, 2, 3, 4] {
        assert_parity("function f(@x) { x = 99 } var a = 5 f(a) return a", v);
        assert_parity("function f(@x) { x = [9] } var a = [1] f(a) return a", v);
    }
}

#[test]
fn escaping_byref_param_all_call_kinds() {
    for v in [2u8, 3, 4] {
        // A NAMED fn whose `@x` is captured by a returned lambda that mutates it
        // propagates to the caller in v2+ too (the caller's arg is cell-threaded
        // in every version, not just v1).
        assert_parity(
            "function m(@x) { return function() { x += 1 } } var n = 5 var f = m(n) f() return n",
            v,
        );
        // A METHOD's `@x` captured by a returned lambda is a NO-OP for the caller
        // (method `@x` is by-value in v2+) — interpreter-confirmed.
        assert_parity(
            "class A { m(@x) { return function() { x += 1 } } } var o = new A() var n = 5 var f = o.m(n) f() return n",
            v,
        );
        // A method that returns `@x` hands back the alias (the caller's argument).
        assert_parity(
            "class A { m(@x) { return @x } } var o = new A() var a = [1] var y = o.m(a) push(y, 2) return a",
            v,
        );
    }
    // v1 escaping `@x` via a returned lambda on a named fn (the original case).
    assert_parity(
        "function f(@a) { return function() { a += 2 } } var x = 10 f(x)() return x",
        1,
    );
}

#[test]
fn v1_user_classes_are_skipped_not_miscompiled() {
    // LeekScript v1 has no real user classes — the interpreter returns `null`
    // for instance construction / methods / fields (only static methods work).
    // Native must SKIP these (never miscompile); a skip is acceptable here.
    for src in [
        "class A { public x = 0 constructor(v) { this.x = v } get() { return this.x } } var a = new A(7) return a.get()",
        "class A { public x = 1 } var o = new A() o.x = 5 return o.x",
        "class A { m(@x) { x = 9 } } var o = new A() var n = 5 o.m(n) return n",
    ] {
        assert_parity_or_skip(src, 1);
    }
}
