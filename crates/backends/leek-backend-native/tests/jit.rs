//! End-to-end JIT tests: source → HIR → Cranelift → run.

use leek_backend_native::{compile, run, NativeArtifact, NativeEmit, NativeError, NativeOptions};
use leek_hir::lower_file_versioned;
use leek_parser::{ast::AstNode, ast::SourceFile, parse};
use leek_span::SourceId;
use leek_syntax::{SyntaxNode, Version};

fn version_of(byte: u8) -> Version {
    match byte {
        1 => Version::V1,
        2 => Version::V2,
        3 => Version::V3,
        _ => Version::V4,
    }
}

fn hir_v(src: &str, version: u8) -> leek_hir::HirFile {
    let source = SourceId::new(1).unwrap();
    let parsed = parse(src, source, version_of(version));
    let file = SourceFile::cast(SyntaxNode::new_root(parsed.green.clone())).expect("parse");
    lower_file_versioned(&file, source, version).0
}

fn hir(src: &str) -> leek_hir::HirFile {
    hir_v(src, 4)
}

fn jit(src: &str) -> String {
    match run(&hir(src), &NativeOptions::debug()) {
        Ok(v) => v.to_string(),
        Err(e) => format!("ERR: {e}"),
    }
}

/// Run `src` under `version` semantics, rendering the result with the
/// matching display version (so v1 real formatting etc. is honored).
fn jit_v(src: &str, version: u8) -> String {
    leek_runtime::DISPLAY_VERSION.with(|c| c.set(version));
    let opts = NativeOptions::debug().with_lang(version, false);
    match run(&hir_v(src, version), &opts) {
        Ok(v) => v.to_string(),
        Err(e) => format!("ERR: {e}"),
    }
}

#[test]
fn native_bodiless_signature_dispatches_by_name() {
    // No directive at all — a bodiless signature whose name is a builtin
    // dispatches that runtime builtin by name (signature-only migration).
    use leek_parser::{ParseFeatures, parse_with_features};
    let prelude_src = "// @experimental: function_signatures\n\
function abs(real x) -> real;\n";
    let user_src = "return abs(-5)\n";
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
    let prelude_ast = SourceFile::cast(SyntaxNode::new_root(p.green)).expect("prelude");
    let u = parse(user_src, source, Version::V4);
    let user_ast = SourceFile::cast(SyntaxNode::new_root(u.green)).expect("user");
    let (h, _d) = leek_hir::lower_file_with_prelude(&user_ast, source, 4, &prelude_ast, prelude_source);
    let out = match run(&h, &NativeOptions::debug()) {
        Ok(v) => v.to_string(),
        Err(e) => format!("ERR: {e}"),
    };
    assert_eq!(out, "5", "abs(-5) via bodiless signature");
}

#[test]
fn native_backend_directive_dispatches_builtin() {
    use leek_parser::{ParseFeatures, parse_with_features};
    // A signature-file `abs` with a `@native-backend:` directive; the
    // call `abs(-5)` dispatches the runtime builtin `abs` (no compiled
    // body), yielding 5.
    let prelude_src = "// @experimental: function_signatures\n\
/** @native-backend: abs */\n\
function abs(real x) -> real;\n";
    let user_src = "return abs(-5)\n";
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
    let prelude_ast = SourceFile::cast(SyntaxNode::new_root(p.green)).expect("prelude");
    let u = parse(user_src, source, Version::V4);
    let user_ast = SourceFile::cast(SyntaxNode::new_root(u.green)).expect("user");
    let (h, _d) = leek_hir::lower_file_with_prelude(&user_ast, source, 4, &prelude_ast, prelude_source);
    let out = match run(&h, &NativeOptions::debug()) {
        Ok(v) => v.to_string(),
        Err(e) => format!("ERR: {e}"),
    };
    assert_eq!(out, "5", "abs(-5) via native directive");
}

#[test]
fn integer_arithmetic() {
    assert_eq!(jit("return 1 + 2 * 3"), "7");
    assert_eq!(jit("return 20 \\ 6"), "3"); // integer division
    assert_eq!(jit("return 20 % 6"), "2");
    assert_eq!(jit("return -5 + 8"), "3");
}

#[test]
fn comparisons_yield_bool() {
    assert_eq!(jit("return 3 < 5"), "true");
    assert_eq!(jit("return 5 < 3"), "false");
    assert_eq!(jit("return 4 == 4"), "true");
}

#[test]
fn locals_and_control_flow() {
    assert_eq!(jit("var x = 10 if (x > 5) { return x } else { return 0 }"), "10");
    assert_eq!(jit("var s = 0 for (var i = 1; i <= 5; i++) { s = s + i } return s"), "15");
    assert_eq!(jit("var n = 0 var i = 0 while (i < 10) { n = n + 2 i = i + 1 } return n"), "20");
}

#[test]
fn reals() {
    assert_eq!(jit("return 6 / 2"), "3.0"); // `/` is real division → real result
    assert_eq!(jit("return 7 / 2"), "3.5");
    assert_eq!(jit("return 1.5 + 2.5"), "4.0");
    assert_eq!(jit("return 1.5 * 2"), "3.0"); // mixed int/real promotes
    assert_eq!(jit("real x = 42 return x"), "42.0");
    assert_eq!(jit("return 3.0 < 3.5"), "true");
    assert_eq!(jit("var x = 0.0 for (var i = 0; i < 4; i++) { x = x + 0.5 } return x"), "2.0");
}

#[test]
fn scalar_math_builtins() {
    // Shared runtime builtins, called via the JIT. sqrt/cbrt are real;
    // floor/ceil/round narrow to int.
    assert_eq!(jit("return sqrt(16.0)"), "4.0");
    assert_eq!(jit("return floor(3.7)"), "3");
    assert_eq!(jit("return ceil(3.2)"), "4");
    assert_eq!(jit("return round(2.5)"), "3");
    assert_eq!(jit("return cbrt(27.0)"), "3.0");
    // Composes with arithmetic and locals.
    assert_eq!(jit("var x = sqrt(9.0) return x + 1"), "4.0");
    assert_eq!(jit("real r = sqrt(2.0) return floor(r * r + 0.5)"), "2");
}

#[test]
fn logical_and_identity_ops() {
    assert_eq!(jit("return true xor false"), "true");
    assert_eq!(jit("return true xor true"), "false");
    assert_eq!(jit("return 1 === 1.0"), "true"); // numeric identity
    assert_eq!(jit("return 2 === 3"), "false");
    assert_eq!(jit("return 2.5 !== 2.5"), "false");
}

#[test]
fn versions() {
    // v1 renders reals with a comma decimal separator.
    assert_eq!(jit_v("return 5 / 2", 1), "2,5");
    assert_eq!(jit_v("return 5 / 2", 4), "2.5");
    // Mixed int/bool `==`: truthiness coercion in v1–v3 (`true`),
    // structurally false in v4.
    assert_eq!(jit_v("return 1 == true", 4), "false");
    assert_eq!(jit_v("return 1 == true", 3), "true");
    assert_eq!(jit_v("return 1 == true", 2), "true");
    assert_eq!(jit_v("return true == 12", 1), "true");
    assert_eq!(jit_v("return false != 0", 3), "false");
    // Strict-mode var coercion (v-independent here, just exercises strict).
    assert_eq!(jit_v("return 7 \\ 2", 4), "3");
    assert_eq!(jit_v("return 10 % 3", 1), "1");
}

#[test]
fn pow_and_poly_builtins() {
    // `**` operator: int**int (const small exp) stays int; any-real → real.
    assert_eq!(jit("return 2 ** 10"), "1024");
    assert_eq!(jit("return 2.0 ** 3"), "8.0");
    assert_eq!(jit("var b = 3 return 2 ** b"), "ERR: unsupported: integer ** with non-constant/large exponent");
    // pow builtin is always real.
    assert_eq!(jit("return pow(2, 10)"), "1024.0");
    assert_eq!(jit("return atan2(0, 1)"), "0.0");
    // abs keeps the kind; signum is int; min/max are polymorphic.
    assert_eq!(jit("return abs(-5)"), "5");
    assert_eq!(jit("return abs(-5.5)"), "5.5");
    assert_eq!(jit("return signum(-3)"), "-1");
    assert_eq!(jit("return max(3, 7)"), "7");
    assert_eq!(jit("return min(3.5, 7)"), "3.5");
    // 0-arg math operates on 0.
    assert_eq!(jit("return sqrt()"), "0.0");
}

#[test]
fn user_function_calls() {
    assert_eq!(jit("function foo() { return 42 } return foo()"), "42");
    assert_eq!(jit("function sq(real x) -> real { return x * x } return sq(3.0)"), "9.0");
    assert_eq!(jit("function add(integer a, integer b) { return a + b } return add(3, 4)"), "7");
    // Calls compose with arithmetic and other calls.
    assert_eq!(jit("function d(integer x) { return x * 2 } return d(5) + d(10)"), "30");
    // Recursion.
    assert_eq!(
        jit("function fact(integer n) { if (n <= 1) { return 1 } return n * fact(n - 1) } return fact(5)"),
        "120"
    );
    // A function returning an array works (the callee's result is a handle).
    assert_eq!(jit("function mk() { return [1, 2] } return mk()"), "[1, 2]");
    // A string-returning callee works (the result is a handle).
    assert_eq!(jit("function greet() { return \"hi\" } return greet()"), "\"hi\"");
}

#[test]
fn arrays() {
    // Literal + display.
    assert_eq!(jit("return [1, 2, 3]"), "[1, 2, 3]");
    assert_eq!(jit("return []"), "[]");
    assert_eq!(jit("return [1.5, 2.5]"), "[1.5, 2.5]");
    // Index read (negative from the end; out-of-range → null).
    assert_eq!(jit("var a = [10, 20, 30] return a[1]"), "20");
    assert_eq!(jit("var a = [10, 20, 30] return a[-1]"), "30");
    assert_eq!(jit("var a = [10, 20, 30] return a[9]"), "null");
    // count + push.
    assert_eq!(jit("var a = [1, 2, 3] return count(a)"), "3");
    assert_eq!(jit("var a = [1, 2] push(a, 3) push(a, 4) return count(a)"), "4");
    assert_eq!(jit("var a = [] push(a, 7) return a[0]"), "7");
    // Index assignment (v4 in-range write; out-of-range is a no-op).
    assert_eq!(jit("var a = [1, 2, 3] a[0] = 5 return a[0]"), "5");
    assert_eq!(jit("var a = [1, 2, 3] a[-1] = 9 return a[2]"), "9");
    assert_eq!(jit("var a = [1, 2, 3] a[9] = 5 return count(a)"), "3");
    assert_eq!(jit("var a = [1, 2, 3] a[1] = 2.5 return a[1]"), "2.5");
    // Typed numeric arrays coerce the written element to the element type.
    assert_eq!(jit("Array<real> a = [0.0] a[0] = 5 return a[0]"), "5.0");
    assert_eq!(jit("Array<integer> a = [0] a[0] = 5.7 return a[0]"), "5");
    // Composites work in v2+; v1 (value semantics) is still gated.
    // v1 composites work (value semantics): an array literal renders the same.
    assert_eq!(jit_v("return [1, 2, 3]", 1), "[1, 2, 3]");
    // v1 copy-on-assign: `b = a` copies, so mutating `b` leaves `a` intact.
    assert_eq!(
        jit_v("var a = [1, 2, 3] var b = a b[0] = 9 return a[0]", 1),
        "1"
    );
    // v1 pass-by-value: a function mutating its array arg doesn't affect the caller's.
    assert_eq!(
        jit_v("function f(x) { x[0] = 9 } var a = [1, 2, 3] f(a) return a[0]", 1),
        "1"
    );
    assert_eq!(jit_v("var a = [1] a[0] = 2 return a[0]", 2), "2");
    assert_eq!(jit_v("return [1, 2, 3]", 3), "[1, 2, 3]");
}

#[test]
fn dynamic_value_ops() {
    // Arithmetic on array elements now dispatches at runtime.
    assert_eq!(jit("var a = [10, 20, 30] return a[0] + a[2]"), "40");
    assert_eq!(jit("var a = [10, 20, 30] return a[1] * 2"), "40");
    assert_eq!(jit("var a = [3, 4] return a[0] < a[1]"), "true");
    assert_eq!(jit("var a = [1.5, 2.5] return a[0] + a[1]"), "4.0");
    // Mixed element + scalar.
    assert_eq!(jit("var a = [7] return a[0] + 3"), "10");
}

#[test]
fn foreach_loops() {
    assert_eq!(jit("var s = 0 for (var x in [1, 2, 3, 4]) { s = s + x } return s"), "10");
    assert_eq!(jit("var p = 1 for (var x in [1, 2, 3, 4]) { p = p * x } return p"), "24");
    // Sum of a real array.
    assert_eq!(jit("var s = 0.0 for (var x in [1.5, 2.5]) { s = s + x } return s"), "4.0");
    // Build an array in the loop.
    assert_eq!(jit("var a = [] for (var x in [1, 2, 3]) { push(a, x) } return count(a)"), "3");
    // v1 is gated (value semantics); v2+ works.
    assert_eq!(
        jit_v("var s = 0 for (var x in [1, 2]) { s = s + x } return s", 1),
        "3"
    );
}

#[test]
fn strings() {
    // Literal (top-level strings render quoted, like the interpreter).
    assert_eq!(jit("return 'hello'"), "\"hello\"");
    // Concatenation (via the shared dynamic value-op → apply_binary).
    assert_eq!(jit("return 'ab' + 'cd'"), "\"abcd\"");
    assert_eq!(jit("return 'x' + 5"), "\"x5\"");
    assert_eq!(jit("return 5 + 'x'"), "\"5x\"");
    // Length and indexing (a one-char substring).
    assert_eq!(jit("return count('hello')"), "5");
    assert_eq!(jit("var s = 'abc' return s[1]"), "\"b\"");
    // foreach over a string, building a reversed copy.
    assert_eq!(jit("var s = '' for (var c in 'abc') { s = c + s } return s"), "\"cba\"");
    // v1 is gated; v2+ works.
    assert_eq!(jit_v("return 'a' + 'b'", 1), "\"ab\"");
}

#[test]
fn maps_and_sets() {
    // Map literal, indexing (int and string keys), display.
    assert_eq!(jit("var m = [1: 10, 2: 20] return m[1]"), "10");
    assert_eq!(jit("return [1: 10, 2: 20]"), "[1 : 10, 2 : 20]");
    assert_eq!(jit("var m = ['a': 1, 'b': 2] return m['b']"), "2");
    // Set literal (dedups), count, display.
    assert_eq!(jit("var s = <1, 2, 3> return count(s)"), "3");
    assert_eq!(jit("return <1, 2, 2, 3>"), "<1, 2, 3>");
    // foreach over a map iterates values.
    assert_eq!(jit("var t = 0 for (var v in [1: 10, 2: 20]) { t = t + v } return t"), "30");
    // Map index-assignment, compound-assignment, and reference aliasing.
    assert_eq!(jit("var m = ['a': 10] m['a'] = 99 return m['a']"), "99");
    assert_eq!(jit("var m = ['a': 10] m['a'] += 5 return m['a']"), "15");
    assert_eq!(jit("var m = ['a': 1] var b = m b['c'] = 2 return count(m)"), "2");
    assert_eq!(jit("var m = [:] m[1] = 'x' return m[1]"), "\"x\"");
    // v2+ works; v1 is gated. v1–v3 map lookups coerce real keys.
    assert_eq!(jit_v("return [1: 2]", 1), "[1 : 2]");
    assert_eq!(jit_v("var m = [5: 12] return m[5.7]", 2), "12");
}

#[test]
fn intervals() {
    assert_eq!(jit("return [1..5]"), "[1..5]");
    assert_eq!(jit("return [2..2]"), "[2..2]");
    // foreach over an interval walks it in unit steps.
    assert_eq!(jit("var s = 0 for (var x in [1..4]) { s = s + x } return s"), "10");
    assert_eq!(jit("var n = 0 for (var x in [1..10]) { n = n + 1 } return n"), "10");
    // v4-only for now.
    assert_eq!(jit_v("return [1..5]", 1), "[1..5]");
}

#[test]
fn stdlib_builtins() {
    // Stdlib builtins now dispatch through the shared catalog in
    // `leek-runtime` (native links it via a trivial host).
    assert_eq!(jit("return 'hello'.substring(1, 3)"), "\"ell\"");
    assert_eq!(jit("return 'hello'.toUpper()"), "\"HELLO\"");
    assert_eq!(jit("return charAt('abc', 1)"), "\"b\"");
    assert_eq!(jit("return 'ab'.startsWith('a')"), "true");
    assert_eq!(jit("return 'a,b,c'.split(',')"), "[\"a\", \"b\", \"c\"]");
    assert_eq!(jit("return ['a', 'b', 'c'].join('-')"), "\"a-b-c\"");
}

#[test]
fn untyped_function_params() {
    assert_eq!(jit("function add(a, b) { return a + b } return add(3, 4)"), "7");
    assert_eq!(jit("function inc(x) { return x + 1 } return inc(41)"), "42");
    assert_eq!(jit("function pick(a, b, c) { return a } return pick(7, 8, 9)"), "7");
}

#[test]
fn object_literals() {
    assert_eq!(jit("return {x: 5}"), "{x: 5}");
    assert_eq!(jit("var o = {a: 1, b: 2} return o.a"), "1");
    assert_eq!(jit("var o = {a: 1} o.a = 9 return o.a"), "9");
    assert_eq!(jit_v("return {a: 1}", 1), "{a: 1}");
}

#[test]
fn globals() {
    assert_eq!(jit("global g = 10 return g"), "10");
    assert_eq!(jit("global g = 5 g = g + 1 return g"), "6");
    assert_eq!(jit("global a = 1 global b = 2 return a + b"), "3");
}

#[test]
fn release_profile_matches_debug() {
    assert_eq!(
        run(&hir("var s = 0 for (var i = 0; i < 100; i++) { s = s + i } return s"), &NativeOptions::release()).map(|v| v.to_string()).unwrap(),
        "4950"
    );
}

#[test]
fn escaping_byref_capture_threads_caller_cell() {
    // A v1 `@a` by-ref param captured by a returned closure that mutates it is
    // threaded as a shared `Value::Cell`: the caller passes its cell, the param
    // reuses it (`leek_make_cell`), the escaped closure mutates it, and the
    // caller observes the change. (Was formerly skipped; now implemented.)
    let f = "var f = function(@a) { return function() { a += 2 } }; var x = 10 f(x)() return x";
    assert_eq!(
        run(&hir(f), &NativeOptions::debug().with_lang(1, false))
            .map(|v| v.to_string())
            .unwrap(),
        "12"
    );
    // Same shape with a named (non-lambda) outer function, called directly.
    let g = "function f(@a) { return function() { a += 2 } }; var x = 10 f(x)() return x";
    assert_eq!(
        run(&hir(g), &NativeOptions::debug().with_lang(1, false))
            .map(|v| v.to_string())
            .unwrap(),
        "12"
    );
}

#[test]
fn unsupported_constructs_are_reported() {
    // A reassigned `@x` by-ref param on a *method* needs the caller's cell
    // threaded through a `Callee::Method` call — which the cell-threading
    // (`byref_cells_threadable`, restricted to plain `Callee::Function`) does
    // not handle. Native must skip (report Unsupported), never miscompile.
    assert!(matches!(
        run(
            &hir("class A { m(@x) { x = 9 } } var o = new A() var n = 5 o.m(n) return n"),
            &NativeOptions::debug().with_lang(1, false)
        ),
        Err(NativeError::Unsupported(_))
    ));
}

#[test]
fn classes() {
    // Field read after constructor.
    assert_eq!(
        jit("class A { public integer x = 0 constructor(v) { this.x = v } public get() { return this.x } } var a = new A(7) return a.get()"),
        "7"
    );
    // Field initializer coerced to the declared type (`real x = 12` → 12.0).
    assert_eq!(
        jit("class A { public real x = 12 } var a = new A() return a.x"),
        "12.0"
    );
    // Constructor arg + mutating method + method-to-method state.
    assert_eq!(
        jit("class Counter { public integer n = 0 constructor(s) { this.n = s } public inc() { this.n = this.n + 1 return this.n } } var c = new Counter(10) c.inc() return c.inc()"),
        "12"
    );
    // Inheritance: overridden method + inherited protected field + ctor.
    assert_eq!(
        jit("class Animal { protected string name = \"?\" constructor(n) { this.name = n } public speak() { return this.name + \" makes a sound\" } } class Dog extends Animal { public speak() { return this.name + \" barks\" } } var d = new Dog(\"Rex\") return d.speak()"),
        "\"Rex barks\""
    );
    // A method calling another method on `this`.
    assert_eq!(
        jit("class M { public a() { return this.b() + 1 } public b() { return 10 } } var m = new M() return m.a()"),
        "11"
    );
}

#[test]
fn hof_with_lambda_and_named_function() {
    // Lambda callbacks.
    assert_eq!(jit("var a = [1,2,3]; return arrayMap(a, x -> x * 10)[2]"), "30");
    assert_eq!(jit("var a = [1,2,3,4]; return count(arrayFilter(a, x -> x > 2))"), "2");
    assert_eq!(jit("var a = [1,2,3,4]; return arrayFoldLeft(a, (acc, x) -> acc + x, 0)"), "10");
    // Named user function passed as a value.
    assert_eq!(jit("function dbl(x){return x*2;} var a=[1,2,3]; return arrayMap(a, dbl)[1]"), "4");
    // Value-capture closure.
    assert_eq!(jit("var n = 5; var f = x -> x + n; return f(10)"), "15");
}

#[test]
fn hof_with_builtin_as_value() {
    // A builtin function passed by name as a HOF callback — boxed as a
    // `Function::Builtin` handle and dispatched by the runtime.
    assert_eq!(jit("var a = [-1, -5]; return arrayMap(a, abs)[1]"), "5");
    assert_eq!(jit("var a = [4.0, 9.0]; return arrayMap(a, sqrt)[1]"), "3.0");
    // As a plain function value, then called.
    assert_eq!(jit("var f = abs; return f(-7)"), "7");
}

#[test]
fn readonly_closure_capture_works() {
    // Read-only capture (value at call time) is supported.
    assert_eq!(jit("var n = 7; var f = x -> x + n; return f(3)"), "10");
}

#[test]
fn shared_mutation_through_cells_works() {
    // A captured *scalar* counter mutated through a closure (cell semantics).
    assert_eq!(
        jit("var c = 0; var inc = function(){ c = c + 1 }; for (var i = 0; i < 100; i++) { inc() } return c"),
        "100"
    );
    // A captured *composite* mutated through one closure and read through
    // another — both closures must share the same underlying cell.
    assert_eq!(
        jit("var shared = [0]; var inc = function(){ shared[0]++ }; var get = function(){ return shared[0] }; for (var i = 0; i < 100; i++) { inc() } return get()"),
        "100"
    );
    // Reassigning a captured int-typed local to a string through a closure:
    // the cell is dynamically typed, so the result is the string (not a
    // narrowed coercion of it).
    assert_eq!(
        jit("var toto = 1; var set = function(){ toto = 'hi' }; set(); return toto"),
        "\"hi\""
    );
}

#[test]
fn rng_builtins_draw_from_a_persistent_sequence() {
    // Range checks (the shape of the upstream RNG tests).
    assert_eq!(jit("var a = rand() return a >= 0 and a < 1"), "true");
    assert_eq!(jit("var a = randInt(5, 10) return a >= 5 and a <= 10"), "true");
    assert_eq!(jit("var a = randFloat(500, 510) return a >= 500 and a < 510"), "true");
    // The generator advances across calls (no per-call reset) — two draws
    // from a wide range differ.
    assert_eq!(jit("var a = randInt(0, 1000000); var b = randInt(0, 1000000); return a != b"), "true");
}

#[test]
fn in_place_collection_builtins() {
    assert_eq!(jit("var r = ['a','b','c','a']; arrayRemoveAll(r, 'a'); return count(r)"), "2");
    assert_eq!(jit("var m = [:]; mapFill(m, 5); return count(m)"), "0");
    assert_eq!(jit("debug('hi'); return 42"), "42");
}

#[test]
fn builtin_class_call_form_constructs() {
    // A bare builtin-class name called as a function is constructor sugar,
    // mirroring the interpreter (`Array(1,2)` == `[1,2]`).
    assert_eq!(jit("var a = Array(); push(a, 7); return count(a)"), "1");
    assert_eq!(jit("var m = Map(); return count(m)"), "0");
    assert_eq!(jit("return count(Array(1, 2, 3))"), "3");
}

#[test]
fn method_call_on_constructor_function_local() {
    // `var a = A()` is `new A()`; the receiver's class propagates through the
    // temp so `a.m()` dispatches statically (transitive new_class_locals).
    assert_eq!(
        jit("class A { integer x = 10 m() { return x } } var a = A(); return a.m()"),
        "10"
    );
}

#[test]
fn static_method_as_value() {
    // `var f = C.staticMethod` boxes a Function::User handle; calling it
    // dispatches indirectly to the uniform-compiled method body.
    assert_eq!(jit("class A { static m() { return 42 } } var f = A.m; return f()"), "42");
    assert_eq!(
        jit("class A { static add(x, y) { return x + y } } var f = A.add; return f(3, 4)"),
        "7"
    );
    // Passed as a HOF callback.
    assert_eq!(
        jit("class A { static dbl(x) { return x * 2 } } var a = [1, 2, 3]; return arrayMap(a, A.dbl)[2]"),
        "6"
    );
}

#[test]
fn static_field_callable() {
    // `A.a()` where `a` is a static field holding a lambda: read the field
    // and invoke it indirectly.
    assert_eq!(jit("class A { static a = -> 12 } return A.a()"), "12");
    assert_eq!(jit("class A { static f = x -> x * 2 } return A.f(5)"), "10");
}

#[test]
fn method_default_arguments() {
    // An instance method called with fewer args than params pads the omitted
    // trailing param from its self-contained default.
    assert_eq!(
        jit("class A { m(x = 2) { return x } } return new A().m()"),
        "2"
    );
    // Explicit arg overrides the default; partial defaults work too.
    assert_eq!(
        jit("class A { m(a, b = 10) { return a + b } } var o = new A(); return o.m(5)"),
        "15"
    );
    // Composite method default.
    assert_eq!(
        jit("class A { m(arr = [1, 2, 3]) { return count(arr) } } return new A().m()"),
        "3"
    );
}

#[test]
fn virtual_method_dispatch() {
    // `this.m()` inside a base method dispatches to the receiver's RUNTIME
    // class's override.
    assert_eq!(
        jit("class A { m() { return 'parent' } t() { return this.m() } } class B extends A { m() { return 'child' } } return new B().t()"),
        "\"child\""
    );
    // A base instance still gets the base method.
    assert_eq!(
        jit("class A { m() { return 'parent' } t() { return this.m() } } class B extends A { m() { return 'child' } } return new A().t()"),
        "\"parent\""
    );
    // Multi-level: C inherits B's override.
    assert_eq!(
        jit("class A { v() { return 1 } t() { return this.v() } } class B extends A { v() { return 2 } } class C extends B {} return new C().t()"),
        "2"
    );
}

#[test]
fn object_literal_method_calls() {
    // A field holding a function/static-method/lambda is invoked.
    assert_eq!(
        jit("class A { static m() { return 12 } } var r = {x: A.m} return r.x()"),
        "12"
    );
    assert_eq!(jit("var o = {add: (a, b) -> a + b} return o.add(3, 4)"), "7");
    // A missing field name is a builtin method on the object.
    assert_eq!(jit("return {a: 5, b: 6}.keys()"), r#"["a", "b"]"#);
    // A builtin-class field constructs.
    assert_eq!(jit("var o = {c: Array} return count(o.c(1, 2, 3))"), "3");
}

#[test]
fn non_constant_default_arguments() {
    // Default calls a function — the callee fills it at entry via the hidden
    // `argc` param (callee-fills-own-defaults).
    assert_eq!(
        jit("function g() { return 7 } function f(x, y = g()) { return x + y } return f(3)"),
        "10"
    );
    // Default references an earlier parameter.
    assert_eq!(jit("function f(x, y = x + 1) { return x * y } return f(5)"), "30");
    // Explicit arg overrides a non-const default.
    assert_eq!(jit("function f(x, y = x + 1) { return x * y } return f(5, 10)"), "50");
    // Chained defaults: a later default reads an earlier filled one.
    assert_eq!(
        jit("function f(x, y = x + 1, z = y + 1) { return x + y + z } return f(5)"),
        "18"
    );
    // Static method default calling another static method.
    assert_eq!(
        jit("class A { static v() { return 55 } static m(x, y = v()) { return x * y } } return A.m(2)"),
        "110"
    );
    // Instance method default calling an instance method (v2+).
    assert_eq!(
        jit("class A { v() { return 55 } m(x, y = v()) { return x * y } } return new A().m(2)"),
        "110"
    );
    // Constructor with a non-const default.
    assert_eq!(
        jit("class A { f static v() { return 55 } constructor(x, y = v()) { this.f = x * y } } return new A(2).f"),
        "110"
    );
    // A multi-block (ternary / conditional) default is filled too.
    assert_eq!(jit("function f(x, y = x > 0 ? 10 : 20) { return x + y } return f(5)"), "15");
    assert_eq!(jit("function f(x, y = x > 0 ? 10 : 20) { return x + y } return f(-3)"), "17");
}

#[test]
fn aliased_class_receiver_dispatch() {
    // `var x = this; x.m()` — the alias's class is tracked for dispatch.
    assert_eq!(
        jit("class A { private m() { return 5 } public n() { var x = this return x.m() } } return new A().n()"),
        "5"
    );
    // `(obj as C).m()` — a cast of a known instance keeps the class.
    assert_eq!(
        jit("class T { public boolean no() { return false } } var t = new T() return (t as T).no()"),
        "false"
    );
    // Dispatch through an alias is VIRTUAL: a typed-A param holding a B
    // resolves B's override.
    assert_eq!(
        jit("class A { m() { return \"p\" } } class B extends A { m() { return \"c\" } } class C { f(A obj) { var x = obj return x.m() } } return new C().f(new B())"),
        "\"c\""
    );
}

#[test]
fn class_super_on_runtime_value() {
    // `.super` on a runtime class value (`x.class.super`) navigates the
    // hierarchy at runtime.
    assert_eq!(
        jit("class A {} class B extends A {} return new B().class.super.name"),
        "\"A\""
    );
    // Builtin classes extend the `Value` root.
    assert_eq!(jit("class A {} return A.class.super"), "<class Value>");
    assert_eq!(jit("class A {} return A.class.super.super"), "null");
}

#[test]
fn v1_division_by_zero_is_null() {
    // v1 division by a statically-zero divisor is `null`, not infinity.
    assert_eq!(jit_v("return 8 / 0", 1), "null");
    assert_eq!(jit_v("return 8 / 0 === null", 1), "true");
    assert_eq!(jit_v("return 0 / 0 === NaN", 1), "false");
}

#[test]
fn class_ref_index_member() {
    // `A['m']` resolves a member like `A.m` (static/instance method as value).
    assert_eq!(jit("class A { static m() { return 42 } } return A[\"m\"]()"), "42");
    assert_eq!(
        jit("class A { m(x) { return x * 2 } } var f = A[\"m\"] return f(new A(), 9)"),
        "18"
    );
}

#[test]
fn class_ref_called_constructs() {
    // A class ref called directly is constructor sugar (`clazz()` == `new A()`).
    assert_eq!(jit("class A {} Class clazz = A return clazz()"), "A {}");
    assert_eq!(
        jit("class A { x constructor(x) { this.x = x } } Class clazz = A return clazz(7)"),
        "A {x: 7}"
    );
    assert_eq!(jit("class A { x = 5 } var c = A return c()"), "A {x: 5}");
}

#[test]
fn method_not_found_falls_back_to_builtin() {
    // No user method, no field → builtin-method sugar; an unknown method or a
    // math builtin on a non-number instance yields null (not a coerced 0).
    assert_eq!(
        jit("class A { public m() { return this.unknownMethod() } } return new A().m()"),
        "null"
    );
    assert_eq!(jit("class A {} return new A().sqrt()"), "null");
    assert_eq!(jit("class A {} return sqrt(new A())"), "null");
    // A genuine builtin method on an instance still works.
    assert_eq!(jit("class A { x = 5 } return new A().string()"), "\"A {x: 5}\"");
}

#[test]
fn as_casts() {
    assert_eq!(jit("Array<Array<real>> a = [[1.0, 7.0]]; integer b = a[0][1] as integer; return b"), "7");
    assert_eq!(jit("return 5.9 as integer"), "5.9");
    assert_eq!(jit("return [1, 2] as Array<integer>"), "[1, 2]");
    assert_eq!(jit("real r = 3 as real; return r"), "3.0");
}

#[test]
fn composite_default_arguments() {
    // A self-contained composite-literal default is folded at compile time.
    assert_eq!(
        jit("function f(any a = [1, [2, 3]]) { return a[1][0] } return f()"),
        "2"
    );
    // Const arithmetic inside a map-literal default.
    assert_eq!(
        jit("function g(any b = ['x' : (1 + 2) * 3]) { return b['x'] } return g()"),
        "9"
    );
    // Fresh per call: a callee mutating its array default must not leak across
    // calls (each call deep-clones the folded default).
    assert_eq!(
        jit("function h(a = []) { push(a, 1); return count(a) } h(); return h()"),
        "1"
    );
    // An explicit arg still overrides the default.
    assert_eq!(
        jit("function k(any a = [9]) { return a[0] } return k([7])"),
        "7"
    );
}

#[test]
fn branch_on_inline_ref_const() {
    // `if (null)` branches through `leek_truthy` (null is falsy). The shim is
    // declared even when no Ref *local* is present (inline-const condition).
    assert_eq!(jit("if (null) { return 1 } return 2"), "2");
    assert_eq!(jit("if (!null) { return 1 } return 2"), "1");
}

#[test]
fn super_method_dispatch() {
    // `super.m()` from a subclass calls the parent's method statically with
    // the real `this` as receiver.
    assert_eq!(
        jit("class A { m() { return 1 } } class B extends A { m() { return super.m() + 10 } } var b = new B(); return b.m()"),
        "11"
    );
    // `super.m()` reaches a method that reads `this`-state via an override
    // chain (the parent method runs against the actual instance).
    assert_eq!(
        jit("class A { integer v = 7 base() { return v } } class B extends A { base() { return super.base() * 2 } } var b = new B(); return b.base()"),
        "14"
    );
}

#[test]
fn recursive_var_lambda_self_reference() {
    // `var f = function(){ f(...) }` captures its own binding. With cells the
    // binding is a shared `Value::Cell` the lambda captured by raw handle, so
    // the self-recursive call resolves (the `LambdaCapture` patch is a no-op).
    assert_eq!(
        jit("var fact = function(x) { if (x <= 1) { return 1 } return x * fact(x - 1) }; return fact(5)"),
        "120"
    );
}

#[test]
fn poly_builtins_on_dynamic_values() {
    // `abs`/`signum` on a boxed/dynamic arg route through the shared
    // `call_builtin` (the inline int-vs-real form can't apply).
    assert_eq!(jit("any x = -5; return abs(x)"), "5");
    assert_eq!(jit("any x = -3; return signum(x)"), "-1");
    assert_eq!(jit("integer | null x = null; if (x != null) { return abs(x) } return abs(-7)"), "7");
}

#[test]
fn four_arg_builtin_dispatch() {
    // `arraySlice(a, start, end, step)` — a 4-arg builtin (via leek_builtin4).
    assert_eq!(jit("return arraySlice([1,2,3,4,5,6,7,8], 0, 4, 2)[1]"), "3");
}

#[test]
fn builtin_class_instanceof_static_and_new() {
    // `instanceof` against a builtin class (shared value_instanceof).
    assert_eq!(jit("return [:] instanceof Map"), "true");
    assert_eq!(jit("return true instanceof Boolean"), "true");
    assert_eq!(jit("return 5 instanceof Integer"), "true");
    assert_eq!(jit("var c = Array; return c instanceof Class"), "true");
    // Static fields on a builtin class.
    assert_eq!(jit("return Integer.MAX_VALUE"), "9223372036854775807");
    assert_eq!(jit("var x = Real.NaN; return x != x"), "true");
    // `new <BuiltinClass>(args)`.
    assert_eq!(jit("var a = new Array(); push(a, 5); return count(a)"), "1");
    assert_eq!(jit("var o = new Object(); return o instanceof Object"), "true");
    assert_eq!(jit("return new Integer(7)"), "7");
    assert_eq!(jit("return Number.name"), "\"Number\"");
    assert_eq!(jit("return Array.fields"), "[]");
}

#[test]
fn index_of_non_composite_is_null() {
    // Indexing a scalar yields null (matching the interpreter), not a skip.
    assert_eq!(jit("var a = 5; return a[0] == null"), "true");
    assert_eq!(jit("var a = 5; return a[0] + 2"), "2");
}

#[test]
fn mixed_value_null_return() {
    // An untyped function that returns a value on some paths and falls
    // through (null) on others compiles with a boxed result.
    assert_eq!(jit("function f(x) { if (x > 0) { return x } } return f(5)"), "5");
    assert_eq!(jit("function f(x) { if (x > 0) { return x } } return f(-1)"), "null");
    assert_eq!(jit("function f(x) { if (x > 0) { return x } } var r = f(-1); return r == null"), "true");
}

#[test]
fn constant_default_arguments() {
    // Omitted trailing args with self-contained constant defaults are
    // padded at the call site; supplied args win.
    assert_eq!(jit("function f(x = 2) { return x } return f()"), "2");
    assert_eq!(jit("function f(x = 2) { return x } return f(7)"), "7");
    assert_eq!(jit("function f(a, b = 10) { return a + b } return f(5)"), "15");
    assert_eq!(jit("function g(s = 'hi') { return s } return g()"), "\"hi\"");
    // Constructor defaults.
    assert_eq!(jit("class A { f constructor(x = 2) { f = x } } return new A().f"), "2");
    assert_eq!(jit("class A { f constructor(x = 2) { f = x } } return new A(9).f"), "9");
    assert_eq!(jit("class A { s constructor(a, b = 3) { s = a + b } } return new A(10).s"), "13");
}

#[test]
fn class_meta_property() {
    // `.class` on any value yields its runtime class (a class value).
    assert_eq!(jit("return [0..1].class == Interval"), "true");
    assert_eq!(jit("return (5).class.name"), "\"Integer\"");
    assert_eq!(jit("var a = [1,2]; return a.class == Array"), "true");
    // A user instance's class is a ClassRef carrying its name.
    assert_eq!(jit("class A {} var o = new A(); return o.class.name"), "\"A\"");
    assert_eq!(jit("class A {} var o = new A(); return o.class instanceof Class"), "true");
}

#[test]
fn typed_map_value_coercion() {
    // `Map<K, real>` coerces an int write to real; `Map<K, integer>`
    // truncates a real write — like typed arrays.
    assert_eq!(jit("Map<integer, real> m = new Map(); m[1] = 5; return m[1]"), "5.0");
    assert_eq!(jit("Map<integer, integer> m = new Map(); m[1] = 5.7; return m[1]"), "5");
}

#[test]
fn emit_clif_dumps_ir() {
    let mut opts = NativeOptions::debug();
    opts.emit = NativeEmit::Clif;
    let NativeArtifact::Text(ir) = compile(&hir("return 1 + 2"), &opts).unwrap() else {
        panic!("expected CLIF text");
    };
    assert!(ir.contains("function"), "CLIF should render a function: {ir}");
    assert!(ir.contains("return"), "CLIF should contain a return: {ir}");
}





#[test]
fn byref_param_in_place_mutation() {
    // `@t` by-ref param: in-place mutation propagates to the caller via the
    // shared Rc (no cell needed). Reassigning a `@t` param still skips.
    assert_eq!(jit("function f(@t) { push(t, 9) } var a = [1, 2]; f(a); return a"), "[1, 2, 9]");
    assert_eq!(jit("function f(@t) { t[0] = 99 } var a = [1, 2]; f(a); return a"), "[99, 2]");
    // `@a` expression alias (already worked) — kept as a regression guard.
    assert_eq!(jit("var a = [1, 2]; var b = @a; push(b, 5); return a"), "[1, 2, 5]");
}

#[test]
fn v1_read_only_byref_param_compiles() {
    // In v1, a `@c` by-ref param that is only ever READ (never written,
    // passed to a call, captured, or returned by reference) is identical
    // to a value param, so it must compile rather than skip on the blanket
    // v1 by-ref gate. (`needs_cell_semantics` only gates v1 by-ref params
    // that are actually mutated or escape.)
    assert_eq!(jit_v("function t(@c) { var cell = c; if (cell != null) 1; } return t(300)", 1), "null");
    assert_eq!(jit_v("function t(@c) { var cell = c return cell != null } return t(300)", 1), "true");
    assert_eq!(jit_v("function t(@a) {} t([[12], [12]]) return 7", 1), "7");
    // A v1 by-ref param mutated *in place* (`push`) propagates through the
    // shared `Rc` — the call site suppresses the by-ref arg's deep-clone.
    assert_eq!(jit_v("function f(@t) { push(t, 9) } var a = []; f(a); return a", 1), "[9]");
    // A *reassigned* v1 by-ref param is now cell-threaded (the caller passes
    // its shared cell, the rebind's cell-write propagates back) — `a` becomes
    // `[9]`, matching the interpreter's by-reference semantics.
    assert_eq!(jit_v("function f(@t) { t = [9] } var a = []; f(a); return a", 1), "[9]");
}

#[test]
fn native_byref_scalar_mutation_does_not_miscompile() {
    // Skip-don't-miscompile pin for `local_mutated_or_escapes`: `@x` is a
    // by-reference param, so reassigning it inside `f` must propagate to the
    // caller (`a` becomes 2). The native handle model can't express scalar
    // by-ref, so it must SKIP — and must *never* silently compile it as
    // by-value, which would yield the WRONG answer `1`.
    let out = jit_v("function f(@x) { x = 2 } var a = 1 f(a) return a", 1);
    assert_ne!(
        out, "1",
        "skip-don't-miscompile violated: @x by-ref compiled as by-value",
    );
    assert!(
        out.starts_with("ERR") || out == "2",
        "expected a skip (ERR) or the correct by-ref result (2), got {out}",
    );
}

#[test]
fn indirect_lambda_under_arity_call_binds_null_not_segfault() {
    // Regression: an indirect call to a lambda with FEWER args than its
    // parameters (`(x => x)()`) used to leave the uniform-ABI `argv` short, so
    // the body's out-of-bounds load of the missing `x` hard-faulted (SIGSEGV).
    // The dispatch now pads missing user params with null (matching the
    // interpreter's lax arity), so the call returns `x = null` cleanly.
    assert_eq!(jit("return (x => x)()"), "null");
    // A two-param lambda called with one arg: the second binds to null.
    assert_eq!(jit("var f = (a, b) => b; return f(1)"), "null");
    // A named-function ref invoked under-arity through a value is likewise
    // padded (the missing param binds to null).
    assert_eq!(jit("function f(x) { return x } var g = f; return g()"), "null");
}

#[test]
fn v4_strict_out_of_bounds_array_write_is_runtime_error() {
    // v4-strict: an out-of-bounds array write faults with ARRAY_OUT_OF_BOUND.
    // The JIT has no exception path, so a runtime shim records the fault and
    // `run()` surfaces it as `NativeError::Runtime` after `main` returns.
    let strict_v4 = |src: &str| -> String {
        leek_runtime::DISPLAY_VERSION.with(|c| c.set(4));
        match run(&hir_v(src, 4), &NativeOptions::debug().with_lang(4, true)) {
            Ok(v) => v.to_string(),
            Err(e) => format!("ERR: {e}"),
        }
    };
    assert!(strict_v4("var a = [1, 2, 3] return a[100] = 12").contains("ARRAY_OUT_OF_BOUND"));
    assert!(strict_v4("var a = [1, 2, 3] return a[-100] = 12").contains("ARRAY_OUT_OF_BOUND"));
    // An in-bounds write is unaffected.
    assert_eq!(strict_v4("var a = [1, 2, 3] a[1] = 9 return a"), "[1, 9, 3]");
    // Non-strict v4 silently drops the OOB write (no error), matching upstream.
    assert_eq!(jit_v("var a = [1, 2, 3] a[100] = 12 return a", 4), "[1, 2, 3]");
}
