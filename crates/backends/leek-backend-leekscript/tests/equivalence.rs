//! End-to-end correctness: emitting then re-lowering produces valid
//! *official* LeekScript (no experimental features) that runs identically.
//!
//! For each program we:
//!   1. lower it with the experimental features ON, and JIT-run it;
//!   2. emit it (pretty / compact / optimized);
//!   3. re-parse + re-lower the emitted text with **all features OFF**,
//!      asserting no error diagnostics (proves it is official LeekScript);
//!   4. JIT-run the re-lowered program and assert the result matches (1).

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use leek_backend_leekscript::{Options, emit};
use leek_backend_native::{NativeOptions, run};
use leek_diagnostics::Severity;
use leek_hir::HirFile;
use leek_hir::lower::lower_file_versioned_with_flags;
use leek_hir::pipeline::HirArtifact;
use leek_hir::{LowerUnit, lower_files};
use leek_parser::{ParseFeatures, ast::AstNode, ast::SourceFile, parse, parse_with_features};
use leek_pipeline::Input;
use leek_recipes::{RecipeParams, pipeline_hir_with_includes};
use leek_resolver::folder::MemFolder;
use leek_resolver::pipeline::ResolveIncludes;
use leek_span::{FeatureFlags, SourceId};
use leek_syntax::{SyntaxNode, Version};

fn all_parse_features() -> ParseFeatures {
    ParseFeatures {
        function_signatures: true,
        generics: true,
        types: true,
        interfaces: true,
        enums: true,
    }
}

fn experimental_flags() -> FeatureFlags {
    FeatureFlags {
        function_signatures: true,
        generic_syntax: true,
        generics: true,
        overloads: true,
        // Prelude seeding is orthogonal to desugaring; keep it off so the
        // test program is exactly what we wrote.
        prelude: false,
        types: true,
        interfaces: true,
        enums: true,
    }
}

const SOURCE: SourceId = match SourceId::new(1) {
    Some(s) => s,
    None => unreachable!(),
};

/// Lower `src` to HIR at `version`, returning the HIR and whether any error
/// diagnostic was produced (parse + lower).
fn lower_at(src: &str, version: Version, pf: ParseFeatures, ff: FeatureFlags) -> (HirFile, bool) {
    let parsed = parse_with_features(src, SOURCE, version, pf);
    let file = SourceFile::cast(SyntaxNode::new_root(parsed.green.clone())).expect("parse root");
    let (hir, diags) = lower_file_versioned_with_flags(&file, SOURCE, u8::from(version), ff);
    let had_error = parsed
        .diagnostics
        .iter()
        .chain(diags.iter())
        .any(|d| d.severity == Severity::Error);
    (hir, had_error)
}

/// [`lower_at`] at the current language version.
fn lower(src: &str, pf: ParseFeatures, ff: FeatureFlags) -> (HirFile, bool) {
    lower_at(src, Version::V4, pf, ff)
}

fn jit(hir: &HirFile) -> String {
    match run(hir, &NativeOptions::debug()) {
        Ok(v) => v.to_string(),
        Err(e) => format!("ERR: {e}"),
    }
}

/// Assert that emitting `src` (which may use experimental features) yields
/// official LeekScript that re-lowers cleanly and runs identically, across
/// pretty / compact / optimized modes.
fn check(src: &str) {
    check_at(src, Version::V4);
}

/// [`check`] at an explicit language version: the program is lowered,
/// emitted, and re-lowered all at `version`, so a construct whose meaning
/// is version-dependent has to survive the round-trip on its own terms.
fn check_at(src: &str, version: Version) {
    let (orig, orig_err) = lower_at(src, version, all_parse_features(), experimental_flags());
    assert!(
        !orig_err,
        "original program failed to lower at {version:?}:\n{src}"
    );
    check_hir_at(&orig, src, version);
}

/// The body of [`check`] on an already-lowered program, so HIR built by
/// hand (multi-file projects) goes through the same round-trip.
/// `src` is only used for comment recovery and failure messages.
fn check_hir(orig: &HirFile, src: &str) {
    check_hir_at(orig, src, Version::V4);
}

fn check_hir_at(orig: &HirFile, src: &str, version: Version) {
    let expected = jit(orig);

    let configs = [
        ("pretty", Options::pretty(version).with_source_text(src)),
        ("compact", Options::compact(version)),
        ("optimized", Options::pretty(version).with_optimize(true)),
    ];

    for (label, opts) in configs {
        let out = emit(orig, &opts).source;
        let (rel, err) = lower_at(
            &out,
            version,
            ParseFeatures::default(),
            FeatureFlags::none(),
        );
        assert!(
            !err,
            "[{label}/{version:?}] emitted output is not valid official LeekScript:\n{out}"
        );
        let got = jit(&rel);
        assert_eq!(
            expected, got,
            "[{label}/{version:?}] result mismatch.\nsource:\n{src}\nemitted:\n{out}"
        );
    }
}

#[test]
fn arithmetic_and_precedence() {
    check("return 1 + 2 * 3 - 4 / 2;");
    check("return (1 + 2) * 3;");
    check("return 2 ** 3 ** 2;");
    check("return (2 ** 3) ** 2;");
    check("return 10 - 3 - 2;");
    check("return 10 - (3 - 2);");
    check("return -(-5);");
    check("return !(1 == 2) && (3 < 4 || 5 > 6);");
    check("return 5 % 3 + 1;");
}

#[test]
fn ternary_and_logical() {
    check("return true ? 1 : 2;");
    check("return 1 < 2 ? (3 > 4 ? 5 : 6) : 7;");
    check("var x = 3; return x > 0 ? x : -x;");
}

#[test]
fn variables_and_assignment() {
    check("var x = 5; x += 3; x *= 2; return x;");
    check("var a = 1; var b = 2; a = b = 10; return a + b;");
    check("var i = 0; i++; ++i; return i;");
}

#[test]
fn control_flow() {
    check("var s = 0; for (var i = 0; i < 5; i++) { s += i; } return s;");
    check("var s = 0; var i = 0; while (i < 4) { s += i; i++; } return s;");
    check("var x = 3; if (x > 5) { return 1; } else if (x > 1) { return 2; } else { return 3; }");
    check("var s = 0; for (var v in [1, 2, 3]) { s += v; } return s;");
}

#[test]
fn collections() {
    check("var a = [1, 2, 3]; return a[1];");
    check("var m = [\"a\": 1, \"b\": 2]; return m[\"b\"];");
    check("var a = [10, 20, 30]; return a[0] + a[2];");
    check("return [:];");
}

#[test]
fn functions_and_lambdas() {
    check("function add(a, b) { return a + b; } return add(3, 4);");
    check("var f = (x) -> x * 2; return f(21);");
    check(
        "function fib(n) { if (n <= 1) { return n; } return fib(n - 1) + fib(n - 2); } return fib(10);",
    );
}

#[test]
fn strings() {
    check("return \"hello\" + \" \" + \"world\";");
    check("var s = \"a\\nb\"; return s;");
}

#[test]
fn enum_desugars_to_class() {
    // Enums require the experimental feature to parse, but the emitted
    // form is a plain class — it must re-lower with features OFF.
    check("enum Color { RED, GREEN, BLUE = 10 } return Color.GREEN + Color.BLUE;");
}

#[test]
fn overloads_are_renamed() {
    check(
        "function f(a) { return a; }\n\
         function f(a, b) { return a + b; }\n\
         return f(5) + f(3, 4);",
    );
}

#[test]
fn classes() {
    check(
        "class Point {\n\
           x y\n\
           constructor(a, b) { this.x = a; this.y = b; }\n\
           sum() { return this.x + this.y; }\n\
         }\n\
         var p = new Point(3, 4);\n\
         return p.sum();",
    );
}

#[test]
fn comments_preserved_in_pretty_dropped_in_compact() {
    let src = "// a leading comment\n\
               function inc(n) {\n\
               \t// inside the body\n\
               \treturn n + 1;\n\
               }\n\
               return inc(41);\n";
    let (hir, err) = lower(src, all_parse_features(), experimental_flags());
    assert!(!err);

    let pretty = emit(&hir, &Options::pretty(Version::V4).with_source_text(src)).source;
    assert!(
        pretty.contains("// a leading comment"),
        "leading comment missing:\n{pretty}"
    );
    assert!(
        pretty.contains("// inside the body"),
        "inner comment missing:\n{pretty}"
    );

    let compact = emit(&hir, &Options::compact(Version::V4)).source;
    assert!(
        !compact.contains("//"),
        "compact kept a comment:\n{compact}"
    );
    // Compact output is a single line of minified source.
    assert!(!compact.contains('\n'), "compact has newlines:\n{compact}");
}

#[test]
fn constant_folding_preserves_result() {
    check("return 2 + 3 * 4;");
    check("var x = true ? 100 : 200; return x;");
    check("function f() { return 1; if (false) { return 2; } } return f();");
}

// ---- globals ----

/// How many top-level `global <name>` declarations the output holds.
fn count_global_decls(out: &str, name: &str) -> usize {
    out.match_indices("global ")
        .filter(|(i, _)| out[i + "global ".len()..].starts_with(name))
        .filter(|(i, _)| {
            let rest = &out[i + "global ".len() + name.len()..];
            // Not a prefix of a longer identifier.
            !rest.starts_with(|c: char| c.is_alphanumeric() || c == '_')
        })
        .count()
}

#[test]
fn file_level_global_emitted_once() {
    // HIR keeps both a `Def::Global` item and the `global g = 3;`
    // statement; emitting both is a redeclaration upstream rejects.
    // The reads stay in main: a bare `g` inside a function body is
    // lowered before `declare_global` runs, so it would not be a
    // `NameRef::Global` and the assertion would test the wrong thing.
    for src in [
        "global g = 3; g = g + 1; return g;",
        "global g; g = 2; return g;",
    ] {
        let (hir, err) = lower(src, all_parse_features(), experimental_flags());
        assert!(!err, "program failed to lower:\n{src}");

        for opts in [Options::pretty(Version::V4), Options::compact(Version::V4)] {
            let out = emit(&hir, &opts).source;
            assert_eq!(
                count_global_decls(&out, "g"),
                1,
                "global declared more than once for `{src}`:\n{out}"
            );
        }
        check(src);
    }
}

// ---- multi-file projects ----

struct Unit {
    path: PathBuf,
    source: SourceId,
    ast: SourceFile,
}

fn parse_unit(path: &str, source: SourceId, text: &str) -> Unit {
    let parsed = parse(text, source, Version::V4);
    assert!(
        parsed
            .diagnostics
            .iter()
            .all(|d| d.severity != Severity::Error),
        "unit {path} failed to parse"
    );
    Unit {
        path: PathBuf::from(path),
        source,
        ast: SourceFile::cast(SyntaxNode::new_root(parsed.green)).expect("parse root"),
    }
}

/// Lower an entry plus one included file into a single `HirFile`, the way
/// the include-aware pipeline does. `resolved` is the include map the
/// resolver would have produced; `None` leaves the units unrelated (the
/// merged-header shape).
fn lower_two_units(entry: &Unit, other: &Unit, resolved: bool) -> HirFile {
    let entry_unit = LowerUnit {
        ast: &entry.ast,
        source: entry.source,
        path: &entry.path,
        version: Version::V4,
    };
    let includes = [LowerUnit {
        ast: &other.ast,
        source: other.source,
        path: &other.path,
        version: Version::V4,
    }];
    let map: BTreeMap<(PathBuf, String), PathBuf> = if resolved {
        let name = other
            .path
            .file_stem()
            .expect("stem")
            .to_string_lossy()
            .into_owned();
        BTreeMap::from([((entry.path.clone(), name), other.path.clone())])
    } else {
        BTreeMap::new()
    };
    let (hir, diags) = lower_files(
        entry_unit,
        &includes,
        resolved.then_some(&map),
        FeatureFlags::none(),
    );
    assert!(
        diags.iter().all(|d| d.severity != Severity::Error),
        "multi-file lowering failed: {diags:?}"
    );
    hir
}

const INCLUDED_SOURCE: SourceId = match SourceId::new(2) {
    Some(s) => s,
    None => unreachable!(),
};

#[test]
fn included_defs_are_emitted() {
    // An included file carries its own `SourceId`; its definitions used to
    // be mistaken for prelude signatures and dropped, leaving calls to
    // functions the output never defines.
    let entry = parse_unit(
        "/main.leek",
        SOURCE,
        "include(\"util\")\nglobal n = helper(2);\nreturn n + TOP;\n",
    );
    let util = parse_unit(
        "/util.leek",
        INCLUDED_SOURCE,
        "function helper(x) { return x * 21; }\nglobal TOP = 5;\n",
    );
    let hir = lower_two_units(&entry, &util, true);

    let out = emit(&hir, &Options::pretty(Version::V4).with_user_source(SOURCE)).source;
    assert!(
        out.contains("function helper("),
        "included function dropped:\n{out}"
    );
    assert_eq!(
        count_global_decls(&out, "TOP"),
        1,
        "included global not declared exactly once:\n{out}"
    );
    assert!(
        !out.contains("include("),
        "include site survived emission:\n{out}"
    );

    // The emitted single file is official LeekScript and runs the same.
    check_hir(&hir, "");
}

#[test]
fn prelude_defs_still_dropped() {
    // Regression guard for the fix above: library headers merged under
    // `leek_prelude::source_id()` are still dropped, unlike includes.
    let entry = parse_unit("/main.leek", SOURCE, "return lib_helper(2);\n");
    let header = parse_unit(
        "/<prelude>",
        leek_prelude::source_id(),
        "function lib_helper(x) { return x * 21; }\n",
    );
    let hir = lower_two_units(&entry, &header, false);

    let dropped = emit(&hir, &Options::pretty(Version::V4).with_user_source(SOURCE)).source;
    assert!(
        !dropped.contains("function lib_helper("),
        "prelude definition leaked into the output:\n{dropped}"
    );
    let kept = emit(
        &hir,
        &Options::pretty(Version::V4)
            .with_user_source(SOURCE)
            .keep_prelude_defs(),
    )
    .source;
    assert!(
        kept.contains("function lib_helper("),
        "keep_prelude_defs() did not keep the definition:\n{kept}"
    );
}

// ---- multi-file projects, through the real include pipeline ----

/// Lower a whole project the way `miku build` does: a real folder, the real
/// [`ResolveIncludes`] step, the real recipe. Nothing about the include
/// graph or the splicer is simulated, so an `include(…)` that survives
/// lowering is visible here.
fn lower_project(entry_path: &str, files: &[(&str, &str)], ff: FeatureFlags) -> Arc<HirFile> {
    let mut folder = MemFolder::new();
    for (path, text) in files {
        folder.insert(*path, *text);
    }
    let entry_text = files
        .iter()
        .find(|(path, _)| *path == entry_path)
        .map(|(_, text)| (*text).to_string())
        .expect("entry exists in the fixture");

    let input = Input {
        source: SOURCE,
        text: entry_text.into(),
        version_byte: 4,
        strict: false,
        flags: ff,
    };
    let resolve = ResolveIncludes::with_counter(
        Arc::new(folder),
        PathBuf::from(entry_path),
        /* start = */ 2,
    );
    let pipeline = pipeline_hir_with_includes(Box::new(resolve), &RecipeParams::permissive())
        .expect("recipe builds");
    let run = pipeline.run(input);
    run.get::<HirArtifact>()
        .expect("HirArtifact present")
        .0
        .clone()
}

#[test]
fn nested_include_is_spliced_not_reemitted() {
    // `include` is a plain statement, so it parses anywhere. The splicer
    // used to walk only the entry's flat top-level list, so a nested site
    // kept its `Stmt::Include` *and* got the included file's definitions
    // emitted next to it — a duplicate definition plus a dependency on a
    // file that was supposed to be inlined.
    let hir = lower_project(
        "/main.leek",
        &[
            (
                "/main.leek",
                "if (true) {\n\tinclude(\"util\");\n}\nreturn f();\n",
            ),
            ("/util.leek", "function f() { return 42; }\n"),
        ],
        FeatureFlags::none(),
    );

    let out = emit(&hir, &Options::pretty(Version::V4)).source;
    assert!(
        !out.contains("include("),
        "nested include site survived emission:\n{out}"
    );
    assert_eq!(
        out.matches("function f(").count(),
        1,
        "included function not emitted exactly once:\n{out}"
    );
    assert_eq!(jit(&hir), "42", "nested include changed the result");
    check_hir(&hir, "");
}

#[test]
fn repeated_global_declared_once() {
    // Two declaration sites share one `DefId`; emitting `global` at both
    // is a redeclaration the official compiler rejects. The round-trip
    // alone cannot see this (our parser accepts it), so assert on the text.
    let src = "global g = 1; global g = 2; return g;";
    let (hir, err) = lower(src, all_parse_features(), experimental_flags());
    assert!(!err, "program failed to lower:\n{src}");

    for opts in [Options::pretty(Version::V4), Options::compact(Version::V4)] {
        let out = emit(&hir, &opts).source;
        assert_eq!(
            count_global_decls(&out, "g"),
            1,
            "global declared more than once:\n{out}"
        );
    }
    check(src);
}

#[test]
fn same_global_in_two_includes() {
    // Both files declare `CFG`; `declare_global` folds them onto one
    // `DefId` but leaves both statements, so the bundle used to carry two
    // `global CFG` declarations.
    let hir = lower_project(
        "/main.leek",
        &[
            (
                "/main.leek",
                "include(\"a\")\ninclude(\"b\")\nreturn CFG;\n",
            ),
            ("/a.leek", "global CFG = 1;\n"),
            ("/b.leek", "global CFG = 2;\n"),
        ],
        FeatureFlags::none(),
    );

    for opts in [Options::pretty(Version::V4), Options::compact(Version::V4)] {
        let out = emit(&hir, &opts).source;
        assert_eq!(
            count_global_decls(&out, "CFG"),
            1,
            "global declared more than once across includes:\n{out}"
        );
    }
    check_hir(&hir, "");
}

// ---- constructs the emitter supports ----

#[test]
fn switch_arms() {
    check(
        "var x = 2; var r = 0; switch (x) { case 1: r = 10; break; case 2: r = 20; break; default: r = 99; } return r;",
    );
    // Fall-through: no `break` after `case 1`.
    check(
        "var x = 1; var r = 0; switch (x) { case 1: r = r + 1; case 2: r = r + 10; break; default: r = 100; } return r;",
    );
    check("var x = 7; var r = 0; switch (x) { case 1: r = 1; break; default: r = -1; } return r;");
    // A switch arm is a statement *list*, so a multi-declarator `var`
    // contributes one declaration apiece. Arms used to be lowered one
    // statement at a time, which kept only the first declarator.
    check("var x = 1; switch (x) { case 1: var a = 1, b = 2; return a + b; } return 0;");
}

#[test]
fn foreach_key_value() {
    check("var t = 0; for (var k : var v in [1, 2, 3]) { t = t + k * v; } return t;");
    check("var s = \"\"; for (var k : var v in [\"a\": 1, \"b\": 2]) { s = s + k + v; } return s;");
}

#[test]
fn foreach_by_ref() {
    check("var a = [1, 2, 3]; for (var @v in a) { v = v * 2; } return a;");
}

#[test]
fn sets_and_intervals() {
    check("return {1, 2, 3};");
    check("return {1, 2..5};");
    check("var t = 0; for (var x in [1..5]) { t = t + x; } return t;");
    check("var t = 0; for (var x in ]1..5[) { t = t + x; } return t;");
    check("var t = 0; for (var x in [1..10:2]) { t = t + x; } return t;");
}

#[test]
fn casts() {
    check("return \"12\" as integer;");
    check("return 3 as real;");
    check("return 12 as string;");
    check("return 1 as boolean;");
}

#[test]
fn by_ref_params() {
    check("function bump(@x) { x = x + 1; } var n = 1; bump(n); return n;");
    check(
        "function swap(@a, @b) { var t = a; a = b; b = t; } var p = 1; var q = 2; swap(p, q); return p * 10 + q;",
    );
}

#[test]
fn object_literals() {
    check("var o = {a: 1, b: 2}; return o.a + o.b;");
    check("var o = {n: [1, 2]}; return o.n[1];");
}

#[test]
fn do_while_break_continue() {
    check(
        "var i = 0; var t = 0; do { i = i + 1; if (i == 2) { continue; } t = t + i; } while (i < 4); return t;",
    );
    check("var t = 0; for (var i = 0; i < 10; i++) { if (i > 3) { break; } t = t + i; } return t;");
    check(
        "var t = 0; var i = 0; while (true) { i++; if (i % 2 == 0) { continue; } if (i > 7) { break; } t = t + i; } return t;",
    );
}

#[test]
fn class_members_and_inheritance() {
    // Class members take modifiers but no `var` / `function` keyword.
    check(
        "class Shape {\n\
         \tprivate static made = 0\n\
         \tprotected name = \"shape\"\n\
         \tconstructor() { Shape.made = Shape.made + 1; }\n\
         \tpublic static count() { return Shape.made; }\n\
         \tprivate tag() { return this.name; }\n\
         \tpublic label() { return this.tag(); }\n\
         }\n\
         class Square extends Shape {\n\
         \tconstructor() { this.name = \"square\"; }\n\
         }\n\
         var s = new Square();\n\
         return s.label() + Shape.count();\n",
    );
    check("class Const { final static N = 7 } return Const.N;");
}

#[test]
fn versions_round_trip() {
    // `Options::version` steers the string delimiter (#395), the type
    // annotations (#154) and comment recovery. These programs use the forms
    // whose *meaning* is version-dependent — `^`/`^=`, string escapes,
    // integer division, non-finite reals — so the day one of them changes in
    // the lowerer without the emitter learning about it, this fails instead
    // of silently changing the generated program.
    const PROGRAMS: &[&str] = &[
        "return 1 + 2 * 3;",
        "var x = 6; x ^= 3; return x;",
        "return 6 ^ 3;",
        "var s = \"a\\\"b\"; return s;",
        "return 7 / 2;",
        "return 1 < 2 == true;",
        "var a = [1, 2, 3]; var t = 0; for (var v in a) { t = t + v; } return t;",
        "function f(n) { return n * 2; } return f(21);",
        // `∞` used to be emitted as `(1.0 / 0.0)`, which at v1 is `null`.
        "return \u{221e};",
        "var x = -\u{221e}; return x;",
    ];
    for version in [Version::V1, Version::V2, Version::V3, Version::V4] {
        for src in PROGRAMS {
            check_at(src, version);
        }
    }
}

// ---- semantics the round-trip used to drop (#154) ----

#[test]
fn declared_types_survive_the_round_trip() {
    // A declared scalar type is a coercion upstream, not documentation:
    // `real r = 5` stores `5.0`. Erasing it to `var r = 5` changed the
    // program's *result*, which is what made this a bug rather than a
    // cosmetic loss.
    check("real r = 5; return r;");
    check("integer n = 7; n /= 2; return n;");
    check("real? r = 5; return r;");
    check("function f(real x) { return x; } return f(5);");
    check("function g(integer a, real b) { return a + b; } return g(1, 2);");
    check("global real G = 5; return G;");
    check("class C { real v = 5 } return (new C()).v;");
    check("Array<real> a = [1, 2]; return a[0];");
    check("Map<string, real> m = [\"k\" : 1]; return m[\"k\"];");
}

#[test]
fn declared_types_below_v4_are_dropped_loudly() {
    // The annotation is official v4 syntax; at v1-v3 it is not emitted (the
    // official servers of those versions predate it), so the coercion is
    // genuinely lost — and has to say so rather than vanish.
    for version in [Version::V1, Version::V2, Version::V3] {
        let (hir, err) = lower_at(
            "real r = 5; return r;",
            version,
            all_parse_features(),
            experimental_flags(),
        );
        assert!(!err, "program failed to lower at {version:?}");
        let out = emit(&hir, &Options::pretty(version));
        assert!(
            !out.source.contains("real"),
            "[{version:?}] annotation emitted below v4:\n{}",
            out.source
        );
        assert!(
            out.diagnostics
                .iter()
                .any(|d| d.code.id() == "E0620" && d.severity == Severity::Warning),
            "[{version:?}] the dropped type was not reported"
        );
    }
}

#[test]
fn a_type_with_no_official_spelling_is_dropped_loudly() {
    // `Array[integer, boolean]` is the experimental tuple shape: there is no
    // non-experimental syntax for it, so the declaration goes out untyped.
    // The point of the change is that it goes out *with a complaint*.
    let (hir, err) = lower(
        "Array[integer, boolean] t = [1, true]; return t[0];",
        all_parse_features(),
        experimental_flags(),
    );
    assert!(!err, "program failed to lower");
    let out = emit(&hir, &Options::pretty(Version::V4));
    assert!(
        out.diagnostics
            .iter()
            .any(|d| d.code.id() == "E0620" && d.severity == Severity::Warning),
        "an unrepresentable declared type was dropped silently:\n{}",
        out.source
    );
    check_hir(&hir, "");
}

#[test]
fn an_ordinary_program_reports_nothing() {
    // The no-false-positive guard: a program with nothing to lose must not
    // pick up a warning, or every build starts printing noise.
    let (hir, err) = lower(
        "real r = 5; function f(integer n) -> integer { return n * 2; } return f(3) + r;",
        all_parse_features(),
        experimental_flags(),
    );
    assert!(!err, "program failed to lower");
    let out = emit(&hir, &Options::pretty(Version::V4));
    assert!(
        out.diagnostics.is_empty(),
        "unexpected diagnostics: {:?}\n{}",
        out.diagnostics,
        out.source
    );
}

#[test]
fn non_finite_reals_are_emitted_as_names_not_arithmetic() {
    // `(1.0 / 0.0)` and `(0.0 / 0.0)` are not `∞` and `NaN` at v1 — division
    // by zero is `null` there — so the emitted program said something else
    // entirely. `∞` is official syntax and lowers straight back to
    // `Literal::Real(INFINITY)`.
    //
    // Asserted on the emitted text rather than through `check_at`, because
    // `jit` runs every program at the native backend's default version: the
    // round-trip harness lowers at `version` but does not *execute* at it, so
    // it cannot see a v1-only divergence (a gap worth closing separately).
    for version in [Version::V1, Version::V2, Version::V3, Version::V4] {
        for (src, want) in [
            ("return \u{221e};", "\u{221e}"),
            ("return -\u{221e};", "-\u{221e}"),
        ] {
            let (hir, err) = lower_at(src, version, all_parse_features(), experimental_flags());
            assert!(!err, "program failed to lower at {version:?}: {src}");
            let out = emit(&hir, &Options::pretty(version)).source;
            assert!(
                out.contains(want),
                "[{version:?}] {src} emitted as `{out}`, not `{want}`"
            );
            assert!(
                !out.contains("0.0"),
                "[{version:?}] {src} still emits division: `{out}`"
            );
        }
    }
}

#[test]
fn an_empty_set_stays_a_set() {
    // `{}` re-parses as an empty *object*, so the emitted program used to
    // hold a different value than the one it came from.
    check("return <>;");
    check("var s = <>; return s;");
    check("var s = <>; s.push(1); return s;");
}

// ---- more multi-file shapes ----

#[test]
fn included_file_top_level_statements_run_in_order() {
    // The included file's main block evaluates at the include site.
    let hir = lower_project(
        "/main.leek",
        &[
            (
                "/main.leek",
                "include(\"util\")\nLOG = LOG + \"e\";\nreturn LOG;\n",
            ),
            ("/util.leek", "global LOG = \"\";\nLOG = LOG + \"u\";\n"),
        ],
        FeatureFlags::none(),
    );
    assert_eq!(jit(&hir), "\"ue\"", "include site ran out of order");
    let out = emit(&hir, &Options::pretty(Version::V4)).source;
    assert_eq!(count_global_decls(&out, "LOG"), 1, "{out}");
    check_hir(&hir, "");
}

#[test]
fn diamond_include_emits_each_def_once() {
    // `a` and `b` both include `common`; a logical merge evaluates it once.
    let hir = lower_project(
        "/main.leek",
        &[
            (
                "/main.leek",
                "include(\"a\")\ninclude(\"b\")\nreturn shared() + TOTAL;\n",
            ),
            ("/a.leek", "include(\"common\")\nTOTAL = TOTAL + 1;\n"),
            ("/b.leek", "include(\"common\")\nTOTAL = TOTAL + 10;\n"),
            (
                "/common.leek",
                "function shared() { return 100; }\nglobal TOTAL = 0;\n",
            ),
        ],
        FeatureFlags::none(),
    );

    let out = emit(&hir, &Options::pretty(Version::V4)).source;
    assert_eq!(
        out.matches("function shared(").count(),
        1,
        "diamond duplicated a definition:\n{out}"
    );
    assert_eq!(count_global_decls(&out, "TOTAL"), 1, "{out}");
    assert!(!out.contains("include("), "{out}");
    check_hir(&hir, "");
}

#[test]
fn include_chain_with_classes() {
    // Included classes reach the emitter too, and their methods and
    // constructors keep working across the file boundary.
    let hir = lower_project(
        "/main.leek",
        &[
            (
                "/main.leek",
                "include(\"util\")\n\
                 var s = new Square(3);\n\
                 return s.area() + area(4);\n",
            ),
            (
                "/util.leek",
                "class Square {\n\
                 \tside\n\
                 \tconstructor(n) { this.side = n; }\n\
                 \tarea() { return this.side * this.side; }\n\
                 }\n\
                 function area(w) { return w * w; }\n",
            ),
        ],
        FeatureFlags::none(),
    );

    let out = emit(&hir, &Options::pretty(Version::V4)).source;
    assert!(
        out.contains("class Square"),
        "included class dropped:\n{out}"
    );
    assert!(
        out.contains("function area("),
        "included function dropped:\n{out}"
    );
    assert!(!out.contains("include("), "{out}");
    assert_eq!(jit(&hir), "25");
    check_hir(&hir, "");
}

#[test]
fn include_in_unbraced_branch_becomes_a_block() {
    // A bare `if (c) include("x");` has no statement list to splice into.
    // The include expands to a block: its main body is usually more than
    // one statement, and leaving it unbraced would put only the first
    // under the branch.
    let hir = lower_project(
        "/main.leek",
        &[
            (
                "/main.leek",
                "global n = 0;\nif (true) include(\"util\");\nreturn n;\n",
            ),
            ("/util.leek", "n = n + 1;\nn = n + 10;\n"),
        ],
        FeatureFlags::none(),
    );

    let out = emit(&hir, &Options::pretty(Version::V4)).source;
    assert!(!out.contains("include("), "{out}");
    assert_eq!(jit(&hir), "11", "only part of the include body ran:\n{out}");
    check_hir(&hir, "");
}

#[test]
fn include_inside_a_function_body_is_spliced() {
    // Definition bodies are lowered before the per-file main blocks exist,
    // so their include sites are spliced in a later pass. The file already
    // ran at top level here, so the nested site merges to nothing.
    let hir = lower_project(
        "/main.leek",
        &[
            (
                "/main.leek",
                "function f() { include(\"util\"); return helper(); }\nreturn f();\n",
            ),
            ("/util.leek", "function helper() { return 42; }\n"),
        ],
        FeatureFlags::none(),
    );

    let out = emit(&hir, &Options::pretty(Version::V4)).source;
    assert!(
        !out.contains("include("),
        "include inside a function body survived emission:\n{out}"
    );
    assert_eq!(out.matches("function helper(").count(), 1, "{out}");
    assert_eq!(jit(&hir), "42");
    check_hir(&hir, "");
}

#[test]
fn included_file_reads_includer_local() {
    // `include` is textual splicing, so this is `var cfg = 3; var got =
    // cfg; return got;`. Every included main block used to be lowered
    // before the entry's, so `cfg` wasn't in scope and `got`'s read became
    // a by-name global lookup that never finds the entry's main-block
    // local — the program returned null (#118).
    let hir = lower_project(
        "/main.leek",
        &[
            ("/main.leek", "var cfg = 3;\ninclude(\"a\")\nreturn got;\n"),
            ("/a.leek", "var got = cfg;\n"),
        ],
        FeatureFlags::none(),
    );
    assert_eq!(
        jit(&hir),
        "3",
        "included file lost sight of the includer's local"
    );
    check_hir(&hir, "");
}

#[test]
fn nested_include_reads_sibling_local() {
    // Declaration order used to come from the include graph's topological
    // order (a, then b) while run-time order came from the include sites
    // (b's `var x`, then a's body). Textual splicing gives `var x = 1; var
    // y = x; return y;` (#339).
    let hir = lower_project(
        "/main.leek",
        &[
            ("/main.leek", "include(\"b\")\nreturn y;\n"),
            ("/b.leek", "var x = 1;\ninclude(\"a\")\n"),
            ("/a.leek", "var y = x;\n"),
        ],
        FeatureFlags::none(),
    );
    assert_eq!(
        jit(&hir),
        "1",
        "sibling include lost sight of the local above its site"
    );
    check_hir(&hir, "");
}
