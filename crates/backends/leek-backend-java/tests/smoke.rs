//! End-to-end smoke: parse → HIR → Java emit. The assertions stay
//! at the shape level — byte parity against the Java reference
//! requires the golden-output harness, which is its own milestone.

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
    use leek_parser::{ParseFeatures, parse_with_features};
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
    let u = parse_with_features(user_src, source, Version::V4, ParseFeatures::default());
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
    assert!(
        !java.contains(NULL_STUB),
        "must not emit the null stub: {java}"
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

/// The null-returning lambda body the emitter used to fall back to when a
/// lambda wrote a captured binding it couldn't box. Nothing may emit it again:
/// it compiles, then silently computes the wrong answer.
const NULL_STUB: &str = "Object... values) throws LeekRunException {return null;}";

#[test]
fn lambda_writing_captured_parameter_is_boxed() {
    // A lambda that writes an outer *parameter* used to emit a null-returning
    // stub (a silent miscompile). Leek closures capture by reference, so the
    // param now binds to a runtime `Box` at function entry in **both** modes:
    // the lambda's write routes through `Box.set(...)` and the post-lambda read
    // through `.get()`, so `f(5)` really returns 6.
    for (opts, body_name) in [
        (Options::clean(Version::V4, 1), "p"),
        (Options::exact(Version::V4, 1), "u_p"),
    ] {
        let java = java_for(
            "// @version:4\nfunction f(p) { var g = function() { p = p + 1 } g() return p }\nreturn f(5)\n",
            &opts,
        );
        assert!(
            java.contains(&format!("final Box {body_name} = new Box(")),
            "captured param should bind to a Box at entry: {java}"
        );
        assert!(
            java.contains(&format!(
                "private FunctionLeekValue __anon_0(final Box {body_name})"
            )),
            "factory should take the Box: {java}"
        );
        assert!(
            java.contains(&format!("{body_name}.set(")),
            "the lambda's write should route through the Box: {java}"
        );
        assert!(
            !java.contains(NULL_STUB),
            "must not emit the null stub: {java}"
        );
    }
}

#[test]
fn v1_lambda_writing_captured_parameter_is_boxed() {
    // Same at v1, where params keep value semantics: the Box still holds a
    // `copy(...)` of the argument, but the closure shares that box.
    let java = java_for(
        "// @version:1\nfunction f(p) { var g = function() { p = p + 1 } g() return p }\nreturn f(5)\n",
        &Options::exact(Version::V1, 1),
    );
    assert!(
        java.contains("Box u_p = new Box(this, copy(p_p));"),
        "v1 captured param should bind to a value-copy Box: {java}"
    );
    assert!(java.contains("u_p.set("), "{java}");
    assert!(
        !java.contains(NULL_STUB),
        "must not emit the null stub: {java}"
    );
}

#[test]
fn lambda_write_visible_to_later_read() {
    // The write has to be visible to a read *after* the lambda ran: the
    // post-lambda `return p` must go through the box, not a stale copy.
    let java = java_for(
        "// @version:4\nfunction f(p) { var g = function() { p = p + 1 } g() return p }\nreturn f(5)\n",
        &Options::clean(Version::V4, 1),
    );
    assert!(
        java.contains("return p.get();"),
        "post-lambda read must go through the box: {java}"
    );
    assert!(
        !java.contains(NULL_STUB),
        "must not emit the null stub: {java}"
    );
}

#[test]
fn class_method_param_written_by_lambda_is_boxed() {
    // Class method / constructor params take the same entry rebind as a
    // top-level function's — they used to get no rebind at all, so a lambda
    // writing one fell through to the null stub.
    let java = java_for(
        "// @version:4\nclass A { method m(p) { var g = function() { p = p + 1 } g() return p } }\nvar o = new A()\nreturn o.m(5)\n",
        &Options::exact(Version::V4, 1),
    );
    assert!(
        java.contains("public Object u_m(Object p_p) throws LeekRunException {"),
        "boxed method param should take the p_ signature slot: {java}"
    );
    assert!(
        java.contains("final Box u_p = new Box(AI_1.this, p_p);"),
        "method param should be rebound to a Box: {java}"
    );
    assert!(java.contains("u_p.set("), "{java}");
    assert!(java.contains("return u_p.get();"), "{java}");
    assert!(
        !java.contains(NULL_STUB),
        "must not emit the null stub: {java}"
    );
}

#[test]
fn lambda_writing_captured_int_is_boxed_in_both_modes() {
    // The plain "lambda increments a captured counter" case, in both modes.
    for (opts, name) in [
        (Options::clean(Version::V4, 1), "n"),
        (Options::exact(Version::V4, 1), "u_n"),
    ] {
        let java = java_for(
            "// @version:4\nvar n = 0\nvar inc = function() { n = n + 1 }\ninc()\ninc()\nreturn n\n",
            &opts,
        );
        assert!(
            java.contains(&format!("Object[] {name} = new Object[]{{")),
            "captured-written local should be boxed: {java}"
        );
        assert!(java.contains(&format!("{name}[0]")), "{java}");
        assert!(
            java.contains(&format!(
                "private FunctionLeekValue __anon_0(final Object[] {name})"
            )),
            "factory should take the box: {java}"
        );
        assert!(
            !java.contains(NULL_STUB),
            "must not emit the null stub: {java}"
        );
    }
}

#[test]
fn nested_lambda_writing_outer_local_is_boxed() {
    // The write is two lambda levels down from the declaration, so the box has
    // to be threaded through *both* factories. The capture/write walkers used
    // to stop at the first lambda body, which left this on the null stub.
    let java = java_for(
        "// @version:4\nvar c = 0\nvar f = function() { var g = function() { c = c + 1 } g() }\nf()\nreturn c\n",
        &Options::clean(Version::V4, 1),
    );
    assert!(java.contains("Object[] c = new Object[]{"), "{java}");
    assert!(java.contains("c[0] = (Object) add(c[0], 1l)"), "{java}");
    assert!(
        java.contains("private FunctionLeekValue __anon_0(final Object[] c)"),
        "outer factory must take the box: {java}"
    );
    assert!(
        java.contains("private FunctionLeekValue __anon_1(final Object[] c)"),
        "inner factory must take the box: {java}"
    );
    assert!(
        !java.contains(NULL_STUB),
        "must not emit the null stub: {java}"
    );
}

#[test]
fn lambda_local_written_by_inner_lambda_is_boxed() {
    // The local is declared *inside* a lambda body and written by a lambda
    // nested in it. The box is an ordinary Java local of the outer lambda's
    // `run` method, which the inner factory call captures.
    let java = java_for(
        "// @version:4\nvar f = function() { var c = 0 var g = function() { c = c + 1 } g() return c }\nreturn f()\n",
        &Options::clean(Version::V4, 1),
    );
    assert!(
        java.contains("Object[] c = new Object[]{"),
        "a local declared inside a lambda must be boxable too: {java}"
    );
    assert!(java.contains("__anon_0(c)"), "{java}");
    assert!(
        java.contains("private FunctionLeekValue __anon_0(final Object[] c)"),
        "{java}"
    );
    assert!(
        !java.contains(NULL_STUB),
        "must not emit the null stub: {java}"
    );
}

#[test]
fn nested_lambda_capture_is_threaded_through_outer_factory() {
    // `a` is referenced *only* from the inner lambda. The outer factory still
    // has to receive it, or the `__anon_1(a)` call it emits names a symbol that
    // isn't in scope — a javac "cannot find symbol", not a silent wrong value.
    let java = java_for(
        "// @version:4\nvar a = 0\nvar b = 0\nvar f = function() { b = b + 1 var g = function() { return a } return g() }\nreturn f()\n",
        &Options::clean(Version::V4, 1),
    );
    assert!(
        java.contains("private FunctionLeekValue __anon_0(final Object[] b, final Object a)"),
        "outer factory must take both the written box and the inner-only capture: {java}"
    );
    assert!(java.contains("__anon_0(b, a)"), "{java}");
    assert!(java.contains("__anon_1(a)"), "{java}");
}

#[test]
fn self_recursive_lambda_with_nested_lambda_routes_through_self_box() {
    // A nested lambda may reference the var the enclosing lambda is being
    // assigned to. The Java local is mid-initialization at that point, so the
    // inner factory's call site has to read it out of `_self_box` — passing
    // the bare name is a javac "might not have been initialized".
    let java = java_for(
        "// @version:4\nvar fact = function(n) { var h = function() { if (n <= 1) { return 1 } return n * fact(n - 1) } return h() }\nreturn fact(5)\n",
        &Options::clean(Version::V4, 1),
    );
    assert!(
        java.contains("__anon_0(n, _self_box[0], _self_box)"),
        "the in-construction var must be passed out of the self box: {java}"
    );
    assert!(
        !java.contains("__anon_0(n, fact,"),
        "must not pass the mid-initialization local: {java}"
    );
}

/// Every Java local a lowered switch declares (`Object __sw_N`, `int __si_N`,
/// …), in emission order.
fn switch_temp_decls(java: &str) -> Vec<String> {
    java.lines()
        .filter_map(|l| {
            let l = l.trim_start();
            let rest = l
                .strip_prefix("Object ")
                .or_else(|| l.strip_prefix("int "))?;
            let name = rest.split([' ', ';']).next()?;
            name.starts_with("__s").then(|| name.to_string())
        })
        .collect()
}

#[test]
fn sequential_and_nested_switches_use_unique_braced_temporaries() {
    // #42: two sequential string switches plus a nested one used to declare
    // `__scrut` / `__idx` three times in the same Java scope (javac: "variable
    // already defined"). Each switch now gets its own numbered temporaries
    // inside its own `{ … }` block, in both modes.
    let src = "var a = 'x' var y = 2 var r = 0 \
               switch (a) { case 'x': r += 1 case 'y': r += 2 } \
               switch (a) { case 'x': switch (y) { case 2: r += 5 break } break default: r += 20 } \
               return r";
    for opts in [
        Options::exact(Version::V4, 1),
        Options::clean(Version::V4, 1),
    ] {
        let java = java_for(src, &opts);
        let decls = switch_temp_decls(&java);
        let unique: std::collections::HashSet<_> = decls.iter().collect();
        assert_eq!(decls.len(), unique.len(), "duplicate switch temp: {java}");
        for name in ["__sw_0", "__si_0", "__sw_1", "__si_1", "__sw_2", "__si_2"] {
            assert!(decls.iter().any(|d| d == name), "missing {name}: {java}");
        }
        assert!(
            !java.contains("__scrut") && !java.contains("__idx"),
            "{java}"
        );
    }
}

#[test]
fn exact_switch_matches_upstream_lowering_shape() {
    // #75: upstream `SwitchBlock.writeComparisonChain` shape, byte for byte:
    // a braced block, `__sw_N` / `__si_N`, grouped labels in an `else if`
    // chain charging 1 op per label, and braced arms charging `ops(1)`.
    let java = java_for(
        "var x = 1 switch (x) { case 1: case 2: return 'a' case 3: return 'b' } return 'none'",
        &Options::exact(Version::V4, 1),
    );
    let expected = "{\n\
                    Object __sw_0 = u_x;\n\
                    int __si_0 = -1;\n\
                    if (ops(eq(__sw_0, 1l) || eq(__sw_0, 2l), 2)) __si_0 = 0;\n\
                    else if (ops(eq(__sw_0, 3l), 1)) __si_0 = 1;\n\
                    switch (__si_0) {\n\
                    case 0: {\n\
                    ops(1);return \"a\";\n\
                    }\n\
                    case 1: {\n\
                    ops(1);return \"b\";\n\
                    }\n\
                    }\n\
                    }\n";
    assert!(java.contains(expected), "{java}");
}

#[test]
fn costed_switch_discriminant_charges_inside_its_initializer() {
    // #75: a discriminant that costs ops charges them inside the `__sw_N`
    // initializer, like every other costed initializer (`Object u_x =
    // ops(1l, 1);`). It used to be a standalone `ops(N);` glued to the front
    // of the `int __si_N = -1;` line, a shape upstream never writes. No
    // reference row has a costed discriminant — they are all bare variables —
    // so this is the only guard on that branch.
    let java = java_for(
        "var x = 1 switch (x + 1) { case 2: return 'a' } return 'none'",
        &Options::exact(Version::V4, 1),
    );
    assert!(
        java.contains("Object __sw_0 = ops((Object) add(u_x, 1l), 1);\nint __si_0 = -1;\n"),
        "{java}"
    );
    assert!(!java.contains("ops(1);int __si_0"), "{java}");
}

#[test]
fn switch_arms_get_their_own_java_scope() {
    // #42: the same local declared in two arms collided in the single scope
    // of an unbraced Java switch block.
    let java = java_for(
        "var x = 1 switch (x) { case 1: var u = 1 return u case 2: var u = 2 return u } return 0",
        &Options::exact(Version::V4, 1),
    );
    assert!(
        java.contains("case 0: {\n") && java.contains("case 1: {\n"),
        "{java}"
    );
}

#[test]
fn switch_ending_in_empty_label_arm_needs_no_trailing_return() {
    // `case 1: case 2: return …` — the empty label arm falls through into a
    // returning arm, so javac sees the switch as never completing normally
    // and rejects a trailing `return null;` as unreachable.
    let src = "function f(x) { switch (x) { case 1: case 2: return 'a' default: return 'd' } } return f(1)";
    for opts in [
        Options::exact(Version::V4, 1),
        Options::clean(Version::V4, 1),
    ] {
        let java = java_for(src, &opts);
        let f = java
            .split("private Object f")
            .nth(1)
            .and_then(|rest| rest.split("runIA").next())
            .expect("function body");
        assert!(
            !f.contains("return null;"),
            "unreachable trailing return: {java}"
        );
    }
}

#[test]
fn clean_native_switch_guards_the_discriminant() {
    // #72: the native path used to emit `switch ((int) ((Number) x).longValue())`,
    // which truncates reals (1.7 matched `case 1`), wraps longs, and throws on
    // strings / null. It now dispatches only a `Long` that fits an `int`, and
    // sends every other subject through the loose-equality `eq` chain.
    let java = java_for(
        "var x = 1.7 switch (x) { case 1: return 'one' case -2: return 'minus' default: return 'd' }",
        &Options::clean(Version::V4, 1),
    );
    assert!(!java.contains("((Number)"), "unguarded int cast: {java}");
    assert!(
        java.contains("if (__sw_0 instanceof Long __swv_0) {"),
        "{java}"
    );
    assert!(
        java.contains("int __swk_0 = (int) (long) __swv_0;"),
        "{java}"
    );
    assert!(java.contains("if (__swk_0 == (long) __swv_0)"), "{java}");
    assert!(java.contains("case -2:"), "{java}");
    assert!(
        java.contains("if (eq(__sw_0, 1l)) __si_0 = 0;"),
        "eq fallback: {java}"
    );
}

#[test]
fn clean_switch_with_out_of_range_or_duplicate_labels_uses_eq_chain() {
    // #72: a label beyond `int` range, or a repeated label, made the native
    // Java switch fail to compile (or match the wrong arm after truncation).
    for src in [
        "var x = 1 switch (x) { case 4294967297: return 'big' case 1: return 'one' } return 'd'",
        "var x = 1 switch (x) { case 1: return 'a' case 1: return 'b' } return 'd'",
    ] {
        let java = java_for(src, &Options::clean(Version::V4, 1));
        assert!(!java.contains("instanceof Long"), "{src}: {java}");
        assert!(!java.contains("((Number)"), "{src}: {java}");
        assert!(java.contains("if (eq(__sw_0, "), "{src}: {java}");
    }
}

#[test]
fn synthesized_identifiers_escape_non_ascii_names() {
    // #94: `g_init_<x>`, `createStaticClass_<C>` and `initClass_<C>` were built
    // from the raw source name instead of the mangled one, so a non-ASCII
    // global or class name emitted an identifier javac rejects.
    let java = java_for(
        "class Ét\u{00e9} { static integer j\u{00f4}ur = 1 }\nglobal caf\u{00e9} = 2\nreturn caf\u{00e9}\n",
        &Options::exact(Version::V4, 1),
    );
    // The only non-ASCII left is inside string literals — the runtime keeps
    // the source spelling there (`addStaticField(this, "j\u{00f4}ur", …)`).
    for line in java.lines().filter(|l| !l.contains('"')) {
        assert!(line.is_ascii(), "non-ASCII identifier: {line}");
    }
    assert!(
        java.contains("private boolean g_init_caf_u00E9 = false;"),
        "{java}"
    );
    assert!(java.contains("g_init_caf_u00E9 = true;"), "{java}");
    assert!(
        java.contains("createStaticClass__u00C9t_u00E9();"),
        "{java}"
    );
    assert!(
        java.contains("private void createStaticClass__u00C9t_u00E9()"),
        "{java}"
    );
    assert!(java.contains("initClass__u00C9t_u00E9();"), "{java}");
    assert!(
        java.contains("private void initClass__u00C9t_u00E9()"),
        "{java}"
    );
}
