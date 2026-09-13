//! Native op-counting matches the interpreter's charge model (the basis for
//! native verifying `.ops(N)` / `.equalsOps` corpus cases). These pin the
//! per-construct charges so the model can't silently drift.

use leek_backend_native::{NativeOptions, ops_used, run};
use leek_parser::{ast::AstNode, parse};
use leek_span::SourceId;
use leek_syntax::{SyntaxNode, Version};

fn ops(src: &str) -> u64 {
    ops_v(src, 4)
}

fn ops_v(src: &str, version: u8) -> u64 {
    let s = SourceId::new(1).unwrap();
    let v = match version {
        1 => Version::V1,
        2 => Version::V2,
        3 => Version::V3,
        _ => Version::V4,
    };
    let p = parse(src, s, v);
    let sf = leek_parser::ast::SourceFile::cast(SyntaxNode::new_root(p.green)).expect("parse");
    let (h, _) = leek_hir::lower_file_versioned(&sf, s, version);
    leek_runtime::DISPLAY_VERSION.with(|c| c.set(version));
    run(&h, &NativeOptions::release().with_lang(version, false)).expect("run");
    ops_used()
}

#[test]
fn null_coalesce_charges_a_single_op() {
    // Upstream JVM op counts (tests/fixtures/ops/snapshot.tsv): `??` is one
    // operator op; the synthesized null comparison must not add another (#78).
    assert_eq!(ops("var f = integer? x => x ?? 0 return f(null)"), 3);
    assert_eq!(ops("var f = integer? x => x ?? 0 return f(7)"), 3);
}

#[test]
fn nested_index_write_charges_no_writeback() {
    // A nested write costs exactly what the same write through an explicit
    // alias does (minus the alias's `var` op): the promotion write-back is
    // uncharged, and skipped entirely in v4 (#79).
    for v in [1, 4] {
        let nested = ops_v("var t = [[1, 2]] t[0][1] = 3", v);
        let aliased = ops_v("var t = [[1, 2]] var m = t[0] m[1] = 3", v);
        assert_eq!(nested + 1, aliased, "v{v}");
    }
}

#[test]
fn nested_index_write_on_a_field_charges_the_field_once() {
    // `this.m[i][j] = v` charges the field access once; re-lowering the
    // chain used to charge it again (#49).
    let nested =
        ops("class A { m = [[0, 0]] f() { this.m[0][1] = 5 } } var a = new A() a.f() return a.m");
    let aliased = ops(
        "class A { m = [[0, 0]] f() { var t = this.m t[0][1] = 5 } } var a = new A() a.f() return a.m",
    );
    assert_eq!(nested + 1, aliased);
}

#[test]
fn op_counts_match_the_interpreter_charge_model() {
    // A bare constant is free; an assignment is a static `leek-charge` op.
    assert_eq!(ops("return 1"), 0);
    assert_eq!(ops("var x = 42 return x"), 1);
    // Binary op costs (BinOp::op_cost): add/sub 1, mul 2, div/mod 5.
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
    let out = run(
        &h,
        &NativeOptions::release()
            .with_lang(4, false)
            .with_op_limit(10_000),
    );
    match out {
        Err(leek_backend_native::NativeError::Runtime(c)) => {
            assert_eq!(c, "TOO_MUCH_OPERATIONS");
        }
        other => panic!("expected a runtime op-budget trip, got {other:?}"),
    }
}

/// Run `src` (v4) with a small op budget on a worker thread and return the
/// error code, failing the test (instead of hanging it) if the program is
/// still running after a generous deadline.
fn budget_error_within_deadline(src: &'static str) -> String {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let s = SourceId::new(1).unwrap();
        let p = parse(src, s, Version::V4);
        let sf = leek_parser::ast::SourceFile::cast(SyntaxNode::new_root(p.green)).unwrap();
        let (h, _) = leek_hir::lower_file_versioned(&sf, s, 4);
        let out = run(
            &h,
            &NativeOptions::release()
                .with_lang(4, false)
                .with_op_limit(10_000),
        );
        let _ = tx.send(match out {
            Err(leek_backend_native::NativeError::Runtime(c)) => c,
            Err(e) => format!("other error: {e}"),
            Ok(v) => format!("completed with {v}"),
        });
    });
    rx.recv_timeout(std::time::Duration::from_secs(60))
        .unwrap_or_else(|_| panic!("`{src}` never stopped: the op budget was not enforced"))
}

#[test]
fn op_budget_stops_goto_only_loops() {
    // No `Branch` terminator anywhere in these cycles — the header/step are
    // plain `Goto`s — so only a back-edge check on `Goto` can stop them.
    for src in [
        "var a = 0 for (;;) { a++ } return a",
        "for (;;) {} return 0",
        "var a = 0 for (;;) { a = a + 1 } return a",
    ] {
        assert_eq!(
            budget_error_within_deadline(src),
            "TOO_MUCH_OPERATIONS",
            "{src}"
        );
    }
}

#[test]
fn op_budget_stops_loops_whose_ops_are_charged_in_callees() {
    // The budget trips inside `f` (whose trap just returns its default); the
    // caller's loop must notice the recorded error at its own back-edge.
    for src in [
        "var n = 0 function f() { n = n + 1 } for (;;) { f() } return n",
        "function f() { var x = 1 return x } while (true) { f() } return 0",
        "function f() { var x = 1 return x } do { f() } while (true) return 0",
    ] {
        assert_eq!(
            budget_error_within_deadline(src),
            "TOO_MUCH_OPERATIONS",
            "{src}"
        );
    }
}
