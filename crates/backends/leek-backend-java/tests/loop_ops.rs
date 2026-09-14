//! Op accounting for loop bodies.
//!
//! Regression cover for #388: `while (true) {}` compiled to a Java loop
//! that charged nothing per iteration, so the per-turn op budget never
//! tripped and the fight ran at 100% CPU until the worker timeout.

use leek_backend_java::{Options, emit};
use leek_parser::{ast::AstNode, parse};
use leek_span::SourceId;
use leek_syntax::{SyntaxNode, Version};

fn java_for(src: &str, opts: &Options) -> String {
    let source = SourceId::new(1).unwrap();
    let parsed = parse(src, source, opts.version);
    let root = SyntaxNode::new_root(parsed.green);
    let sf = leek_parser::ast::SourceFile::cast(root).expect("parse");
    let (hir, _diags) = leek_hir::lower_file(&sf, source);
    emit(&hir, opts).java
}

/// The text between the `while (…) {` header and its closing brace.
fn while_body(java: &str) -> String {
    let (_, after) = java.split_once("while (").expect("a while loop");
    let (_, body) = after.split_once(") {").expect("a while header");
    let (body, _) = body.split_once('}').expect("a loop body");
    body.to_string()
}

/// Clean mode has no per-statement ticks: a block's static cost arrives as
/// the single `Stmt::Charge` the charge pass prepends, and that pass skips a
/// block costing nothing. An empty body must still charge its entry tick by
/// hand, or the loop is free and spins forever.
#[test]
fn clean_empty_while_true_still_charges() {
    let java = java_for(
        "// @version:4\nwhile (true) {}\n",
        &Options::clean(Version::V4, 1),
    );
    assert!(
        while_body(&java).contains("ops("),
        "empty clean-mode loop body charges nothing:\n{java}"
    );
}

/// Same hole in the other two loop emitters that route through
/// `emit_body_with_entry_tick`.
#[test]
fn clean_empty_do_while_and_for_still_charge() {
    for (src, header) in [
        ("// @version:4\ndo {} while (true)\n", "do {"),
        ("// @version:4\nfor (;;) {}\n", "; ) {"),
    ] {
        let java = java_for(src, &Options::clean(Version::V4, 1));
        let (_, body) = java.split_once(header).expect("a loop header");
        let (body, _) = body.split_once('}').expect("a loop body");
        assert!(
            body.contains("ops("),
            "empty clean-mode loop body charges nothing for {src:?}:\n{java}"
        );
    }
}

/// A clean-mode body that holds *any* statement already charges through the
/// charge pass, so the hand-emitted tick must not double up there.
#[test]
fn clean_nonempty_while_body_charges_once() {
    let java = java_for(
        "// @version:4\nwhile (true) { var x }\n",
        &Options::clean(Version::V4, 1),
    );
    let body = while_body(&java);
    assert_eq!(
        body.matches("ops(").count(),
        1,
        "expected exactly one charge in the body:\n{java}"
    );
}

/// Clean mode keeps the bare `true` condition on purpose: `is_infinite_loop`
/// reads javac's own rule off it to drop the (then unreachable) trailing
/// `return null;`. Wrapping the literal in `bool(...)` the way exact mode does
/// would bring `missing return statement` back.
#[test]
fn clean_while_true_keeps_a_javac_visible_constant_condition() {
    let java = java_for(
        "// @version:4\nwhile (true) {}\n",
        &Options::clean(Version::V4, 1),
    );
    assert!(java.contains("while (true) {"), "{java}");
    assert!(
        !java.contains("return null;"),
        "a return after a provably-infinite loop is unreachable:\n{java}"
    );
}

/// Characterization: exact mode already ticks an empty body and wraps a
/// literal condition in `bool(...)` so javac cannot fold the loop away. This
/// locks that shape while the fix above lands next door.
#[test]
fn exact_empty_while_true_still_ticks() {
    let java = java_for(
        "// @version:4\nwhile (true) {}\n",
        &Options::exact(Version::V4, 1),
    );
    assert!(java.contains("while (ops(bool(true), 0)) {"), "{java}");
    assert!(
        while_body(&java).contains("ops(1);"),
        "exact mode lost the body-entry tick:\n{java}"
    );
    // The `bool(...)` wrapper keeps the loop opaque to javac, so the trailing
    // return is reachable and required.
    assert!(java.contains("return null;"), "{java}");
}
