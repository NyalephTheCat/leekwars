//! Reflection on a constructed class: `new Test()` binds omitted constructor
//! params to null (matching the interpreter), `x.class.fields` returns the
//! declared field names on a runtime class-reference value, and dynamic
//! `obj[field]` read/write copies fields by name. skip-don't-miscompile:
//! correct value OR skip (ERR), never wrong.

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
fn reflection_field_copy_or_skip() {
    let refl = "class Test { a b c constructor(a, b, c) { this.a = a this.b = b this.c = c } } var test1 = new Test(1, 2, 3) var test2 = new Test() for (var field in test1.class.fields) { test2[field] = test1[field] } return test2";
    let cases: &[(&str, &str, u8)] = &[
        (refl, "Test {a: 1, b: 2, c: 3}", 4),
        (refl, "Test {a: 1, b: 2, c: 3}", 3),
        // omitted untyped ctor params bind to null.
        ("class A { x constructor(x) { this.x = x } } return new A()", "A {x: null}", 4),
        // x.class.fields on a runtime class-ref value.
        ("class T { a b constructor(a,b){this.a=a this.b=b} } var t = new T(1,2) return t.class.fields", "[\"a\", \"b\"]", 4),
    ];
    for (i, (src, expected, v)) in cases.iter().enumerate() {
        let n = native_v(src, *v);
        let ok = n == *expected || n.starts_with("ERR");
        eprintln!("case {i} (v{v}): native={n:?} expected={expected:?} {}", if ok { "OK" } else { "MISCOMPILE" });
        assert!(ok, "MISCOMPILE case {i}: native={n} expected={expected}\n  {src}");
    }
}

#[test]
fn omitted_scalar_ctor_param_skips() {
    // A declared-scalar ctor param can't hold null, so under-arity still skips.
    let n = native_v("class A { integer x constructor(integer x) { this.x = x } } return new A()", 4);
    assert!(n.starts_with("ERR") || n == "A {x: 0}", "got {n}");
}
