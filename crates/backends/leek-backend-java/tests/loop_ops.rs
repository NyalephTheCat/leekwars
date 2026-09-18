//! Op accounting for loop bodies.
//!
//! Regression cover for #388: `while (true) {}` compiled to a Java loop
//! that charged nothing per iteration, so the per-turn op budget never
//! tripped and the fight ran at 100% CPU until the worker timeout.

use leek_backend_java::{Options, emit};
use leek_parser::{ParseFeatures, ast::AstNode, parse_with_features};
use leek_span::SourceId;
use leek_syntax::{SyntaxNode, Version};

fn java_for(src: &str, opts: &Options) -> String {
    let source = SourceId::new(1).unwrap();
    let parsed = parse_with_features(src, source, opts.version, ParseFeatures::default());
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

/// The body of the first `for (…) {` header in `java`, up to its closing brace.
/// `emit_for` newline-breaks after the init clause, so the header ends at the
/// `; <step>) {` fragment rather than a single `) {`.
fn for_body(java: &str) -> String {
    let (_, after) = java.split_once("for (").expect("a for loop");
    let (_, body) = after.split_once(") {").expect("a for header");
    let (body, _) = body.split_once('}').expect("a loop body");
    body.to_string()
}

/// #485: a `for` whose condition is absent (or the literal `true`) cannot
/// complete normally per JLS, exactly like `while (true)`. Clean mode emits the
/// bare constant, so javac rejects a trailing `return null;` as unreachable —
/// `stmt_definitely_returns` has to drop it.
#[test]
fn clean_for_ever_drops_the_unreachable_return() {
    for src in [
        "// @version:4\nfor (;;) {}\n",
        "// @version:4\nfor (;true;) {}\n",
        "// @version:4\nfor (var i = 0; true; i++) {}\n",
    ] {
        let java = java_for(src, &Options::clean(Version::V4, 1));
        assert!(
            java.contains("true; "),
            "clean mode must keep a javac-visible constant condition for {src:?}:\n{java}"
        );
        assert!(
            !java.contains("return null;"),
            "a return after a provably-infinite `for` is unreachable for {src:?}:\n{java}"
        );
    }
}

/// A `for` that can complete normally still needs its trailing `return null;`,
/// and one whose body `break`s out is not infinite.
#[test]
fn clean_finite_for_keeps_the_trailing_return() {
    for src in [
        "// @version:4\nfor (var i = 0; i < 3; i++) {}\n",
        "// @version:4\nfor (;;) { break; }\n",
    ] {
        let java = java_for(src, &Options::clean(Version::V4, 1));
        assert!(
            java.contains("return null;"),
            "a `for` that can complete normally must keep its return for {src:?}:\n{java}"
        );
    }
}

/// Statements after a provably-infinite `for` are unreachable too, so clean
/// mode's dead-code elimination has to drop them — the same `emit_stmts`
/// cutoff that already covers `while (true)`.
///
/// A *trailing bare expression* is the one shape this does not reach: the main
/// block splits it off before `emit_stmts` runs and re-emits it as runIA's
/// `return`. That hole is not specific to `for` — `while (true) {}` followed by
/// an expression has it too — so it is left alone here.
#[test]
fn clean_for_ever_drops_following_statements() {
    let java = java_for(
        "// @version:4\nfor (;;) {}\nvar after = 1\n",
        &Options::clean(Version::V4, 1),
    );
    assert!(
        !java.contains("after"),
        "code after a provably-infinite `for` is unreachable:\n{java}"
    );
}

/// The empty-body charge hole of #388 applies to `for (;;)` as well: the loop
/// is infinite, so a body that charges nothing per iteration spins forever.
#[test]
fn clean_empty_for_ever_still_charges() {
    let java = java_for(
        "// @version:4\nfor (;;) {}\n",
        &Options::clean(Version::V4, 1),
    );
    assert!(
        for_body(&java).contains("ops("),
        "empty clean-mode `for (;;)` body charges nothing:\n{java}"
    );
}

/// Exact mode wraps the absent condition in `bool(...)` the same way
/// `loop_cond_string` wraps a written literal: the loop stays opaque to javac,
/// so the trailing `return null;` is reachable and required rather than
/// rejected as unreachable.
#[test]
fn exact_for_ever_keeps_an_opaque_condition_and_the_return() {
    let java = java_for(
        "// @version:4\nfor (;;) {}\n",
        &Options::exact(Version::V4, 1),
    );
    assert!(java.contains("ops(bool(true), 0); "), "{java}");
    assert!(java.contains("return null;"), "{java}");
    assert!(
        for_body(&java).contains("ops(1);"),
        "exact mode lost the body-entry tick:\n{java}"
    );
}

/// The `while (…)` the foreach lowering emits, i.e. the per-iteration body.
fn foreach_body(java: &str) -> String {
    let (_, after) = java.split_once("hasNext()) {").expect("a foreach loop");
    let (body, _) = after.split_once('}').expect("a loop body");
    body.to_string()
}

/// #486: `emit_foreach` writes its per-iteration ticks inline rather than
/// through `emit_body_with_entry_tick`, so clean mode's empty-body hole (#388)
/// was never closed for it and `for (var x in […]) {}` charged nothing per
/// iteration — op drift against both exact mode and the native backend.
#[test]
fn clean_empty_foreach_still_charges() {
    for src in [
        "// @version:4\nfor (var x in [1,2,3]) {}\n",
        "// @version:4\nfor (var k : var v in [1,2,3]) {}\n",
    ] {
        let java = java_for(src, &Options::clean(Version::V4, 1));
        assert!(
            foreach_body(&java).contains("ops("),
            "empty clean-mode foreach body charges nothing for {src:?}:\n{java}"
        );
    }
}

/// As for the other loops, the hand-emitted tick must not double up on a body
/// the charge pass already charges.
#[test]
fn clean_nonempty_foreach_body_charges_once() {
    let java = java_for(
        "// @version:4\nfor (var x in [1,2,3]) { var y }\n",
        &Options::clean(Version::V4, 1),
    );
    let body = foreach_body(&java);
    assert_eq!(
        body.matches("ops(").count(),
        1,
        "expected exactly one charge in the body:\n{java}"
    );
}

/// Characterization: foreach's exact-mode ticks are shape- and
/// version-dependent — unlike the flat `ops(1);` the other three loops take
/// from `emit_body_with_entry_tick` — so they stay inline. Locking the shapes
/// here keeps the #486 fix from flattening them.
#[test]
fn exact_foreach_keeps_its_own_per_iteration_ticks() {
    let value_v4 = java_for(
        "// @version:4\nfor (var x in [1,2,3]) {}\n",
        &Options::exact(Version::V4, 1),
    );
    assert!(
        foreach_body(&value_v4).contains("ops(1);"),
        "value-only v4 foreach lost its tick:\n{value_v4}"
    );
    // v1 charges a second op for the by-value copy on `set(...)`.
    let value_v1 = java_for(
        "// @version:1\nfor (var x in [1,2,3]) {}\n",
        &Options::exact(Version::V1, 1),
    );
    assert!(
        foreach_body(&value_v1).contains("ops(1);ops(1);"),
        "v1 foreach lost its by-value copy tick:\n{value_v1}"
    );
    // Key:value at v2+ charges nothing per iteration; the fix must not add one.
    let keyed_v4 = java_for(
        "// @version:4\nfor (var k : var v in [1,2,3]) {}\n",
        &Options::exact(Version::V4, 1),
    );
    assert!(
        !foreach_body(&keyed_v4).contains("ops("),
        "keyed v4 foreach must charge nothing per iteration:\n{keyed_v4}"
    );
}
