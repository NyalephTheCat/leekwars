//! The include-graph queries: what they compute, what re-runs when a
//! file changes, and that they agree with the folder-backed walk they
//! are the memoized half of.
//!
//! The fixture and the event-logging database live in [`support`],
//! shared with `program_queries.rs`.

mod support;

use std::path::{Path, PathBuf};

use leek_db::queries::{IncludeGraph, include_edges, include_parse_failures, resolve_include};
use leek_diagnostics::{Diagnostic, codes};
use leek_resolver::closure::resolve_include_closure;
use leek_resolver::folder::MemFolder;
use leek_resolver::include_graph::{IncludeGraphResult, build_include_graph};
use leek_resolver::interner::{PathInterner, SourceInterner};
use leek_span::{FeatureFlags, SourceId};
use leek_syntax::Version;
use support::{Fixture, ran, vpath};

/// The same closure, walked the non-memoized way: a `MemFolder` over
/// the same texts and an interner pre-bound to the same ids, so paths,
/// versions and spans are directly comparable.
fn pure_walk(fixture: &Fixture, entry: &str, version: Version) -> IncludeGraphResult {
    let (folder, interner) = pure_inputs(fixture);
    let entry_path = vpath(entry);
    let entry_text = fixture.text(entry);
    build_include_graph(
        Path::new(&entry_path),
        &entry_text,
        version,
        &folder,
        |path| interner.intern(path),
    )
}

/// The pure closure's diagnostics, which include the per-site
/// `INCLUDE_PARSE_FAILED` reports the memoized path must match.
fn pure_closure_diagnostics(fixture: &Fixture, entry: &str, version: Version) -> Vec<Diagnostic> {
    let (folder, interner) = pure_inputs(fixture);
    let entry_path = vpath(entry);
    let entry_text = fixture.text(entry);
    let (_, diagnostics) = resolve_include_closure(
        Path::new(&entry_path),
        &entry_text,
        version,
        &folder,
        &interner,
        FeatureFlags::none(),
    );
    diagnostics
}

/// A folder over the fixture's texts and an interner already bound to
/// the ids its inputs carry.
fn pure_inputs(fixture: &Fixture) -> (MemFolder, PathInterner) {
    let mut folder = MemFolder::new();
    let interner = PathInterner::new();
    for (path, file) in &fixture.inputs {
        folder.insert(path.clone(), file.text(&fixture.db).as_ref());
        interner.assign(Path::new(path), file.source(&fixture.db));
    }
    (folder, interner)
}

fn version_of(graph: &IncludeGraph, name: &str) -> Version {
    graph
        .file(Path::new(&vpath(name)))
        .expect("file is in the graph")
        .version
}

fn paths(graph: &IncludeGraph) -> Vec<PathBuf> {
    graph.files.iter().map(|f| f.path.clone()).collect()
}

// ---- What the graph computes ----

#[test]
fn the_graph_orders_leaves_first_and_ends_with_the_entry() {
    let fixture = Fixture::new(&[
        ("main.leek", "include(\"a\")\nvar m = 1;\n"),
        ("a.leek", "include(\"b\")\nvar a = 1;\n"),
        ("b.leek", "var b = 1;\n"),
    ]);
    let graph = fixture.graph("main.leek", Version::V4);
    assert!(graph.diagnostics.is_empty(), "{:?}", graph.diagnostics);
    assert_eq!(
        paths(&graph),
        [
            PathBuf::from(vpath("b.leek")),
            PathBuf::from(vpath("a.leek")),
            PathBuf::from(vpath("main.leek")),
        ]
    );
    let includes: Vec<_> = graph.includes().map(|f| f.path.clone()).collect();
    assert_eq!(
        includes,
        [
            PathBuf::from(vpath("b.leek")),
            PathBuf::from(vpath("a.leek"))
        ],
        "the entry is not one of its own includes"
    );
}

#[test]
fn a_name_the_workspace_has_no_file_for_is_reported_at_its_site() {
    let fixture = Fixture::new(&[("main.leek", "include(\"ghost\")\n")]);
    let graph = fixture.graph("main.leek", Version::V4);
    assert_eq!(graph.diagnostics.len(), 1, "{:?}", graph.diagnostics);
    assert_eq!(graph.diagnostics[0].code, codes::INCLUDE_NOT_FOUND);
    assert_eq!(
        graph.diagnostics[0].span.source,
        SourceId::new(1).expect("non-zero"),
        "anchored in the includer, not the missing file"
    );
}

#[test]
fn the_class_set_spans_the_whole_closure() {
    let fixture = Fixture::new(&[
        ("main.leek", "include(\"a\")\nclass entryClass {}\n"),
        ("a.leek", "class leafClass {}\nclass leafClass2 {}\n"),
    ]);
    let graph = fixture.graph("main.leek", Version::V4);
    assert_eq!(
        graph.class_names(),
        ["entryClass", "leafClass", "leafClass2"]
    );
}

/// The entry's settled version is the caller's, and a pragma-less
/// include inherits it — the rule
/// `entry_uses_caller_version_and_pragmaless_include_inherits_it`
/// pins for the folder-backed walk. Here it is also why the version
/// has to be part of the graph's key: the same leaf, in the same
/// workspace, settles differently under two entries.
#[test]
fn a_pragmaless_leaf_inherits_the_entry_version_the_graph_was_keyed_on() {
    let fixture = Fixture::new(&[
        ("main.leek", "include(\"util\")\n"),
        ("util.leek", "var x = 1;\n"),
    ]);
    let v2 = fixture.graph("main.leek", Version::V2);
    let v4 = fixture.graph("main.leek", Version::V4);
    assert_eq!(version_of(&v2, "main.leek"), Version::V2);
    assert_eq!(version_of(&v2, "util.leek"), Version::V2);
    assert_eq!(version_of(&v4, "main.leek"), Version::V4);
    assert_eq!(
        version_of(&v4, "util.leek"),
        Version::V4,
        "the v2 answer must not have been served out of the cache"
    );
}

#[test]
fn an_explicit_pragma_on_a_leaf_beats_the_entry_version() {
    let fixture = Fixture::new(&[
        ("main.leek", "include(\"old\")\n"),
        ("old.leek", "// @version:1\nvar x = 1;\n"),
    ]);
    let graph = fixture.graph("main.leek", Version::V4);
    assert_eq!(version_of(&graph, "old.leek"), Version::V1);
}

#[test]
fn a_cycle_is_reported_at_the_include_that_closed_it() {
    let fixture = Fixture::new(&[("a.leek", "include(\"b\")"), ("b.leek", "include(\"a\")")]);
    let graph = fixture.graph("a.leek", Version::V4);
    let cycle = graph
        .diagnostics
        .iter()
        .find(|d| d.code == codes::CIRCULAR_INCLUDE)
        .expect("circular-include diagnostic");
    assert_eq!(
        cycle.span.source,
        SourceId::new(2).expect("non-zero"),
        "the site is `b.leek`'s include, not `a.leek`'s"
    );
}

// ---- Agreement with the folder-backed walk ----

/// The tracked walk and the pure one are two DFS bodies over the same
/// pure scan and the same candidate order. This pins them to the same
/// answer on a fixture that exercises a diamond, a chain, a cycle-free
/// re-entry and a missing name at once.
#[test]
fn the_tracked_graph_matches_the_folder_backed_walk() {
    let fixture = Fixture::new(&[
        (
            "main.leek",
            "include(\"a\")\ninclude(\"b\")\ninclude(\"ghost\")\n",
        ),
        ("a.leek", "include(\"shared\")\nclass alpha {}\n"),
        ("b.leek", "include(\"shared\")\n"),
        ("shared.leek", "// @version:2\nfunction helper() {}\n"),
    ]);
    let tracked = fixture.graph("main.leek", Version::V4);
    let pure = pure_walk(&fixture, "main.leek", Version::V4);

    assert_eq!(
        paths(&tracked),
        pure.files
            .iter()
            .map(|f| f.path.clone())
            .collect::<Vec<_>>(),
        "same dependency order"
    );
    assert_eq!(
        tracked.files.iter().map(|f| f.version).collect::<Vec<_>>(),
        pure.files.iter().map(|f| f.version).collect::<Vec<_>>(),
        "same settled versions"
    );
    assert_eq!(
        tracked.files.iter().map(|f| f.source).collect::<Vec<_>>(),
        pure.files.iter().map(|f| f.source).collect::<Vec<_>>(),
        "same source ids"
    );
    assert_eq!(
        tracked.files.iter().map(|f| &f.classes).collect::<Vec<_>>(),
        pure.files.iter().map(|f| &f.classes).collect::<Vec<_>>(),
        "same class declarations"
    );
    assert_eq!(tracked.forward, pure.forward, "same edges");
    assert_eq!(tracked.resolved, pure.resolved, "same name resolution");
    assert_eq!(
        tracked.include_sites, pure.include_sites,
        "same include sites"
    );
    assert_eq!(
        tracked.diagnostics, pure.diagnostics,
        "same diagnostics, verbatim"
    );
}

// ---- Diagnostics survive the cache ----

/// The failure this guards against is a diagnostic that only ever
/// appears while a memo is being filled: the first `publishDiagnostics`
/// after an edit reports a broken include and every later one silently
/// does not.
#[test]
fn the_per_site_parse_failures_match_the_pure_closure_and_replay_on_a_hit() {
    let fixture = Fixture::new(&[
        ("main.leek", "include(\"a\")\ninclude(\"b\")\n"),
        ("a.leek", "include(\"bad\")\n"),
        ("b.leek", "include(\"bad\")\n"),
        ("bad.leek", "function ( { var\n"),
    ]);
    let entry = fixture.file("main.leek");

    let first = include_parse_failures(&fixture.db, fixture.files, entry, Version::V4);
    assert_eq!(first.len(), 2, "one per include site: {first:?}");
    assert!(first.iter().all(|d| d.code == codes::INCLUDE_PARSE_FAILED));
    assert_ne!(
        first[0].span.source, first[1].span.source,
        "the two sites are in different files"
    );

    let second = include_parse_failures(&fixture.db, fixture.files, entry, Version::V4);
    assert_eq!(
        first, second,
        "a cache hit must report exactly what the miss reported"
    );

    let pure: Vec<Diagnostic> = pure_closure_diagnostics(&fixture, "main.leek", Version::V4)
        .into_iter()
        .filter(|d| d.code == codes::INCLUDE_PARSE_FAILED)
        .collect();
    assert_eq!(
        first, pure,
        "the memoized path must raise the same diagnostics, at the same spans, as the pure closure"
    );
}

// ---- What re-runs ----

/// A leaf edit that changes what the leaf declares walks exactly one
/// chain: that leaf's lex, that leaf's scan, the graph. Not the
/// entry's lex, not anyone's parse.
#[test]
fn editing_a_leaf_reruns_that_leafs_lex_and_the_graph_and_nothing_else() {
    let mut fixture = Fixture::new(&[
        ("main.leek", "include(\"util\")\nvar m = 1;\n"),
        ("util.leek", "var x = 1;\n"),
    ]);
    let entry = fixture.file("main.leek");
    let _ = fixture.graph("main.leek", Version::V4);
    let _ = fixture.db.drain();

    fixture.edit("util.leek", "class leaf {}\nvar x = 1;\n");
    let _ = fixture.graph("main.leek", Version::V4);
    let events = fixture.db.drain();

    assert_eq!(ran(&events, "lex_query"), 1, "{events:?}");
    assert_eq!(ran(&events, "include_edges"), 1, "{events:?}");
    assert_eq!(ran(&events, "include_graph"), 1, "{events:?}");
    assert_eq!(
        ran(&events, "parse_query"),
        0,
        "no file is parsed: {events:?}"
    );
    assert_eq!(
        ran(&events, "resolve_include"),
        0,
        "the file set did not change: {events:?}"
    );
    // And the entry's own scan is untouched, so nothing hanging off it
    // is invalidated either.
    assert_eq!(
        include_edges(&fixture.db, entry).includes.len(),
        1,
        "the entry still includes one file"
    );
    assert_eq!(
        ran(&fixture.db.drain(), "include_edges"),
        0,
        "the entry's scan was a cache hit"
    );
}

/// The incremental win, stated the other way round: an edit that
/// leaves the include sites and class names alone stops at the scan.
/// The graph is not even asked to recompute, because salsa sees the
/// scan's value is unchanged.
#[test]
fn a_leaf_edit_that_changes_no_include_edge_stops_at_the_scan() {
    let mut fixture = Fixture::new(&[
        ("main.leek", "include(\"util\")\n"),
        ("util.leek", "var x = 1;\n"),
    ]);
    let _ = fixture.graph("main.leek", Version::V4);
    let _ = fixture.db.drain();

    fixture.edit("util.leek", "var x = 2;\n");
    let _ = fixture.graph("main.leek", Version::V4);
    let events = fixture.db.drain();

    assert_eq!(ran(&events, "lex_query"), 1, "{events:?}");
    assert_eq!(ran(&events, "include_edges"), 1, "{events:?}");
    assert_eq!(
        ran(&events, "include_graph"),
        0,
        "the edges are unchanged, so the graph is still valid: {events:?}"
    );
}

#[test]
fn editing_the_entry_body_reruns_no_leaf() {
    let mut fixture = Fixture::new(&[
        ("main.leek", "include(\"util\")\nvar m = 1;\n"),
        ("util.leek", "var x = 1;\n"),
    ]);
    let leaf = fixture.file("util.leek");
    let _ = fixture.graph("main.leek", Version::V4);
    let _ = fixture.db.drain();

    fixture.edit("main.leek", "include(\"util\")\nvar m = 2;\nvar n = 3;\n");
    let _ = fixture.graph("main.leek", Version::V4);
    let events = fixture.db.drain();

    assert_eq!(
        ran(&events, "lex_query"),
        1,
        "only the entry is re-lexed: {events:?}"
    );
    assert_eq!(ran(&events, "include_edges"), 1, "{events:?}");
    // The leaf's scan is still the cached one.
    let _ = include_edges(&fixture.db, leaf);
    assert_eq!(
        ran(&fixture.db.drain(), "include_edges"),
        0,
        "the leaf was neither re-lexed nor re-scanned"
    );
}

#[test]
fn adding_an_include_line_reruns_the_graph_and_the_newly_reachable_leaf() {
    let mut fixture = Fixture::new(&[
        ("main.leek", "include(\"util\")\n"),
        ("util.leek", "var x = 1;\n"),
        ("extra.leek", "var y = 2;\n"),
    ]);
    let before = fixture.graph("main.leek", Version::V4);
    assert_eq!(before.includes().count(), 1);
    let _ = fixture.db.drain();

    fixture.edit("main.leek", "include(\"util\")\ninclude(\"extra\")\n");
    let after = fixture.graph("main.leek", Version::V4);
    let events = fixture.db.drain();

    assert_eq!(after.includes().count(), 2, "the new leaf is reachable");
    assert_eq!(ran(&events, "include_graph"), 1, "{events:?}");
    assert_eq!(
        ran(&events, "resolve_include"),
        1,
        "only the new name is resolved; the old one is a hit: {events:?}"
    );
    assert_eq!(
        ran(&events, "lex_query"),
        2,
        "the entry, re-lexed, and the newly reached leaf: {events:?}"
    );
    assert_eq!(
        ran(&events, "include_edges"),
        2,
        "the entry's scan and the new leaf's: {events:?}"
    );
}

// ---- The candidate order ----

#[test]
fn a_name_resolves_to_the_sibling_dot_leek_before_the_bare_sibling() {
    let fixture = Fixture::new(&[
        ("main.leek", ""),
        ("util.leek", "var from_dot_leek = 1;\n"),
        ("util", "var from_bare = 1;\n"),
    ]);
    let site =
        leek_db::queries::IncludeRef::new(&fixture.db, vpath("main.leek"), "util".to_string());
    let resolved = resolve_include(&fixture.db, fixture.files, site).expect("resolves");
    assert_eq!(resolved.canonical_path(&fixture.db), &vpath("util.leek"));
}

#[test]
fn a_name_the_workspace_does_not_hold_resolves_to_nothing() {
    let fixture = Fixture::new(&[("main.leek", "")]);
    let site =
        leek_db::queries::IncludeRef::new(&fixture.db, vpath("main.leek"), "ghost".to_string());
    assert!(resolve_include(&fixture.db, fixture.files, site).is_none());
}
