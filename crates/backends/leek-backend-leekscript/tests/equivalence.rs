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

use leek_backend_leekscript::{Options, emit};
use leek_backend_native::{NativeOptions, run};
use leek_diagnostics::Severity;
use leek_hir::HirFile;
use leek_hir::lower::lower_file_versioned_with_flags;
use leek_hir::{LowerUnit, lower_files};
use leek_parser::{ParseFeatures, ast::AstNode, ast::SourceFile, parse, parse_with_features};
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

/// Lower `src` to HIR, returning the HIR and whether any error diagnostic
/// was produced (parse + lower).
fn lower(src: &str, pf: ParseFeatures, ff: FeatureFlags) -> (HirFile, bool) {
    let parsed = parse_with_features(src, SOURCE, Version::V4, pf);
    let file = SourceFile::cast(SyntaxNode::new_root(parsed.green.clone())).expect("parse root");
    let (hir, diags) = lower_file_versioned_with_flags(&file, SOURCE, 4, ff);
    let had_error = parsed
        .diagnostics
        .iter()
        .chain(diags.iter())
        .any(|d| d.severity == Severity::Error);
    (hir, had_error)
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
    let (orig, orig_err) = lower(src, all_parse_features(), experimental_flags());
    assert!(!orig_err, "original program failed to lower:\n{src}");
    check_hir(&orig, src);
}

/// The body of [`check`] on an already-lowered program, so HIR built by
/// hand (multi-file projects) goes through the same round-trip.
/// `src` is only used for comment recovery and failure messages.
fn check_hir(orig: &HirFile, src: &str) {
    let expected = jit(orig);

    let configs = [
        ("pretty", Options::pretty(Version::V4).with_source_text(src)),
        ("compact", Options::compact(Version::V4)),
        (
            "optimized",
            Options::pretty(Version::V4).with_optimize(true),
        ),
    ];

    for (label, opts) in configs {
        let out = emit(orig, &opts).source;
        let (rel, err) = lower(&out, ParseFeatures::default(), FeatureFlags::none());
        assert!(
            !err,
            "[{label}] emitted output is not valid official LeekScript:\n{out}"
        );
        let got = jit(&rel);
        assert_eq!(
            expected, got,
            "[{label}] result mismatch.\nsource:\n{src}\nemitted:\n{out}"
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
