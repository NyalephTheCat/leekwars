//! In v1, function args are passed by value (deep clone). An `@x` by-ref param
//! mutated *in place* (`push(x,…)`, `x[i]=…`) still propagates to the caller —
//! a direct call suppresses the by-ref arg's deep-clone (shared backing store),
//! and an indirect call threads the arg's shared cell. A *reassigned* by-ref
//! param in a directly-called function still needs a real cell the direct-call
//! path doesn't thread, so it skips. skip-don't-miscompile: correct value OR skip.

use leek_backend_native::{NativeOptions, run};
use leek_hir::lower_file_versioned;
use leek_parser::{ast::AstNode, ast::SourceFile, parse};
use leek_span::SourceId;
use leek_syntax::{SyntaxNode, Version};

fn native_v(src: &str, v: u8) -> String {
    let source = SourceId::new(1).unwrap();
    let ver = match v {
        1 => Version::V1,
        2 => Version::V2,
        3 => Version::V3,
        _ => Version::V4,
    };
    let parsed = parse(src, source, ver);
    let sf = SourceFile::cast(SyntaxNode::new_root(parsed.green)).unwrap();
    let hir = lower_file_versioned(&sf, source, v).0;
    leek_runtime::DISPLAY_VERSION.with(|c| c.set(v));
    let opts = NativeOptions::debug().with_lang(v, false);
    match run(&hir, &opts) {
        Ok(x) => x.to_string(),
        Err(e) => format!("ERR: {e}"),
    }
}

#[test]
fn v1_inplace_byref_propagates_or_skips() {
    // correct value cases (in-place mutation propagates).
    let pass: &[(&str, &str)] = &[
        (
            "function f(@x) { push(x, 12) } var a = [] f(a) return a",
            "[12]",
        ),
        (
            "function f(x) { push(x, 12) } function g(@x) { push(x, 12) } var a = [] var b = [] f(a) g(b) return [a, b]",
            "[[], [12]]",
        ),
        (
            "function f(@x) { x[0] = 9 } var a = [1, 2] f(a) return a",
            "[9, 2]",
        ),
        // a by-value param still clones (no propagation).
        (
            "function f(x) { push(x, 12) } var a = [] f(a) return a",
            "[]",
        ),
        // `@x` in a function taken as a value, invoked indirectly: the indirect
        // dispatch now threads the arg cell for `@x` (and v1-clones the by-value
        // `f`'s arg), so the in-place mutation propagates.
        (
            "function f(x) { push(x, 12) } function g(@x) { push(x, 12) } var a = [] var b = [] var t = [f, g]; t[0](a) t[1](b) return [a, b]",
            "[[], [12]]",
        ),
        // reassignment of the binding (`x = [9]`) is now cell-threaded: the
        // caller passes its cell and the rebind propagates back.
        (
            "function f(@x) { x = [9] } var a = [1] f(a) return a",
            "[9]",
        ),
    ];
    for (i, (src, expected)) in pass.iter().enumerate() {
        let n = native_v(src, 1);
        let ok = n == *expected || n.starts_with("ERR");
        eprintln!(
            "pass {i}: native={n:?} expected={expected:?} {}",
            if ok { "OK" } else { "MISCOMPILE" }
        );
        assert!(
            ok,
            "MISCOMPILE pass {i}: native={n} expected={expected}\n  {src}"
        );
    }
    // must-skip cases (need a real cell the threading can't supply; never
    // miscompile). A reassigned `@x` on a *method* goes through `Callee::Method`,
    // which the cell-threading (plain `Callee::Function` only) doesn't handle.
    let skip: &[&str] =
        &["class A { m(@x) { x = [9] } } var o = new A() var a = [1] o.m(a) return a"];
    for (i, src) in skip.iter().enumerate() {
        let n = native_v(src, 1);
        assert!(
            n.starts_with("ERR"),
            "expected skip for skip {i}, got {n}\n  {src}"
        );
    }
}
