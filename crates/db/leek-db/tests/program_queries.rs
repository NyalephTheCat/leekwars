//! The whole-program queries and the parse key they turn on.
//!
//! Three things are pinned here.
//!
//! 1. **What re-runs a parse.** These tests used to live in
//!    `leek-parser` and watch a counter inside `parse_query`; they moved
//!    when the program-wide class set stopped being a field on
//!    `SourceFile` and became a query argument, because the contract
//!    they pin is no longer about that crate's input fields. They watch
//!    salsa's event stream instead of a counter, so the same assertions
//!    survive the query moving crates again.
//! 2. **What the class key buys.** One leaf, two programs, two parses —
//!    and an edit inside one program that leaves its class set alone
//!    re-parses only the file that changed.
//! 3. **What the program queries compute.** Resolve, type check and
//!    lower see the whole include closure, which the single-file
//!    queries beside them deliberately do not.

mod support;

use std::sync::Arc;

use leek_db::queries::{
    class_names, lower_program, parse_query, program_classes, resolve_program, typecheck_program,
};
use leek_db::{ProgramClasses, SourceFile};
use leek_diagnostics::Severity;
use leek_query::OptLevel;
use leek_syntax::Version;
use support::{EventDb, Fixture, ran};

// ---- What re-runs a parse ----

/// A class declared in the file itself, found by the parser's own token
/// pre-scan — so the fixture parses cleanly under the *empty* program
/// class set and the reruns below are about nothing else.
const SRC: &str = "class c {}\nc x = new c();\n";

/// Prime the cache, apply `edit`, ask again, and report how many times
/// `parse_query` executed the second time.
fn reruns_after(edit: impl FnOnce(&mut EventDb, SourceFile)) -> usize {
    let mut fixture = Fixture::new(&[("main.leek", SRC)]);
    let file = fixture.file("main.leek");

    let _ = parse_query(&fixture.db, file, ProgramClasses::none(&fixture.db));
    assert_eq!(
        ran(&fixture.db.drain(), "parse_query"),
        1,
        "the first ask must execute the query"
    );

    edit(&mut fixture.db, file);

    let _ = parse_query(&fixture.db, file, ProgramClasses::none(&fixture.db));
    ran(&fixture.db.drain(), "parse_query")
}

#[test]
fn an_untouched_input_reuses_the_cached_parse() {
    assert_eq!(reruns_after(|_, _| {}), 0, "no edit, no work");
}

/// Salsa inputs do not compare: `set_x().to(same_value)` bumps the
/// revision and re-runs every query that read `x`, equal or not. That is
/// why `leek_lsp::workspace::apply_language` reads each field back and
/// writes only the ones that differ — dropping that guard would
/// re-parse the open document on every `didChange`.
#[test]
fn re_setting_a_field_to_its_current_value_still_reparses() {
    assert_eq!(
        reruns_after(|db, file| {
            use salsa::Setter;
            file.set_version_byte(db).to(4);
        }),
        1,
        "a no-op write is still a write as far as salsa is concerned"
    );
}

#[test]
fn changing_the_language_version_reparses() {
    // The version picks the grammar, so a cached parse from another
    // version is simply the wrong tree.
    assert_eq!(
        reruns_after(|db, file| {
            use salsa::Setter;
            file.set_version_byte(db).to(1);
        }),
        1
    );
}

#[test]
fn changing_the_experimental_feature_flags_reparses() {
    // `flags_bits` becomes `ParseFeatures`, which gates syntax
    // (generics, enums, interfaces …).
    assert_eq!(
        reruns_after(|db, file| {
            use salsa::Setter;
            file.set_flags_bits(db).to(0b1111_1111);
        }),
        1
    );
}

#[test]
fn changing_strict_does_not_reparse() {
    // `strict` is a type-checker input; parsing must not depend on it,
    // or toggling it would throw away every green tree in the cache.
    assert_eq!(
        reruns_after(|db, file| {
            use salsa::Setter;
            file.set_strict(db).to(true);
        }),
        0
    );
}

// ---- What the class key buys ----

/// The class set used to be a field on the file, so one file had one
/// parse and pushing a new set threw it away. As a key it is a second
/// memo entry instead: asking under a new set computes, and asking
/// again under the old one is still a hit.
#[test]
fn two_class_sets_are_two_parses_of_one_file_not_one_that_keeps_moving() {
    let fixture = Fixture::new(&[("main.leek", "fromAnotherFile y = 1;\n")]);
    let file = fixture.file("main.leek");
    let none = ProgramClasses::none(&fixture.db);
    let known = ProgramClasses::new(&fixture.db, vec!["fromAnotherFile".to_string()]);

    let bare = parse_query(&fixture.db, file, none);
    let _ = fixture.db.drain();

    let typed = parse_query(&fixture.db, file, known);
    assert_eq!(
        ran(&fixture.db.drain(), "parse_query"),
        1,
        "a different class set is a different question"
    );
    assert_ne!(
        bare.green, typed.green,
        "the class set decides whether `fromAnotherFile y = 1;` is a typed declaration"
    );

    assert_eq!(parse_query(&fixture.db, file, none).green, bare.green);
    assert_eq!(
        ran(&fixture.db.drain(), "parse_query"),
        0,
        "the first set's parse is still cached, not evicted by the second"
    );
}

/// The same leaf, in two programs whose class sets differ, parses
/// differently — which is exactly what a single workspace-wide union
/// could not express (#163).
#[test]
fn one_leaf_parses_differently_under_two_programs() {
    let fixture = Fixture::new(&[
        ("with.leek", "include(\"util\")\nclass fromAnotherFile {}\n"),
        ("without.leek", "include(\"util\")\n"),
        ("util.leek", "fromAnotherFile y = 1;\n"),
    ]);
    let leaf = fixture.file("util.leek");

    let with = program_classes(
        &fixture.db,
        fixture.files,
        fixture.file("with.leek"),
        Version::V4,
    );
    let without = program_classes(
        &fixture.db,
        fixture.files,
        fixture.file("without.leek"),
        Version::V4,
    );
    assert_eq!(with.names(&fixture.db), &["fromAnotherFile".to_string()]);
    assert!(without.names(&fixture.db).is_empty());

    assert_ne!(
        parse_query(&fixture.db, leaf, with).green,
        parse_query(&fixture.db, leaf, without).green,
        "the leaf is the same file; the programs around it are not"
    );
}

#[test]
fn class_names_are_one_files_declarations_in_order() {
    let fixture = Fixture::new(&[(
        "main.leek",
        "class second {}\nclass first {}\ninclude(\"other\")\n",
    )]);
    assert_eq!(
        class_names(&fixture.db, fixture.file("main.leek")),
        ["second", "first"],
        "declaration order, not sorted — the fold sorts"
    );
}

#[test]
fn program_classes_spans_the_closure_sorted_and_deduplicated() {
    let fixture = Fixture::new(&[
        ("main.leek", "include(\"a\")\nclass shared {}\n"),
        ("a.leek", "include(\"b\")\nclass alpha {}\n"),
        ("b.leek", "class shared {}\nclass beta {}\n"),
    ]);
    let classes = program_classes(
        &fixture.db,
        fixture.files,
        fixture.file("main.leek"),
        Version::V4,
    );
    assert_eq!(classes.names(&fixture.db), &["alpha", "beta", "shared"]);
    assert_eq!(
        classes.names(&fixture.db),
        &fixture.graph("main.leek", Version::V4).class_names(),
        "the fold over the per-file query must agree with the graph's own"
    );
}

/// The incremental claim the key exists for: an edit inside one file of
/// a program re-parses that file and nothing else, because the class
/// set — and therefore every other file's parse key — is unchanged.
#[test]
fn editing_a_leaf_body_reparses_only_that_leaf() {
    let mut fixture = Fixture::new(&[
        ("main.leek", "include(\"a\")\nreturn 1;\n"),
        ("a.leek", "include(\"b\")\nvar a = 1;\n"),
        ("b.leek", "var b = 1;\n"),
    ]);
    let entry = fixture.file("main.leek");
    let _ = resolve_program(&fixture.db, fixture.files, entry, Version::V4);
    let _ = fixture.db.drain();

    fixture.edit("b.leek", "var b = 2;\n");
    let _ = resolve_program(&fixture.db, fixture.files, entry, Version::V4);
    let events = fixture.db.drain();

    assert_eq!(
        ran(&events, "parse_query"),
        1,
        "only the edited leaf: {events:?}"
    );
    assert_eq!(
        ran(&events, "program_classes"),
        0,
        "the class set did not change, so its memo is still valid: {events:?}"
    );
}

/// The other side of it: a class *is* program-wide, so declaring one
/// re-keys every parse in the program. That cost is the semantics, not
/// a leak — upstream resolves type words against the whole program.
#[test]
fn declaring_a_class_in_a_leaf_reparses_the_whole_program() {
    let mut fixture = Fixture::new(&[
        ("main.leek", "include(\"a\")\nreturn 1;\n"),
        ("a.leek", "include(\"b\")\nvar a = 1;\n"),
        ("b.leek", "var b = 1;\n"),
    ]);
    let entry = fixture.file("main.leek");
    let _ = resolve_program(&fixture.db, fixture.files, entry, Version::V4);
    let _ = fixture.db.drain();

    fixture.edit("b.leek", "class late {}\nvar b = 1;\n");
    let _ = resolve_program(&fixture.db, fixture.files, entry, Version::V4);
    let events = fixture.db.drain();

    assert_eq!(ran(&events, "program_classes"), 1, "{events:?}");
    assert_eq!(
        ran(&events, "parse_query"),
        3,
        "every file is now asked a different question: {events:?}"
    );
}

// ---- What the program queries compute ----

fn errors(diagnostics: &[leek_diagnostics::Diagnostic]) -> Vec<&leek_diagnostics::Diagnostic> {
    diagnostics
        .iter()
        .filter(|d| d.severity == Severity::Error)
        .collect()
}

/// A two-file program: the entry calls a function the leaf declares.
fn two_file_program() -> Fixture {
    Fixture::new(&[
        (
            "main.leek",
            "include(\"util\")\nvar answer = helper();\nreturn answer;\n",
        ),
        ("util.leek", "function helper() {\n\treturn 1;\n}\n"),
    ])
}

#[test]
fn resolve_program_sees_a_function_declared_in_an_included_file() {
    let fixture = two_file_program();
    let entry = fixture.file("main.leek");

    let program = resolve_program(&fixture.db, fixture.files, entry, Version::V4);
    assert!(
        errors(&program.diagnostics).is_empty(),
        "the closure declares `helper`: {:?}",
        program.diagnostics
    );
    assert!(
        declares(&program.table, "helper"),
        "the included file's function is in the program's symbol table"
    );

    // The single-file query beside it is the control: it only ever sees
    // the entry, so the leaf's declaration is not in its table.
    let alone = leek_db::queries::resolve_query(&fixture.db, entry);
    assert!(
        !declares(&alone.table, "helper"),
        "the entry on its own cannot know `helper`"
    );
    assert!(
        declares(&alone.table, "answer"),
        "…while its own declarations are there, so the tables are comparable"
    );
}

/// Whether the resolve table holds a declaration called `name`.
fn declares(table: &leek_resolver::index::ResolveTable, name: &str) -> bool {
    table.symbols.iter().any(|symbol| symbol.name == name)
}

#[test]
fn typecheck_program_sees_the_included_declarations_too() {
    let fixture = two_file_program();
    let entry = fixture.file("main.leek");

    let program = typecheck_program(&fixture.db, fixture.files, entry, Version::V4);
    assert!(
        errors(&program.diagnostics).is_empty(),
        "{:?}",
        program.diagnostics
    );
    assert!(
        program.signatures.fn_returns.contains_key("helper"),
        "the checker recorded the included function's return type"
    );
}

#[test]
fn lower_program_merges_every_file_into_one_hir() {
    let fixture = two_file_program();
    let entry = fixture.file("main.leek");

    let program = lower_program(&fixture.db, fixture.files, entry, Version::V4, OptLevel::O0);
    assert!(
        program.hir.defs.iter().any(|def| def.name() == "helper"),
        "the included file's function is in the entry's HIR"
    );
}

/// `lower_program` is keyed on the optimization level, so an `O1`
/// caller reads an optimized tree out of the cache instead of cloning
/// the `O0` one and optimizing the copy on every run — which is what
/// the single-file `lower_hir_query` still leaves its callers doing.
#[test]
fn lower_program_is_keyed_on_the_optimization_level() {
    let fixture = Fixture::new(&[("main.leek", "var x = 1 + 2;\nreturn x;\n")]);
    let entry = fixture.file("main.leek");

    let unoptimized = lower_program(&fixture.db, fixture.files, entry, Version::V4, OptLevel::O0);
    let _ = fixture.db.drain();

    let optimized = lower_program(&fixture.db, fixture.files, entry, Version::V4, OptLevel::O1);
    assert_eq!(
        ran(&fixture.db.drain(), "lower_program"),
        1,
        "a different level is a different question"
    );
    assert_ne!(
        unoptimized.hir, optimized.hir,
        "`1 + 2` folds at O1 and does not at O0"
    );

    let again = lower_program(&fixture.db, fixture.files, entry, Version::V4, OptLevel::O0);
    assert_eq!(
        ran(&fixture.db.drain(), "lower_program"),
        0,
        "the O0 tree is still cached, not evicted by the O1 one"
    );
    assert!(Arc::ptr_eq(&again.hir, &unoptimized.hir));
}

// ---- One parse per file ----

/// A zero-include entry is parsed **once**.
///
/// It used to be parsed twice, and this test asserted that (#522).
/// `ResolveIncludes` published a `KnownClassesArtifact` unconditionally,
/// even for a closure of one file whose classes are empty, and that
/// artifact was exactly the gate `leek_parser::pipeline`'s `run_parse`
/// used to decide the salsa query could not serve the run — so the
/// `Parse` step parsed directly, while `Resolve`, seeing an include
/// graph with no includes, fell through to `resolve_query` and parsed
/// the same text again. Two green trees, one run, for a file with no
/// includes at all.
///
/// The fix was never a smaller gate. It was routing the include-aware
/// path through `resolve_program` / `typecheck_program` /
/// `lower_program`, which is what the whole program now does: every read
/// of a file's tree in a whole-program pass goes through `parse_query`
/// keyed on that program's own `program_classes`, so there is one memo
/// and one allocation per file.
#[test]
fn a_zero_include_entry_is_parsed_once() {
    let fixture = Fixture::new(&[("main.leek", "class c {}\nvar x = new c();\nreturn x;\n")]);
    let entry = fixture.file("main.leek");

    let _ = fixture.db.drain();
    let resolved = resolve_program(&fixture.db, fixture.files, entry, Version::V4);
    let events = fixture.db.drain();
    assert!(resolved.diagnostics.is_empty(), "{resolved:?}");
    assert_eq!(
        ran(&events, "parse_query"),
        1,
        "one parse for a one-file program: {events:?}"
    );

    // And the tree the program read is the memo, not a second copy.
    let classes = program_classes(&fixture.db, fixture.files, entry, Version::V4);
    let _ = fixture.db.drain();
    let again = parse_query(&fixture.db, entry, classes).green;
    let events = fixture.db.drain();
    assert_eq!(ran(&events, "parse_query"), 0, "a cache hit: {events:?}");
    let once_more = parse_query(&fixture.db, entry, classes).green;
    assert!(
        std::ptr::eq(&raw const *again, &raw const *once_more),
        "one shared green tree, not two allocations"
    );
}
