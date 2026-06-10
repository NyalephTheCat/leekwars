//! A user class reference used as a *value* — passed to a higher-order builtin
//! (`arrayMap(a, A)`) or stored in an object slot that's later called
//! (`var o = {c: A}; o.c(x)`) — constructs the class through a synthetic
//! per-class constructor thunk registered in the runtime's call dispatch.
//! The invariant under test is skip-don't-miscompile: native must produce the
//! correct value *or* skip (`ERR`), never a wrong value. A class that can't be
//! constructed by the native `new` path (extends a builtin, overrides
//! `string()`, or v1's deep-clone semantics) gets no thunk and keeps skipping.

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
fn classref_as_value_constructs_or_skips() {
    // (src, expected, version). native must equal expected OR skip (ERR).
    let cases: &[(&str, &str, u8)] = &[
        // class ref as a HOF callback: each element constructs `A(element)`.
        (
            "class A { x constructor(x) { this.x = x } } var a = [1, 2, 3, 4] return arrayMap(a, A)",
            "[A {x: 1}, A {x: 2}, A {x: 3}, A {x: 4}]",
            4,
        ),
        // class ref stored in an object slot, then called.
        (
            "class A { x constructor(x) { this.x = x } } var f = A var o = {c: f} return o.c('a')",
            "A {x: \"a\"}",
            4,
        ),
        // works in v2/v3 too (reference semantics).
        (
            "class A { x constructor(x) { this.x = x } } var a = [1, 2, 3, 4] return arrayMap(a, A)",
            "[A {x: 1}, A {x: 2}, A {x: 3}, A {x: 4}]",
            3,
        ),
        (
            "class A { x constructor(x) { this.x = x } } var a = [1, 2, 3, 4] return arrayMap(a, A)",
            "[A {x: 1}, A {x: 2}, A {x: 3}, A {x: 4}]",
            2,
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

#[test]
fn unconstructible_classref_value_skips() {
    // A class native's `new` can't build gets no thunk → the use-as-value site
    // must SKIP (return ERR), never construct a wrong value.
    let cases: &[(&str, u8)] = &[
        (
            "class A extends Array {} var a = [1, 2] return arrayMap(a, A)",
            4,
        ),
        (
            "class A { x constructor(x){this.x=x} string(){return 'z'} } var a = [1] return arrayMap(a, A)",
            4,
        ),
        (
            "class A { x constructor(x) { this.x = x } } var a = [1, 2] return arrayMap(a, A)",
            1,
        ),
    ];
    for (i, (src, v)) in cases.iter().enumerate() {
        let n = native_v(src, *v);
        eprintln!("skip {i} (v{v}): native={n:?}");
        assert!(n.starts_with("ERR"), "expected skip, got {n}\n  {src}");
    }
}
