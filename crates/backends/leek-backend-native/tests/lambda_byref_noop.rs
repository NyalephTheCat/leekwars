//! In v2+, a *lambda* value's `@x` by-reference parameter is a no-op — the
//! mutation does NOT propagate to the caller (only *named* functions propagate,
//! via cross-function cells). So a pure-local lambda `@x` (only read /
//! self-reassigned, never a HOF callback) compiles as a plain by-value param.
//! v1 DOES propagate, so it must keep skipping. skip-don't-miscompile: correct
//! value OR skip (ERR), never wrong.

use leek_backend_native::{run, NativeOptions};
use leek_hir::lower_file_versioned;
use leek_parser::{ast::AstNode, ast::SourceFile, parse};
use leek_span::SourceId;
use leek_syntax::{SyntaxNode, Version};

fn native_v(src: &str, v: u8) -> String {
    let source = SourceId::new(1).unwrap();
    let ver = match v { 1 => Version::V1, 2 => Version::V2, 3 => Version::V3, _ => Version::V4 };
    let parsed = parse(src, source, ver);
    let sf = SourceFile::cast(SyntaxNode::new_root(parsed.green)).unwrap();
    let hir = lower_file_versioned(&sf, source, v).0;
    leek_runtime::DISPLAY_VERSION.with(|c| c.set(v));
    let opts = NativeOptions::debug().with_lang(v, false);
    match run(&hir, &opts) { Ok(x) => x.to_string(), Err(e) => format!("ERR: {e}") }
}

#[test]
fn lambda_byref_noop_or_skip() {
    let inc = "var f = function(@a) { a++ } var b = 10 f(b) return b";
    let cases: &[(&str, &str, u8)] = &[
        (inc, "10", 2),  // v2: lambda @a is a no-op
        (inc, "10", 3),  // v3: same
        (inc, "11", 1),  // v1: propagates → native must SKIP (can't be 10)
        // reassigning the param is also a no-op.
        ("var f = function(@a) { a = 99 } var b = 10 f(b) return b", "10", 2),
        // a named function's @a is unaffected by this exemption.
        ("function f(@a) { a++ } var b = 10 f(b) return b", "11", 2),
    ];
    for (i, (src, expected, v)) in cases.iter().enumerate() {
        let n = native_v(src, *v);
        let ok = n == *expected || n.starts_with("ERR");
        eprintln!("case {i} (v{v}): native={n:?} expected={expected:?} {}", if ok { "OK" } else { "MISCOMPILE" });
        assert!(ok, "MISCOMPILE case {i}: native={n} expected={expected}\n  {src}");
    }
    // v1 DOES propagate (a `@a` by-ref on a lambda value), and native now
    // models it via cell-threading at the indirect call site → 11.
    let v1 = native_v(inc, 1);
    assert!(v1 == "11" || v1.starts_with("ERR"), "v1 lambda @x: got {v1}");
}
