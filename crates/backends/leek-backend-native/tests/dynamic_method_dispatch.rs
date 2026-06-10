//! A method call on a receiver whose class isn't statically known — a captured
//! `this` inside a lambda (`m() { return (-> n(this))() }`), or an `expr as C`
//! cast value (`mapGet(…) as G; g.check()`) — dispatches at runtime on the
//! receiver's actual class via `leek_call_method` (instance method on the
//! runtime class, else builtin), mirroring the interpreter's
//! `dispatch_method_call`. skip-don't-miscompile: correct value OR skip (ERR).

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
fn dynamic_dispatch_or_skip() {
    let lam = "class A { x = 5 m() { return (-> n(this))() } n(p) { return p.x } } return A().m()";
    let cast = "class G { boolean check() { return true } } var m = ['a': new G()] var g = mapGet(m, 'a', new G()) as G if (g.check()) { return 1 } return 0";
    // a subclass override must dispatch on the RUNTIME class (virtual).
    let virt = "class A { x = 5 m() { return (-> n(this))() } n(p) { return p.x } } class B extends A { n(p) { return 99 } } return B().m()";
    let cases: &[(&str, &str, u8)] = &[
        (lam, "5", 4),
        (lam, "5", 3),
        (lam, "5", 2),
        (cast, "1", 4),
        (virt, "99", 4),
        // unknown receiver, name isn't a user method → builtin fallback → null.
        (
            "class A { x = 5 } var a = new A() return a.notAMethod()",
            "null",
            4,
        ),
    ];
    for (i, (src, expected, v)) in cases.iter().enumerate() {
        let n = native_v(src, *v);
        let ok = n == *expected || n.starts_with("ERR");
        eprintln!(
            "case {i} (v{v}): native={n:?} expected={expected:?} {}",
            if ok { "OK" } else { "MISCOMPILE" }
        );
        assert!(
            ok,
            "MISCOMPILE case {i}: native={n} expected={expected}\n  {src}"
        );
    }
}
