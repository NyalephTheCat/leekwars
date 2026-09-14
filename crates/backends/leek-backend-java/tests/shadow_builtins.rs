//! Emission for builtin names the source reassigns (`cos = function…`).
//!
//! A reassigned builtin is read through the AI's `__shadows` map, with the
//! original builtin dispatch as the `else` arm of a ternary. The two arms are
//! emitted from the same `shadowed_builtins` set: the fallback goes through
//! the shadow-free entry points (`write_call_unshadowed` /
//! `write_name_unshadowed`) instead of re-entering the shadow-testing one with
//! the shared set cleared, so a *different* shadowed name nested inside the
//! fallback keeps its own test, and nothing mutates emitter state while an
//! expression is being written.

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

/// Occurrences of `needle` in `hay`.
fn count(hay: &str, needle: &str) -> usize {
    hay.matches(needle).count()
}

/// The `else` arm of the first shadow ternary for `name`: everything after
/// that ternary's `) : (`.
fn fallback_arm<'a>(java: &'a str, name: &str) -> &'a str {
    let key = format!("(__shadows.containsKey(\"{name}\")");
    let start = java
        .find(&key)
        .unwrap_or_else(|| panic!("no shadow test for `{name}`: {java}"));
    let rest = &java[start..];
    let sep = rest
        .find(") : (")
        .unwrap_or_else(|| panic!("no fallback arm for `{name}`: {java}"));
    &rest[sep + ") : (".len()..]
}

#[test]
fn shadowed_call_fallback_keeps_nested_shadow_tests() {
    // Both `cos` and `sin` are reassigned. The `cos` call tests the shadow map
    // once; inside *both* of its arms the argument `sin(2)` is a call to
    // another shadowed name, so it must carry its own test in each. Clearing
    // the shared set to emit the fallback used to un-shadow `sin` there, which
    // would have called the real `sin` after the user replaced it.
    let java = java_for(
        "// @version:1\n\
         cos = function(x) { return 1 }\n\
         sin = function(x) { return 2 }\n\
         return cos(sin(2))\n",
        &Options::exact(Version::V1, 1),
    );
    assert_eq!(
        count(&java, "__shadows.containsKey(\"cos\")"),
        1,
        "the shadowed callee is tested exactly once: {java}"
    );
    let fallback = fallback_arm(&java, "cos");
    assert!(
        fallback.contains("__shadows.containsKey(\"sin\")"),
        "the fallback arm must keep the nested `sin` shadow test: {java}"
    );
    // Once per arm of the `cos` ternary.
    assert_eq!(
        count(&java, "__shadows.containsKey(\"sin\")"),
        2,
        "`sin` is tested in both arms of the `cos` ternary: {java}"
    );
}

#[test]
fn nested_shadowed_calls_stay_bounded() {
    // Three distinct reassigned builtins, nested. Every level tests its own
    // name in *both* arms of the level above it, so the innermost call is
    // emitted 2^depth times: 1 / 2 / 4 tests from outside in. (The
    // clear-and-restore fallback emitted 1 / 1 / 1 — smaller, and wrong: the
    // inner names lost their shadow test and called the real builtin.)
    // The size ceiling pins that doubling as the *ceiling*, so a lowering that
    // re-expands the nest per shadowed name is caught here and not in a
    // corpus snapshot. Current emission is ~2.0 KiB.
    let java = java_for(
        "// @version:1\n\
         cos = function(x) { return 1 }\n\
         sin = function(x) { return 2 }\n\
         tan = function(x) { return 3 }\n\
         return cos(sin(tan(2)))\n",
        &Options::exact(Version::V1, 1),
    );
    assert_eq!(count(&java, "__shadows.containsKey(\"cos\")"), 1, "{java}");
    assert_eq!(count(&java, "__shadows.containsKey(\"sin\")"), 2, "{java}");
    assert_eq!(count(&java, "__shadows.containsKey(\"tan\")"), 4, "{java}");
    assert!(
        java.len() < 2500,
        "3-deep shadowed nest emitted {} bytes: {java}",
        java.len()
    );
}

#[test]
fn shadowed_name_read_falls_back_to_the_builtin_reference() {
    // A non-call read of a reassigned builtin goes through `write_name`'s
    // ternary, whose fallback is the first-class builtin reference.
    let java = java_for(
        "// @version:1\npush = 1\nreturn push\n",
        &Options::exact(Version::V1, 1),
    );
    assert!(
        java.contains("__shadows.put(\"push\""),
        "the assignment writes the shadow map: {java}"
    );
    assert!(
        java.contains("(__shadows.containsKey(\"push\") ? __shadows.get(\"push\") : ("),
        "the read tests the shadow map: {java}"
    );
    assert!(
        fallback_arm(&java, "push").contains("anonymous_push"),
        "the fallback is the builtin function reference: {java}"
    );
}

/// A builtin reassigned inside a **class method** shadows the name for the
/// whole file: the write is a `__shadows.put`, and every later read or call of
/// the name has to test the map. The collector used to root itself at the main
/// block and top-level `function` bodies only, so this write was invisible —
/// the method emitted a bare `u_cos = …` (no such Java variable) and the call
/// below dispatched to the real builtin (#253).
#[test]
fn builtin_reassigned_in_a_class_method_shadows_the_name() {
    let java = java_for(
        "// @version:4\n\
         class A { static m() { cos = function(x) { return 1 } } }\n\
         A.m()\n\
         return cos(0)\n",
        &Options::exact(Version::V4, 1),
    );
    assert!(
        java.contains("private final HashMap<String, Object> __shadows"),
        "the file declares the shadow map: {java}"
    );
    assert!(
        java.contains("__shadows.put(\"cos\""),
        "the method's assignment writes the shadow map: {java}"
    );
    assert!(
        java.contains("__shadows.containsKey(\"cos\") ? execute(__shadows.get(\"cos\")"),
        "the later call routes through the shadow map: {java}"
    );
}

/// Same hole on the other class-body roots: a constructor, an instance-field
/// initialiser and a method's parameter default are all executable code that
/// can reassign a builtin name.
#[test]
fn builtin_reassigned_in_other_class_bodies_shadows_the_name() {
    for src in [
        "class A { A() { cos = 1 } }\nreturn cos\n",
        "class A { f = (cos = 1); }\nreturn cos\n",
        "class A { m(p = (cos = 1)) { return p } }\nreturn cos\n",
    ] {
        let java = java_for(
            &format!("// @version:4\n{src}"),
            &Options::exact(Version::V4, 1),
        );
        assert!(
            java.contains("__shadows.put(\"cos\""),
            "the write goes to the shadow map for {src:?}: {java}"
        );
        assert!(
            java.contains("__shadows.containsKey(\"cos\")"),
            "the later read tests the shadow map for {src:?}: {java}"
        );
    }
}

/// A **compound** assignment to a builtin name is a write to that name just
/// like a plain one: `cos += 1` reads the shadow (or the builtin reference)
/// and stores back through `__shadows`. Matching only `BinaryOp::Assign`
/// missed it, and the store fell through to a bare `u_cos = …`.
#[test]
fn compound_assign_to_a_builtin_name_shadows_it() {
    let java = java_for(
        "// @version:4\ncos += 1\nreturn cos\n",
        &Options::exact(Version::V4, 1),
    );
    assert!(
        java.contains("__shadows.put(\"cos\""),
        "the compound assignment writes the shadow map: {java}"
    );
    assert!(
        !java.contains("u_cos ="),
        "the store must not invent a local for the shadowed name: {java}"
    );
}

/// A bare `foreach` header binds an l-value, so `for (push in …)` writes the
/// name `push` with no assignment anywhere in the file. #504 taught the store
/// to route a shadowed name through `__shadows`, but the collector never saw
/// the binding, so `push` was not in the set and the store emitted a bare
/// `u_push = …` — a Java local that is never declared.
#[test]
fn foreach_binding_a_builtin_name_shadows_it() {
    let java = java_for(
        "// @version:4\nfor (push in [1,2]) {}\nreturn push\n",
        &Options::exact(Version::V4, 1),
    );
    assert!(
        java.contains("__shadows.put(\"push\""),
        "the loop header writes the shadow map: {java}"
    );
    assert!(
        !java.contains("u_push ="),
        "the store must not invent a local for the bound name: {java}"
    );
}
