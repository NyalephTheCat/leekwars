//! Direct tests for [`leek_types::check_collecting_files`], the
//! multi-file type-check entry point `leek_types::pipeline::TypeCheck`
//! takes whenever an include graph is present.
//!
//! Like its resolver twin, nothing exercised this function before
//! (#194) — the crate had no `tests/` directory at all — which is why
//! #118's ordering divergence could sit here unnoticed.

use std::path::{Path, PathBuf};

use leek_diagnostics::{Code, Diagnostic};
use leek_parser::ast::{AstNode, SourceFile};
use leek_parser::{ParseFeatures, parse_with_features};
use leek_resolver::FileUnit;
use leek_resolver::folder::MemFolder;
use leek_resolver::include_graph::{ResolvedFile, build_include_graph};
use leek_span::SourceId;
use leek_syntax::{SyntaxNode, Version};
use leek_types::Type;
use leek_types::{Options, TypeCheckResult, check_collecting_files};

struct Parsed {
    path: PathBuf,
    source: SourceId,
    ast: SourceFile,
    version: Version,
}

struct Checked {
    result: TypeCheckResult,
    sources: Vec<(PathBuf, SourceId)>,
}

impl Checked {
    fn source_of(&self, path: &str) -> SourceId {
        match self.sources.iter().find(|(p, _)| p == Path::new(path)) {
            Some((_, source)) => *source,
            None => panic!("{path} is part of the closure; have {:?}", self.sources),
        }
    }

    fn diags(&self, code: Code) -> Vec<&Diagnostic> {
        self.result
            .diagnostics
            .iter()
            .filter(|d| d.code.name() == code.name())
            .collect()
    }

    /// The type recorded for the expression spanning `text` in `path`.
    fn ty_of(&self, path: &str, file_text: &str, expr: &str) -> Option<Type> {
        let source = self.source_of(path);
        let start = u32::try_from(
            file_text
                .find(expr)
                .expect("fixture contains the expression"),
        )
        .expect("fixture offsets fit in u32");
        let end = start + u32::try_from(expr.len()).expect("fixture offsets fit in u32");
        self.result
            .table
            .exprs
            .iter()
            .find(|t| t.span.source == source && t.span.start == start && t.span.end == end)
            .map(|t| t.ty.clone())
    }
}

fn check(entry: &str, files: &[(&str, &str)]) -> Checked {
    check_with(entry, files, Options::default())
}

fn check_with(entry: &str, files: &[(&str, &str)], opts: Options) -> Checked {
    let mut folder = MemFolder::new();
    for (p, t) in files {
        folder.insert(*p, *t);
    }
    let entry_text = files
        .iter()
        .find(|(p, _)| *p == entry)
        .map(|(_, t)| (*t).to_string())
        .expect("entry exists in fixture");

    let mut next: u32 = 1;
    let graph = build_include_graph(Path::new(entry), &entry_text, Version::V4, &folder, |_| {
        let id = SourceId::new(next).unwrap();
        next += 1;
        id
    });

    let parsed: Vec<Parsed> = graph
        .files
        .iter()
        .map(|f: &ResolvedFile| Parsed {
            path: f.path.clone(),
            source: f.source,
            ast: SourceFile::cast(SyntaxNode::new_root(
                parse_with_features(&f.text, f.source, f.version, ParseFeatures::default()).green,
            ))
            .expect("source file parses"),
            version: f.version,
        })
        .collect();

    let units: Vec<FileUnit<'_>> = parsed
        .iter()
        .map(|p| FileUnit {
            ast: &p.ast,
            source: p.source,
            version: p.version,
            path: &p.path,
        })
        .collect();

    Checked {
        result: check_collecting_files(&units, Some(&graph.resolved), opts),
        sources: parsed.iter().map(|p| (p.path.clone(), p.source)).collect(),
    }
}

#[test]
fn empty_slice_checks_to_nothing() {
    let result = check_collecting_files(&[], None, Options::default());
    assert!(result.diagnostics.is_empty());
    assert!(result.table.exprs.is_empty());
}

// ---- Cross-file signatures ----

#[test]
fn a_call_to_an_included_function_infers_its_declared_return() {
    // Signatures are collected from every file before any body is
    // checked, so the direction of the include doesn't matter here.
    let r = check(
        "/main.leek",
        &[
            ("/main.leek", "include(\"a\")\nvar v = two()\n"),
            ("/a.leek", "function two() -> integer { return 2 }\n"),
        ],
    );
    assert_eq!(
        r.ty_of("/main.leek", "include(\"a\")\nvar v = two()\n", "two()"),
        Some(Type::Integer),
        "{:?}",
        r.result.table.exprs
    );
}

#[test]
fn an_included_file_sees_a_class_declared_in_the_entry() {
    let entry = "include(\"a\")\nclass Box { integer n }\n";
    let a = "Box b = new Box()\nvar got = b.n\n";
    let r = check("/main.leek", &[("/main.leek", entry), ("/a.leek", a)]);
    assert_eq!(
        r.ty_of("/a.leek", a, "b.n"),
        Some(Type::Integer),
        "{:?}",
        r.result.diagnostics
    );
}

// ---- Main statements are checked in include-site order (#118) ----

#[test]
fn an_included_file_types_an_includer_var_declared_above_the_include_site() {
    // Checking the closure in include-graph order recorded /a.leek's
    // `n` before the entry's `integer n = 3` was ever seen, so it typed as
    // `Any`. Under splicing the type is known at the site.
    let a = "var m = n\n";
    let r = check(
        "/main.leek",
        &[
            ("/main.leek", "integer n = 3\ninclude(\"a\")\n"),
            ("/a.leek", a),
        ],
    );
    assert_eq!(
        r.ty_of("/a.leek", a, "n"),
        Some(Type::Integer),
        "{:?}",
        r.result.table.exprs
    );
}

#[test]
fn the_entry_does_not_type_an_included_var_before_the_include_site() {
    // The mirror image: an included file's top-level `var` must not
    // give the entry's earlier statements a type.
    let entry = "var e = n\ninclude(\"a\")\n";
    let r = check(
        "/main.leek",
        &[("/main.leek", entry), ("/a.leek", "integer n = 3\n")],
    );
    assert_eq!(
        r.ty_of("/main.leek", entry, "n"),
        Some(Type::Any),
        "{:?}",
        r.result.table.exprs
    );
}

#[test]
fn a_boxed_include_checks_the_included_statements() {
    // `if (c) include("a");` — a top-level-statements-only walk would
    // never reach this site.
    let a = "var m = n\n";
    let r = check(
        "/main.leek",
        &[
            (
                "/main.leek",
                "integer n = 3\nvar c = 1\nif (c) include(\"a\");\n",
            ),
            ("/a.leek", a),
        ],
    );
    assert_eq!(
        r.ty_of("/a.leek", a, "n"),
        Some(Type::Integer),
        "{:?}",
        r.result.table.exprs
    );
}

#[test]
fn a_file_included_from_two_sites_is_checked_once() {
    // The diamond rule: /shared.leek's statements must not be typed
    // twice, or its expressions appear twice in the table.
    let shared = "var s = 1\n";
    let r = check(
        "/main.leek",
        &[
            ("/main.leek", "include(\"a\")\ninclude(\"b\")\n"),
            ("/a.leek", "include(\"shared\")\n"),
            ("/b.leek", "include(\"shared\")\n"),
            ("/shared.leek", shared),
        ],
    );
    let shared_source = r.source_of("/shared.leek");
    let entries = r
        .result
        .table
        .exprs
        .iter()
        .filter(|t| t.span.source == shared_source)
        .count();
    assert_eq!(entries, 1, "{:?}", r.result.table.exprs);
}

#[test]
fn a_cycle_back_to_the_entry_does_not_re_enter_the_entrys_main_block() {
    // The include graph records the back edge before it detects the
    // cycle, so an expander that did not seed itself with the entry
    // would recurse forever here.
    let entry = "var e = 1\ninclude(\"a\")\n";
    let r = check(
        "/main.leek",
        &[
            ("/main.leek", entry),
            ("/a.leek", "include(\"main\")\nvar a = 1\n"),
        ],
    );
    let entry_source = r.source_of("/main.leek");
    let ones = r
        .result
        .table
        .exprs
        .iter()
        .filter(|t| t.span.source == entry_source && t.span.start == 8)
        .count();
    assert_eq!(ones, 1, "the entry's main block is typed once");
}

// ---- Per-file source ids, versions and options ----

#[test]
fn a_diagnostic_from_an_included_file_carries_that_files_source_id() {
    let r = check_with(
        "/main.leek",
        &[
            ("/main.leek", "include(\"a\")\n"),
            ("/a.leek", "function f() -> integer { return \"s\" }\n"),
        ],
        Options {
            strict: true,
            ..Options::default()
        },
    );
    let diags = r.diags(leek_diagnostics::codes::INCOMPATIBLE_TYPE);
    assert_eq!(diags.len(), 1, "{:?}", r.result.diagnostics);
    assert_eq!(diags[0].span.source, r.source_of("/a.leek"));
}

#[test]
fn the_strict_flag_reaches_every_file_of_the_closure() {
    // The declared-return check is strict-only, and the offending
    // function lives in an included file: if `Options` were applied
    // only to the entry, this would be silent.
    const FILES: &[(&str, &str)] = &[
        ("/main.leek", "include(\"a\")\n"),
        ("/a.leek", "function f() -> integer { return \"s\" }\n"),
    ];
    let lenient = check("/main.leek", FILES);
    assert!(
        lenient
            .diags(leek_diagnostics::codes::INCOMPATIBLE_TYPE)
            .is_empty(),
        "{:?}",
        lenient.result.diagnostics
    );

    let strict = check_with(
        "/main.leek",
        FILES,
        Options {
            strict: true,
            ..Options::default()
        },
    );
    assert_eq!(
        strict
            .diags(leek_diagnostics::codes::INCOMPATIBLE_TYPE)
            .len(),
        1,
        "{:?}",
        strict.result.diagnostics
    );
}

#[test]
fn an_included_file_is_checked_at_its_own_version() {
    // v2 lexes `and` as a keyword. Checking the included file at the
    // entry's v4 would make `1 and 0` a pair of unknown names.
    let a = "// @version:2\nvar t = 1 and 0\n";
    let r = check(
        "/main.leek",
        &[("/main.leek", "include(\"old\")\n"), ("/old.leek", a)],
    );
    assert_eq!(
        r.ty_of("/old.leek", a, "1 and 0"),
        Some(Type::Boolean),
        "{:?}",
        r.result.table.exprs
    );
}

#[test]
fn without_an_include_graph_every_files_statements_are_still_checked() {
    let a_text = "var a = 1\n";
    let entry_text = "var e = 2\n";
    let a = SourceFile::cast(SyntaxNode::new_root(
        parse_with_features(
            a_text,
            SourceId::new(1).unwrap(),
            Version::V4,
            ParseFeatures::default(),
        )
        .green,
    ))
    .unwrap();
    let entry = SourceFile::cast(SyntaxNode::new_root(
        parse_with_features(
            entry_text,
            SourceId::new(2).unwrap(),
            Version::V4,
            ParseFeatures::default(),
        )
        .green,
    ))
    .unwrap();
    let units = [
        FileUnit {
            ast: &a,
            source: SourceId::new(1).unwrap(),
            version: Version::V4,
            path: Path::new("/a.leek"),
        },
        FileUnit {
            ast: &entry,
            source: SourceId::new(2).unwrap(),
            version: Version::V4,
            path: Path::new("/main.leek"),
        },
    ];
    let result = check_collecting_files(&units, None, Options::default());
    assert!(
        result
            .table
            .exprs
            .iter()
            .any(|t| t.span.source == SourceId::new(1).unwrap()),
        "the non-entry file contributes typed expressions"
    );
}
