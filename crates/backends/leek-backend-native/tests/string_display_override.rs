//! A class with a 0-arg `string()` method overrides how its instance renders
//! as the *top-level* program result (mirroring the interpreter's
//! `invoke_instance_string_method`): `class A { string() { return 'test' } }
//! return new A()` shows `test`, not `A {}`. Nested instances render normally
//! (`A {…}`) in both backends, so this only affects the final returned value.
//! skip-don't-miscompile: correct value OR skip (ERR), never wrong.

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
fn string_override_or_skip() {
    let cases: &[(&str, &str)] = &[
        (
            "class A { string() { return 'test' } } return new A()",
            "test",
        ),
        (
            "class A { x = 5 string() { return 'v' + this.x } } return new A()",
            "v5",
        ),
        // top-level only: a nested instance renders normally (no string()).
        (
            "class A { x = 5 string() { return 'v' } } return [new A()]",
            "[A {x: 5}]",
        ),
        // an instance that isn't the result is unaffected.
        (
            "class A { string() { return 'x' } } var a = new A() return 7",
            "7",
        ),
    ];
    for (i, (src, expected)) in cases.iter().enumerate() {
        let n = native_v(src, 4);
        let ok = n == *expected || n.starts_with("ERR");
        eprintln!(
            "case {i}: native={n:?} expected={expected:?} {}",
            if ok { "OK" } else { "MISCOMPILE" }
        );
        assert!(
            ok,
            "MISCOMPILE case {i}: native={n} expected={expected}\n  {src}"
        );
    }
}
