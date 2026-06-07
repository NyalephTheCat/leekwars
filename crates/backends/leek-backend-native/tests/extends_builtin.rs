//! A class extending a builtin collection (`class A extends Array {}`) is
//! collapsed to the underlying builtin constructor — exactly as the
//! interpreter's `construct_user_class` does: `new A()` is a plain `[]`,
//! `push` works, and `instanceof A` is false. skip-don't-miscompile.

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
fn extends_builtin_collapses_or_skips() {
    let cases: &[(&str, &str, u8)] = &[
        ("class A extends Array {} return new A()", "[]", 4),
        ("class A extends Array {} var a = new A() push(a, 12) return a", "[12]", 4),
        ("class A extends Array {} var a = new A() push(a, 12) return a", "[12]", 2),
        // collapsed to a plain array → not an instance of A.
        ("class A extends Array {} var a = new A() if (a instanceof A) { return 1 } return 0", "0", 4),
        ("class M extends Map {} var m = new M() m['k'] = 5 return m['k']", "5", 4),
    ];
    for (i, (src, expected, v)) in cases.iter().enumerate() {
        let n = native_v(src, *v);
        let ok = n == *expected || n.starts_with("ERR");
        eprintln!("case {i} (v{v}): native={n:?} expected={expected:?} {}", if ok { "OK" } else { "MISCOMPILE" });
        assert!(ok, "MISCOMPILE case {i}: native={n} expected={expected}\n  {src}");
    }
}
