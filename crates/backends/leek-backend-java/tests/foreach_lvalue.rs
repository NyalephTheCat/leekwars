//! `for (x in …)` with no `var`: the binding is an **l-value**, not a
//! declaration.
//!
//! The header stores one slot per iteration into whatever `x` already names —
//! a global, a static or inherited class field, a reassigned builtin, an outer
//! local a lambda shares. The foreach emitter used to reimplement a three-case
//! subset of the assignment l-value vocabulary and fell through to a bare
//! `u_x = …` for everything else, naming a Java local that does not exist
//! (javac "cannot find symbol") or the wrong storage. It now delegates to the
//! same `write_place_store` dispatch a plain `=` uses.
//!
//! The capture walks had the matching hole: they visited a foreach's iterable
//! and body but never its binding targets, so a lambda whose only write to an
//! outer binding went through a foreach header looked write-free and the
//! binding was never boxed — neither the `Object[]` a captured-written `var`
//! gets nor the runtime `Box` a parameter gets.

use leek_backend_java::{Options, emit};
use leek_parser::{ParseFeatures, ast::AstNode, parse_with_features};
use leek_span::SourceId;
use leek_syntax::{SyntaxNode, Version};

fn java_for(src: &str, opts: &Options) -> String {
    let source = SourceId::new(1).unwrap();
    let parsed = parse_with_features(src, source, opts.version, ParseFeatures::default());
    let root = SyntaxNode::new_root(parsed.green);
    let sf = leek_parser::ast::SourceFile::cast(root).expect("parse");
    let (hir, diags) = leek_hir::lower_file(&sf, source);
    assert!(
        diags
            .iter()
            .all(|d| d.severity != leek_diagnostics::Severity::Error),
        "lowering errors for {src:?}: {diags:?}"
    );
    emit(&hir, opts).java
}

fn v4(src: &str) -> String {
    java_for(src, &Options::exact(Version::V4, 1))
}

#[track_caller]
fn assert_contains(java: &str, needle: &str) {
    assert!(java.contains(needle), "expected `{needle}` in:\n{java}");
}

/// A global is pre-declared by lowering wherever its `global` statement sits,
/// so a loop *above* that statement still binds the global slot — not a
/// name-keyed reference. The store is the global field, coerced the way a
/// plain `=` coerces it.
#[test]
fn foreach_over_a_global_declared_after_the_loop_writes_the_global_field() {
    let java = v4("// @version:4\nfor (g in [1,2]) {}\nglobal g = 0\nreturn g\n");
    assert_contains(&java, "g_g = (Object) __v1.getValue();");
    assert!(
        !java.contains("u_g ="),
        "the store must not invent a local for the global:\n{java}"
    );
}

/// A static field lives on the `ClassLeekValue`, not in a Java variable: the
/// write is the reflective `setField(<class>, "<name>", …, <calling class>)`
/// an assignment emits. This used to be `u_s = …` — a javac "cannot find
/// symbol", since no `u_s` is ever declared.
#[test]
fn foreach_over_a_static_class_field_writes_through_set_field() {
    let java =
        v4("// @version:4\nclass A { static s; static m() { for (s in [1,2]) {} } }\nA.m()\n");
    assert_contains(
        &java,
        r#"setField(u_A, "s", (Object) __v1.getValue(), u_A);"#,
    );
    assert!(
        !java.contains("u_s ="),
        "the store must not invent a local for the static field:\n{java}"
    );
}

/// An instance field the current class did not itself declare (here: inherited
/// from a parent) is not a direct Java field of the emitted inner class, so it
/// too goes through `setField` — on `this`.
#[test]
fn foreach_over_an_inherited_instance_field_writes_through_set_field() {
    let java = v4(
        "// @version:4\nclass A { f; }\nclass B extends A { m() { for (f in [1,2]) {} } }\nreturn new B()\n",
    );
    assert_contains(
        &java,
        r#"setField(u_B.this, "f", (Object) __v1.getValue(), u_B);"#,
    );
    assert!(
        !java.contains("u_f ="),
        "the store must not invent a local for the inherited field:\n{java}"
    );
}

/// The class's *own* instance field keeps the direct Java field write — the
/// delegation must not turn a same-class `this.f` store into a reflective one.
#[test]
fn foreach_over_an_own_instance_field_still_writes_the_java_field() {
    let java = v4("// @version:4\nclass A { f; m() { for (f in [1,2]) {} } }\nreturn new A()\n");
    assert_contains(&java, "u_A.this.f = (Object) __v1.getValue();");
    assert!(
        !java.contains(r#"setField(u_A.this, "f""#),
        "an own field must stay a direct Java field write:\n{java}"
    );
}

/// A name a `=` assignment already shadowed is read back out of the AI's
/// `__shadows` map, so the foreach store has to put it there: there is no Java
/// variable called `u_push`.
#[test]
fn foreach_over_a_shadowed_builtin_name_writes_the_shadow_map() {
    let java = v4("// @version:4\npush = 1\nfor (push in [1,2]) {}\nreturn push\n");
    assert_contains(&java, r#"__shadows.put("push", (Object) __v1.getValue());"#);
    assert!(
        !java.contains("u_push ="),
        "the store must not invent a local for the shadowed name:\n{java}"
    );
}

/// The binding target is a *write* to the outer local, so the lambda captures
/// it and the local has to be heap-boxed — an outlined factory takes captures
/// as `final` parameters, which a plain assignment to `u_acc` would not
/// survive. Both the capture walk and the boxed-locals collection have to see
/// through the loop header for this.
#[test]
fn a_lambda_binding_an_outer_local_in_a_foreach_header_boxes_it() {
    let java = v4(
        "// @version:4\nvar acc = 0\nvar f = function() { for (acc in [1,2]) {} }\nf()\nreturn acc\n",
    );
    assert_contains(&java, "Object[] u_acc = new Object[]{");
    assert_contains(&java, "__anon_0(final Object[] u_acc)");
    assert_contains(&java, "u_acc[0] = (Object) __v1.getValue();");
    assert_contains(&java, "return u_acc[0];");
}

/// Same, for the `key : value` header — the key binding is a second l-value
/// and needs the same treatment as the value one.
#[test]
fn a_lambda_binding_outer_locals_as_foreach_key_and_value_boxes_both() {
    let java = v4(
        "// @version:4\nvar k = 0\nvar v = 0\nvar f = function() { for (k : v in [1,2]) {} }\nf()\nreturn k + v\n",
    );
    assert_contains(&java, "Object[] u_k = new Object[]{");
    assert_contains(&java, "Object[] u_v = new Object[]{");
    assert_contains(&java, "u_k[0] = (Object) __v1.getKey();");
    assert_contains(&java, "u_v[0] = (Object) __v1.getValue();");
}

/// A *parameter* a lambda binds in a foreach header carries the other kind of
/// box — a runtime `Box` bound at the callee's entry — so the store is
/// `Box.set`, the first arm of the shared l-value dispatch. The capture query
/// that decides this (`leek_hir::captured_by_nested_lambda_stmts`) has to see
/// the binding target as well, or the factory gets a `final Object` it cannot
/// assign.
#[test]
fn a_lambda_binding_an_outer_parameter_in_a_foreach_header_boxes_it() {
    let java = v4(
        "// @version:4\nfunction h(p) { var q = function() { for (p in [1,2]) {} }\nreturn q() }\nreturn h(1)\n",
    );
    assert_contains(&java, "final Box u_p = new Box(this, p_p);");
    assert_contains(&java, "__anon_0(final Box u_p)");
    assert_contains(&java, "u_p.set((Object) __v1.getValue());");
}

/// Same for an enclosing foreach's own declared binding: an inner lambda that
/// rebinds it in its own header makes it a captured write, so the outer loop
/// has to declare it as a `Box` instead of a plain `Object`.
#[test]
fn a_lambda_rebinding_an_enclosing_foreach_binding_boxes_it() {
    let java = v4(
        "// @version:4\nfor (var x in [1,2]) { var q = function() { for (x in [3,4]) {} }\nq() }\nreturn 0\n",
    );
    assert_contains(&java, "final Box u_x = new Box(");
    assert_contains(&java, "u_x.set((Object) __v2.getValue());");
}

/// A binding the header *declares* is still a local of the loop, not a
/// capture: `for (var x in …)` inside a lambda must not box or thread
/// anything.
#[test]
fn a_foreach_declared_binding_inside_a_lambda_is_not_a_capture() {
    let java = v4("// @version:4\nvar f = function() { for (var x in [1,2]) {} }\nreturn f()\n");
    assert_contains(&java, "Object u_x = null;");
    assert!(
        !java.contains("__anon_"),
        "a lambda with no outer capture must stay inline:\n{java}"
    );
}
