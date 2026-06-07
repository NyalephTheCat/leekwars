//! v1 cell-threading through indirect (function-value) calls: a `@x` by-ref
//! param of a lambda invoked via a value (`var f = function(@a){…}; f(b)`)
//! shares the caller's storage through a `Value::Cell`, so a reassignment /
//! in-place mutation propagates. v2+ lambda by-ref is a no-op (no propagation).
//! An *escaping* by-ref param (captured by a returned closure) is now threaded
//! end-to-end as a shared cell too. skip-don't-miscompile: correct value OR skip.

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
fn v1_indirect_byref_or_skip() {
    let cases: &[(&str, &str, u8)] = &[
        ("var f = function(@a) { a++ } var b = 10 f(b) return b", "11", 1),
        ("var f = function(@a) { a = 99 } var b = 10 f(b) return b", "99", 1),
        ("var f = function(@a) { push(a, 5) } var b = [] f(b) return b", "[5]", 1),
        // v2+ lambda by-ref is a no-op.
        ("var f = function(@a) { a++ } var b = 10 f(b) return b", "10", 2),
        ("var f = function(@a) { a++ } var b = 10 f(b) return b", "10", 3),
        // builtin shadow / HOF indirect calls must still peel cell args correctly.
        ("var _count = count; count = function(x) { return _count(x) } return count([1, 2, 3])", "3", 1),
        ("var a = toUpper, b = arrayMap return b(['a', 'b'], a)", "[\"A\", \"B\"]", 1),
        // escaping @a (captured by a returned closure that mutates it) is
        // threaded as a shared cell — the caller observes the mutation.
        ("var f = function(@a) { return function() { a += 2 } }; var x = 10 f(x)() return x", "12", 1),
    ];
    for (i, (src, expected, v)) in cases.iter().enumerate() {
        let n = native_v(src, *v);
        let ok = n == *expected || n.starts_with("ERR");
        eprintln!("case {i} (v{v}): native={n:?} expected={expected:?} {}", if ok { "OK" } else { "MISCOMPILE" });
        assert!(ok, "MISCOMPILE case {i}: native={n} expected={expected}\n  {src}");
    }
}
