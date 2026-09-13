//! Nested index assignment semantics: every sub-expression of the l-value
//! is evaluated exactly once (#49), and v1-v3 LegacyArray promotion still
//! propagates through every nesting level.

use leek_backend_native::{NativeOptions, run};
use leek_parser::{ast::AstNode, parse};
use leek_span::SourceId;
use leek_syntax::{SyntaxNode, Version};

fn eval(src: &str, version: u8) -> String {
    let source = SourceId::new(1).unwrap();
    let syntax_version = match version {
        1 => Version::V1,
        2 => Version::V2,
        3 => Version::V3,
        _ => Version::V4,
    };
    let parsed = parse(src, source, syntax_version);
    let sf = leek_parser::ast::SourceFile::cast(SyntaxNode::new_root(parsed.green)).expect("parse");
    let (hir, _) = leek_hir::lower_file_versioned(&sf, source, version);
    leek_runtime::DISPLAY_VERSION.with(|c| c.set(version));
    let result = run(&hir, &NativeOptions::release().with_lang(version, false)).expect("run");
    format!("{result:?}")
}

#[test]
fn nested_index_side_effects_run_once() {
    // `i++` runs once and only row 0 is written: 100 * i + 10 * g[0][1] + g[1][1].
    let src = "var g = [[0, 0], [0, 0]] var i = 0 g[i++][1] = 5 \
               return 100 * i + 10 * g[0][1] + g[1][1]";
    for v in 1..=4 {
        assert_eq!(eval(src, v), "Int(150)", "v{v}");
    }
}

#[test]
fn nested_index_calls_run_once() {
    let src = "global n = 0 function f() { n++ return 0 } \
               var a = [[0, 0]] a[f()][1] = 5 return n * 10 + a[0][1]";
    for v in 1..=4 {
        assert_eq!(eval(src, v), "Int(15)", "v{v}");
    }
}

#[test]
fn rhs_reassigning_the_outer_index_does_not_redirect_the_writeback() {
    // The write-back targets the slot that was read (`g[0]`), not the one
    // `i` names after the RHS ran.
    let src = "var g = [[0, 0], [7, 7]] var i = 0 g[i][1] = (i = 1) \
               return [g[0][1], g[1][0], g[1][1]]";
    for v in 1..=4 {
        let out = eval(src, v);
        assert!(out.contains("[Int(1), Int(7), Int(7)]"), "v{v}: {out}");
    }
}

#[test]
fn legacy_promotion_propagates_through_three_levels() {
    // v1-v3: an out-of-range write promotes the innermost array to a map,
    // and the write-back must carry it up to `a[0][0]`.
    for v in 1..=3 {
        assert_eq!(
            eval("var a = [[[]]] a[0][0][5] = 1 return a[0][0][5]", v),
            "Int(1)",
            "v{v}"
        );
    }
}
