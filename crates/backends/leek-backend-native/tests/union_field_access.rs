//! A union-typed local (`Cell | integer x`) lowers to `Type::Any`, so
//! inference would otherwise narrow it to a scalar from its only assignment
//! (`x = 3` → `Int`). A `instanceof`-guarded `x.field` access in the *other*
//! arm still has to compile, though — so such locals are promoted to a boxed
//! `Ref` and the field read goes through the dynamic path. The invariant under
//! test is skip-don't-miscompile: native must produce the correct value *or*
//! skip (`ERR`), never a wrong value.

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
fn union_instanceof_field_correct_or_skip() {
    // The two upstream cases are v4. native must equal expected OR skip.
    let cases: &[(&str, &str)] = &[
        // instanceof is false (x is an int) → else branch, abs(3) = 3. The
        // dead `x.id` in the then-arm must still compile.
        (
            "class Cell { integer id = 5 } Cell | integer x = 3; if (x instanceof Cell) { return x.id } else { return abs(x) }",
            "3",
        ),
        // instanceof false → else branch, add(10, 5) = 15.
        (
            "class A { integer v = 0 } A | integer x = 10; function add(integer a, integer b) { return a + b } if (x instanceof A) { return x.v } else { return add(x, 5) }",
            "15",
        ),
        // instanceof TRUE (x really is a Cell) → then branch, x.id = 7.
        (
            "class Cell { integer id = 7 } Cell | integer x = new Cell(); if (x instanceof Cell) { return x.id } else { return abs(x) }",
            "7",
        ),
    ];
    for (i, (src, expected)) in cases.iter().enumerate() {
        let n = native_v(src, 4);
        let ok = n == *expected || n.starts_with("ERR");
        eprintln!(
            "case {i}: native={n:?} expected={expected:?} {}  {src}",
            if ok { "OK" } else { "MISCOMPILE" }
        );
        assert!(
            ok,
            "MISCOMPILE case {i}: native={n} expected={expected}\n  {src}"
        );
    }
}
