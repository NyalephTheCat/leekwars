//! End-to-end smoke: parse → HIR → Java emit. The assertions stay
//! at the shape level — byte parity against the Java reference
//! requires the golden-output harness, which is its own milestone.

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

#[test]
fn exact_mode_emits_class_skeleton() {
    let java = java_for(
        "// @version:4\nvar x = 1\nreturn x\n",
        &Options::exact(Version::V4, 42),
    );
    assert!(java.contains("public class AI_42 extends AI {"), "{java}");
    assert!(
        java.contains("public AI_42() throws LeekRunException {"),
        "{java}"
    );
    assert!(java.contains("super("), "{java}");
    assert!(
        java.contains("public Object runIA(Session session)"),
        "{java}"
    );
    // Exact mode: u_-prefixed locals.
    assert!(java.contains("Object u_x ="), "{java}");
    // Exact mode: op tick folded into the value-producing expression.
    assert!(java.contains("ops("), "{java}");
}

#[test]
fn java_backend_directive_replaces_call() {
    use leek_parser::{ParseFeatures, parse_with_features};
    // A signature file: bodiless `add` with a `@java-backend:` directive.
    // The call `add(1, 2)` should emit the substituted directive body
    // instead of a normal `f_add(...)` call.
    let src = "// @experimental: function_signatures\n\
/**\n * @java-backend: Math.addExact(%0, %1)\n */\n\
function add(integer a, integer b) -> integer;\n\
return add(1, 2)\n";
    let source = SourceId::new(1).unwrap();
    let parsed = parse_with_features(
        src,
        source,
        Version::V4,
        ParseFeatures {
            function_signatures: true,
            ..Default::default()
        },
    );
    let root = SyntaxNode::new_root(parsed.green);
    let sf = leek_parser::ast::SourceFile::cast(root).expect("parse");
    let (hir, _diags) = leek_hir::lower_file(&sf, source);
    let java = emit(&hir, &Options::exact(Version::V4, 1)).java;
    // The call site emits the substituted directive body
    // (`Math.addExact(1l, 2l)`) rather than a normal call to `add`.
    assert!(
        java.contains("Math.addExact(1l, 2l)"),
        "directive body should be emitted at the call site: {java}"
    );
    // The bodiless signature emits no method stub.
    assert!(
        !java.contains("f_add("),
        "bodiless signature should not emit a method: {java}"
    );
}

#[test]
fn prelude_builtin_call_uses_directive() {
    use leek_parser::{ParseFeatures, parse, parse_with_features};
    // User code calls `abs` with *no* local declaration — it resolves
    // to the implicit prelude's signature and emits that signature's
    // `@java-backend:` directive.
    let prelude_src = "// @experimental: function_signatures\n\
/** @java-backend: Math.abs(%0) */\n\
function abs(real x) -> real;\n";
    let user_src = "// @version:4\nreturn abs(-5)\n";
    let source = SourceId::new(1).unwrap();
    let prelude_source = SourceId::new(0xF00D).unwrap();

    let p = parse_with_features(
        prelude_src,
        prelude_source,
        Version::V4,
        ParseFeatures {
            function_signatures: true,
            ..Default::default()
        },
    );
    let prelude_ast =
        leek_parser::ast::SourceFile::cast(SyntaxNode::new_root(p.green)).expect("prelude parse");
    let u = parse(user_src, source, Version::V4);
    let user_ast =
        leek_parser::ast::SourceFile::cast(SyntaxNode::new_root(u.green)).expect("user parse");

    let (hir, _diags) =
        leek_hir::lower_file_with_prelude(&user_ast, source, 4, &prelude_ast, prelude_source);
    let java = emit(&hir, &Options::exact(Version::V4, 1)).java;
    assert!(
        java.contains("Math.abs("),
        "prelude builtin call should emit its directive: {java}"
    );
}

#[test]
fn clean_mode_drops_prefix_and_folds_charges() {
    let java = java_for(
        "// @version:4\nvar x = 1\nreturn x\n",
        &Options::clean(Version::V4, 42),
    );
    // Clean mode: unprefixed local.
    assert!(java.contains("Object x ="), "{java}");
    assert!(!java.contains("Object u_x"), "{java}");
    // Clean mode: folded charge call, not per-stmt.
    assert!(java.contains("ops("), "{java}");
    assert!(!java.contains("ops(1);"), "{java}");
}

#[test]
fn function_call_uses_f_prefix_when_user_name_collides_with_runtime() {
    let java = java_for(
        "// @version:4\nfunction add(a, b) { return a + b }\nreturn add(1, 2)\n",
        &Options::clean(Version::V4, 1),
    );
    // `add` collides with the runtime's add helper — keep `f_` prefix
    // even in clean mode.
    assert!(java.contains("f_add"), "{java}");
}

#[test]
fn string_concat_uses_add_helper_with_string_cast() {
    let java = java_for(
        "// @version:4\nreturn \"hi: \" + 3\n",
        &Options::exact(Version::V4, 1),
    );
    // Reference emits `(String) add(...)` — same `add` overload as
    // numeric `+`, just cast through `String` at the call site.
    assert!(java.contains("(String) add("), "{java}");
}

#[test]
fn if_else_emits_braced_blocks() {
    let java = java_for(
        "// @version:4\nif (1 < 2) { return 1 } else { return 2 }\n",
        &Options::exact(Version::V4, 1),
    );
    assert!(java.contains("if ("), "{java}");
    // Reference splits `}\nelse {` onto two lines in exact mode.
    assert!(java.contains("}\nelse {"), "{java}");
}

#[test]
fn for_loop_emits_header_with_init() {
    let java = java_for(
        "// @version:4\nfor (var i = 0; i < 3; i = i + 1) { debug(i) }\n",
        &Options::exact(Version::V4, 1),
    );
    // Init/cond/step all rendered in the for header; not hoisted.
    assert!(java.contains("for (Object u_i ="), "{java}");
    assert!(
        java.contains("less(u_i, 3l)") || java.contains(" < 3l"),
        "{java}"
    );
}

/// A small file-based environment catalog (the generic env-dispatch path
/// still used by `--library path/to.lib`). The leek-wars game functions
/// themselves now dispatch via `@java-dispatch:` directives in their
/// signature header, not this catalog.
fn fight_catalog() -> std::sync::Arc<dyn leek_environment::EnvironmentCatalog> {
    let src = "namespace = com.leekwars.generator.classes.*\n\
        getCell\tEntityClass\tstatic\t0\t1\t5\n\
        getLife\tEntityClass\tstatic\t0\t1\t50\n\
        getNearestEnemy\tFightClass\tstatic\t0\t0\t50\n\
        moveToward\tFightClass\tstatic\t1\t2\t500\n";
    std::sync::Arc::new(leek_environment::FileCatalog::parse(src).expect("catalog"))
}

#[test]
fn environment_catalog_emits_generator_dispatch() {
    let opts = Options::clean(Version::V4, 7).with_environment(fight_catalog());
    let java = java_for(
        "// @version:4\nvar c = getCell()\nmoveToward(getNearestEnemy(), 5)\nreturn c\n",
        &opts,
    );
    // Generator-compatible static dispatch + the dispatch-class import.
    assert!(
        java.contains("import com.leekwars.generator.classes.*;"),
        "{java}"
    );
    assert!(java.contains("EntityClass.getCell("), "{java}");
    assert!(java.contains("FightClass.moveToward("), "{java}");
    assert!(java.contains("FightClass.getNearestEnemy("), "{java}");
}

#[test]
fn fight_ai_extends_entity_ai_base_class() {
    let opts = Options::clean(Version::V4, 9)
        .with_environment(fight_catalog())
        .with_base_class("EntityAI");
    let java = java_for(
        "// @version:4
return getLife()
",
        &opts,
    );
    assert!(
        java.contains("public class AI_9 extends EntityAI {"),
        "{java}"
    );
    assert!(java.contains("EntityClass.getLife("), "{java}");
}

#[test]
fn without_catalog_game_function_is_bare_call() {
    // No environment catalog → unknown name falls back to a bare call
    // (unchanged behaviour), and no generator import is added.
    let java = java_for(
        "// @version:4\nreturn getCell()\n",
        &Options::clean(Version::V4, 7),
    );
    assert!(!java.contains("com.leekwars.generator"), "{java}");
    assert!(!java.contains("EntityClass.getCell"), "{java}");
    assert!(java.contains("getCell("), "{java}");
}

#[test]
fn lambda_write_to_captured_local_is_boxed() {
    // A lambda that *writes* to a captured outer local used to emit a
    // null-returning stub (a silent miscompile). LeekScript closures capture
    // by reference, so the local is now heap-boxed as a shared `Object[]`:
    // declared as `Object[] count = new Object[]{...}`, read/written via
    // `count[0]`, and threaded into the outlined factory as a
    // `final Object[]` parameter. (Clean mode emits bare local names.)
    let java = java_for(
        "// @version:4\nvar count = 0\nvar inc = function() { count = count + 1 }\ninc()\ninc()\nreturn count\n",
        &Options::clean(Version::V4, 1),
    );
    assert!(
        java.contains("Object[] count = new Object[]{"),
        "captured-written local should be boxed: {java}"
    );
    assert!(
        java.contains("count[0]"),
        "boxed local should be accessed via [0]: {java}"
    );
    assert!(
        java.contains("final Object[] count"),
        "factory should take the box as a final Object[] param: {java}"
    );
    // The outlined factory is used, not the null-returning stub.
    assert!(
        java.contains("__anon_"),
        "lambda should be outlined: {java}"
    );
    assert!(
        !java.contains("throws LeekRunException {return null;}}"),
        "must not emit the null-returning stub: {java}"
    );
}

#[test]
fn lambda_reading_captured_local_is_not_boxed() {
    // A read-only capture must stay on the plain `final Object` path — boxing
    // is reserved for captured-AND-written locals.
    let java = java_for(
        "// @version:4\nvar base = 10\nvar add = function(x) { return x + base }\nreturn add(5)\n",
        &Options::clean(Version::V4, 1),
    );
    assert!(
        !java.contains("Object[] base"),
        "read-only capture should not be boxed: {java}"
    );
    assert!(
        java.contains("final Object base"),
        "read-only capture should be a plain final Object param: {java}"
    );
}

#[test]
fn for_loop_var_captured_and_written_is_boxed_in_header() {
    // A `for (var i = ...)` loop variable that a nested lambda captures and
    // writes must be boxed in the for-header too, so its declaration matches
    // the `[0]` accesses emitted elsewhere (regression guard for the
    // for-init emission path, which is separate from `emit_var_decl`).
    let java = java_for(
        "// @version:4\nfor (var i = 0; i < 1; i++) { var inc = function() { i = i + 10 } inc() }\nreturn 0\n",
        &Options::clean(Version::V4, 1),
    );
    assert!(
        java.contains("for (Object[] i = new Object[]{"),
        "captured-written for-loop var should be boxed in the header: {java}"
    );
    assert!(
        java.contains("i[0]"),
        "boxed for-loop var should be accessed via [0]: {java}"
    );
}

#[test]
fn lambda_writing_captured_parameter_uses_null_stub_known_limitation() {
    // KNOWN LIMITATION (item 3 / round-3 boxing scope boundary): the boxing
    // that makes a captured-and-written *local* shared (see
    // `lambda_write_to_captured_local_is_boxed`) does NOT yet cover a captured
    // *parameter*. A lambda that writes to an outer parameter still falls back
    // to the null-returning stub — it compiles but returns the wrong value at
    // runtime (the java backend is emit-only here, so the corpus can't catch
    // it). This test pins that boundary: when boxing is extended to parameters,
    // this assertion will flip and should be updated to assert the box instead.
    let java = java_for(
        "// @version:4\nfunction f(p) { var g = function() { p = p + 1 } g() return p }\nreturn f(5)\n",
        &Options::clean(Version::V4, 1),
    );
    assert!(
        java.contains("Object... values) throws LeekRunException {return null;}"),
        "captured-parameter write is expected to still hit the null stub: {java}"
    );
}
