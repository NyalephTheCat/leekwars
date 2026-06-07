//! Native op-counting matches the interpreter's charge model (the basis for
//! native verifying `.ops(N)` / `.equalsOps` corpus cases). These pin the
//! per-construct charges so the model can't silently drift.

use leek_backend_native::{ops_used, run, NativeOptions};
use leek_parser::{ast::AstNode, parse};
use leek_span::SourceId;
use leek_syntax::{SyntaxNode, Version};

fn ops(src: &str) -> u64 {
    let s = SourceId::new(1).unwrap();
    let p = parse(src, s, Version::V4);
    let sf = leek_parser::ast::SourceFile::cast(SyntaxNode::new_root(p.green)).expect("parse");
    let (h, _) = leek_hir::lower_file_versioned(&sf, s, 4);
    leek_runtime::DISPLAY_VERSION.with(|c| c.set(4));
    run(&h, &NativeOptions::release().with_lang(4, false)).expect("run");
    ops_used()
}

#[test]
fn op_counts_match_the_interpreter_charge_model() {
    // A bare constant is free; an assignment is a static `leek-charge` op.
    assert_eq!(ops("return 1"), 0);
    assert_eq!(ops("var x = 42 return x"), 1);
    // Binary op costs (binary_op_cost): add/sub 1, mul 2, div/mod 5.
    assert_eq!(ops("return 1 + 1"), 1);
    assert_eq!(ops("return 2 * 2"), 2);
    assert_eq!(ops("return 2 / 2"), 5);
    assert_eq!(ops("return 2 \\ 2"), 5);
    // A conditional branch costs 1.
    assert_eq!(ops("if (1) {} return 0"), 1);
    assert_eq!(ops("return 1 and 2"), 1);
    // Interval literal = 2; set literal = 2 per element.
    assert_eq!(ops("return [1..2]"), 2);
    assert_eq!(ops("return <1, 2, 3, 4>"), 8);
    // String concat: Add (1) + result chars; with the var-decl static op.
    assert_eq!(ops("var s = \"a\" + \"b\" return s"), 4);
    // A builtin's runtime cost (interval probe builtin = interval 2 + cost 1).
    assert_eq!(ops("return intervalMin([1..2])"), 3);
}

#[test]
fn op_budget_stops_a_runaway_loop() {
    // With a finite budget, an unbounded loop trips TOO_MUCH_OPERATIONS instead
    // of spinning forever (the runtime-error verification path).
    let s = SourceId::new(1).unwrap();
    let src = "var a = 0 for (var i = 0; i < 100000000; ++i) a = a + 1 return a";
    let p = parse(src, s, Version::V4);
    let sf = leek_parser::ast::SourceFile::cast(SyntaxNode::new_root(p.green)).unwrap();
    let (h, _) = leek_hir::lower_file_versioned(&sf, s, 4);
    let out = run(&h, &NativeOptions::release().with_lang(4, false).with_op_limit(10_000));
    match out {
        Err(leek_backend_native::NativeError::Runtime(c)) => {
            assert_eq!(c, "TOO_MUCH_OPERATIONS");
        }
        other => panic!("expected a runtime op-budget trip, got {other:?}"),
    }
}
