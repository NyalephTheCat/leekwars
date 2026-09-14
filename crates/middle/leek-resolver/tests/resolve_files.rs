//! Direct tests for [`leek_resolver::resolve_collecting_files`], the
//! multi-file entry point the LSP, `miku` and `leekc` reach through
//! `leek_resolver::pipeline::Resolve`.
//!
//! Nothing exercised this function before (#194): the only multi-file
//! suites drove either `leek_hir::lower_files` or a HIR-only recipe that
//! never plans the resolve step. That is also where #118's remaining
//! divergence lived, so the include-order cases below are the ones that
//! would have caught it.

use std::path::{Path, PathBuf};

use leek_diagnostics::{Code, Diagnostic, codes};
use leek_parser::ast::{AstNode, SourceFile};
use leek_parser::{ParseFeatures, parse_with_features};
use leek_resolver::folder::MemFolder;
use leek_resolver::include_graph::{ResolvedFile, build_include_graph};
use leek_resolver::{FileUnit, Options, ResolveResult, resolve_collecting_files};
use leek_span::SourceId;
use leek_syntax::{SyntaxNode, Version};

/// One parsed file of the closure, owned so the `FileUnit` slice can
/// borrow from it.
struct Parsed {
    path: PathBuf,
    source: SourceId,
    ast: SourceFile,
    version: Version,
}

struct Resolved {
    result: ResolveResult,
    /// Canonical path → source id, so a test can name the file a
    /// diagnostic should have landed in.
    sources: Vec<(PathBuf, SourceId)>,
}

impl Resolved {
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
            .filter(|d| d.code == code)
            .collect()
    }

    /// Every reference that resolved to a symbol named `name`.
    fn refs_to(&self, name: &str) -> Vec<&leek_resolver::ResolvedRef> {
        self.result
            .table
            .references
            .iter()
            .filter(|r| {
                self.result
                    .table
                    .symbol(r.target)
                    .is_some_and(|s| s.name == name)
            })
            .collect()
    }
}

/// Walk the include graph for `entry`, parse every file, and resolve the
/// closure the way the pipeline does — entry last, graph attached.
fn resolve(entry: &str, files: &[(&str, &str)]) -> Resolved {
    resolve_with(entry, files, Options::default())
}

fn resolve_with(entry: &str, files: &[(&str, &str)], opts: Options) -> Resolved {
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

    let mut result = resolve_collecting_files(&units, Some(&graph.resolved), opts);
    result.diagnostics.splice(0..0, graph.diagnostics);
    Resolved {
        result,
        sources: parsed.iter().map(|p| (p.path.clone(), p.source)).collect(),
    }
}

#[test]
fn empty_slice_resolves_to_nothing() {
    let result = resolve_collecting_files(&[], None, Options::default());
    assert!(result.diagnostics.is_empty());
    assert!(result.table.symbols.is_empty());
    assert!(result.table.references.is_empty());
}

// ---- Cross-file visibility (hoisted declarations) ----

#[test]
fn entry_resolves_a_function_declared_in_an_included_file() {
    let r = resolve(
        "/main.leek",
        &[
            ("/main.leek", "include(\"util\")\nhelper()\n"),
            ("/util.leek", "function helper() { return 1; }\n"),
        ],
    );
    assert_eq!(r.refs_to("helper").len(), 1, "{:?}", r.result.diagnostics);
    assert!(
        r.result.diagnostics.is_empty(),
        "{:?}",
        r.result.diagnostics
    );
}

#[test]
fn an_included_file_resolves_a_function_declared_in_the_entry() {
    // Functions and classes are hoisted program-wide, so the direction
    // of the include does not matter for them — only main statements
    // are ordered.
    let r = resolve(
        "/main.leek",
        &[
            (
                "/main.leek",
                "include(\"util\")\nfunction fromEntry() { return 1; }\n",
            ),
            ("/util.leek", "var v = fromEntry()\n"),
        ],
    );
    assert_eq!(
        r.refs_to("fromEntry").len(),
        1,
        "{:?}",
        r.result.diagnostics
    );
}

#[test]
fn entry_resolves_a_class_declared_in_an_included_file() {
    let r = resolve(
        "/main.leek",
        &[
            ("/main.leek", "include(\"shapes\")\nvar c = new Circle()\n"),
            ("/shapes.leek", "class Circle { }\n"),
        ],
    );
    assert_eq!(r.refs_to("Circle").len(), 1, "{:?}", r.result.diagnostics);
}

// ---- Main statements run in include-site order (#118) ----

#[test]
fn an_included_file_sees_an_includer_var_declared_above_the_include_site() {
    // The repro from #118: under textual splicing `cfg` is in scope by
    // the time /a.leek's statements run. Resolving the closure in
    // include-graph order walked /a.leek first, so the reference
    // recorded nothing and LSP navigation was wrong for that name.
    let r = resolve(
        "/main.leek",
        &[
            ("/main.leek", "var cfg = 3\ninclude(\"a\")\n"),
            ("/a.leek", "var got = cfg\n"),
        ],
    );
    let refs = r.refs_to("cfg");
    assert_eq!(refs.len(), 1, "{:?}", r.result.table.references);
    // /a.leek's `cfg` read, not some reference inside the entry.
    assert_eq!(refs[0].name_offset, 10);
}

#[test]
fn the_entry_does_not_see_an_included_var_before_the_include_site() {
    // The mirror image: an included file's top-level `var` must not be
    // visible to entry statements written above the include site.
    let r = resolve(
        "/main.leek",
        &[
            ("/main.leek", "var x = got\ninclude(\"a\")\n"),
            ("/a.leek", "var got = 1\n"),
        ],
    );
    assert!(
        r.refs_to("got").is_empty(),
        "`got` is declared below the include site: {:?}",
        r.result.table.references
    );
}

#[test]
fn redeclaring_an_entry_var_in_an_included_file_is_reported_in_the_included_file() {
    // REDECLARED_SYMBOL anchors at the *second* declaration, and
    // include-site order decides which one that is. Splicing puts
    // /a.leek's `x` second, so the error belongs to /a.leek.
    let r = resolve(
        "/main.leek",
        &[
            ("/main.leek", "var x = 1\ninclude(\"a\")\n"),
            ("/a.leek", "var x = 2\n"),
        ],
    );
    let diags = r.diags(codes::REDECLARED_SYMBOL);
    assert_eq!(diags.len(), 1, "{:?}", r.result.diagnostics);
    assert_eq!(diags[0].span.source, r.source_of("/a.leek"));
}

#[test]
fn a_terminator_in_an_included_file_kills_the_statements_after_the_include_site() {
    // Termination tracking crosses the include boundary, because under
    // splicing the `return` and the trailing statement share one list.
    let r = resolve(
        "/main.leek",
        &[
            ("/main.leek", "include(\"a\")\nvar after = 1\n"),
            ("/a.leek", "return 1\n"),
        ],
    );
    let diags = r.diags(codes::CANT_ADD_INSTRUCTION_AFTER_BREAK);
    assert_eq!(diags.len(), 1, "{:?}", r.result.diagnostics);
    assert_eq!(
        diags[0].span.source,
        r.source_of("/main.leek"),
        "the dead statement is the entry's"
    );
}

#[test]
fn a_terminator_inside_a_boxed_include_does_not_kill_the_following_statements() {
    // `if (c) include("a");` expands into a block, so the `return` ends
    // that block rather than the main statement list.
    let r = resolve(
        "/main.leek",
        &[
            (
                "/main.leek",
                "var c = 1\nif (c) include(\"a\");\nvar after = 1\n",
            ),
            ("/a.leek", "return 1\n"),
        ],
    );
    assert!(
        r.diags(codes::CANT_ADD_INSTRUCTION_AFTER_BREAK).is_empty(),
        "{:?}",
        r.result.diagnostics
    );
}

#[test]
fn a_boxed_include_resolves_the_included_statements() {
    // A flat "top-level statements only" iterator would never reach
    // this site; the expander is consulted at the `Stmt::Include` arm
    // instead, wherever that arm is reached from.
    let r = resolve(
        "/main.leek",
        &[
            (
                "/main.leek",
                "var cfg = 3\nvar c = 1\nif (c) include(\"a\");\n",
            ),
            ("/a.leek", "var got = cfg\n"),
        ],
    );
    assert_eq!(r.refs_to("cfg").len(), 1, "{:?}", r.result.table.references);
}

#[test]
fn a_file_included_from_two_sites_runs_its_main_block_once() {
    // The diamond rule: /shared.leek's `var s` is declared at the first
    // site that reaches it and nowhere else, so there is no
    // REDECLARED_SYMBOL.
    let r = resolve(
        "/main.leek",
        &[
            ("/main.leek", "include(\"a\")\ninclude(\"b\")\n"),
            ("/a.leek", "include(\"shared\")\n"),
            ("/b.leek", "include(\"shared\")\n"),
            ("/shared.leek", "var s = 1\n"),
        ],
    );
    assert!(
        r.diags(codes::REDECLARED_SYMBOL).is_empty(),
        "{:?}",
        r.result.diagnostics
    );
}

#[test]
fn a_cycle_back_to_the_entry_does_not_re_enter_the_entrys_main_block() {
    // `build_include_graph` records the back edge before it detects the
    // cycle, so an expander that did not treat the entry as already
    // expanded would recurse forever (or double-declare `e`).
    let r = resolve(
        "/main.leek",
        &[
            ("/main.leek", "var e = 1\ninclude(\"a\")\n"),
            ("/a.leek", "include(\"main\")\nvar a = 1\n"),
        ],
    );
    assert_eq!(
        r.diags(codes::CIRCULAR_INCLUDE).len(),
        1,
        "{:?}",
        r.result.diagnostics
    );
    assert!(
        r.diags(codes::REDECLARED_SYMBOL).is_empty(),
        "{:?}",
        r.result.diagnostics
    );
}

// ---- Per-file source ids, versions and options ----

#[test]
fn a_diagnostic_in_an_included_file_carries_that_files_source_id() {
    let r = resolve(
        "/main.leek",
        &[
            ("/main.leek", "include(\"a\")\n"),
            ("/a.leek", "var d = 1\nvar d = 2\n"),
        ],
    );
    let diags = r.diags(codes::REDECLARED_SYMBOL);
    assert_eq!(diags.len(), 1, "{:?}", r.result.diagnostics);
    assert_eq!(diags[0].span.source, r.source_of("/a.leek"));
    assert_ne!(diags[0].span.source, r.source_of("/main.leek"));
}

#[test]
fn an_included_file_is_resolved_at_its_own_version() {
    // v2 lexes `and` as a keyword; v4 lexes it as an identifier. If the
    // included file were walked at the entry's version, `and` would
    // become an unknown variable here.
    let r = resolve(
        "/main.leek",
        &[
            ("/main.leek", "include(\"old\")\n"),
            ("/old.leek", "// @version:2\nvar t = 1 and 0\n"),
        ],
    );
    assert!(
        r.diags(codes::UNKNOWN_VARIABLE).is_empty(),
        "{:?}",
        r.result.diagnostics
    );
}

#[test]
fn options_apply_to_included_files_too() {
    // Two same-named functions are a REDECLARED_SYMBOL unless
    // `experimental_overloads` is on. The flag has to reach every file
    // of the closure, not just the entry.
    const FILES: &[(&str, &str)] = &[
        ("/main.leek", "include(\"a\")\n"),
        (
            "/a.leek",
            "function f() { return 1; }\nfunction f(x) { return x; }\n",
        ),
    ];

    let default = resolve("/main.leek", FILES);
    assert_eq!(
        default.diags(codes::REDECLARED_SYMBOL).len(),
        1,
        "{:?}",
        default.result.diagnostics
    );

    let overloads = resolve_with(
        "/main.leek",
        FILES,
        Options {
            experimental_overloads: true,
            ..Options::default()
        },
    );
    assert!(
        overloads.diags(codes::REDECLARED_SYMBOL).is_empty(),
        "{:?}",
        overloads.result.diagnostics
    );
}

#[test]
fn the_experimental_import_gate_applies_to_an_included_file() {
    // The gate is read off `Options`, so an `import` written in an
    // included file is rejected exactly like one in the entry.
    let r = resolve(
        "/main.leek",
        &[
            ("/main.leek", "include(\"a\")\n"),
            ("/a.leek", "import foo\n"),
        ],
    );
    let diags = r.diags(codes::AI_NOT_EXISTING);
    assert_eq!(diags.len(), 1, "{:?}", r.result.diagnostics);
    assert_eq!(diags[0].span.source, r.source_of("/a.leek"));
    assert!(diags[0].message.contains("experimental"));
}

#[test]
fn without_an_include_graph_every_files_main_statements_still_run() {
    // The graph-less shape a caller with no include closure gets: files
    // in slice order, `include(...)` sites inert.
    let a_text = "var dup = 1\nvar dup = 2\n";
    let entry_text = "var e = 1\n";
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
    let result = resolve_collecting_files(&units, None, Options::default());
    let redeclared: Vec<_> = result
        .diagnostics
        .iter()
        .filter(|d| d.code == codes::REDECLARED_SYMBOL)
        .collect();
    assert_eq!(redeclared.len(), 1, "{:?}", result.diagnostics);
}
