//! Charging HIR the frontend actually produces.
//!
//! The unit tests in `src/lib.rs` assemble [`HirFile`]s by hand, which is
//! precise about arithmetic but says nothing about the shapes lowering
//! emits. These run real source through parse + lower and charge the
//! result — the end-to-end reach the deleted `pipeline.rs` salsa tests
//! carried, restated against [`add_charges`] directly.

use leek_charge::{ChargeOpts, add_charges};
use leek_hir::{Def, HirFile, Stmt, lower_file};
use leek_parser::ast::{AstNode, SourceFile};
use leek_parser::{ParseFeatures, parse_with_features};
use leek_span::SourceId;
use leek_syntax::{SyntaxNode, Version};

fn lower(src: &str) -> HirFile {
    let source = SourceId::new(1).expect("source id");
    let parsed = parse_with_features(src, source, Version::V4, ParseFeatures::default());
    let file = SourceFile::cast(SyntaxNode::new_root(parsed.green)).expect("parses");
    lower_file(&file, source).0
}

/// The `Charge` payload a block starts with, or `None` if it has none.
fn entry_charge(stmts: &[Stmt]) -> Option<u64> {
    match stmts.first() {
        Some(Stmt::Charge(n)) => Some(*n),
        _ => None,
    }
}

#[test]
fn lowered_source_is_charged_in_main_and_in_every_function_body() {
    let hir = lower("function add(a, b) { return a + b; }\nvar x = add(1, 2);\n");
    let charged = add_charges(&hir, ChargeOpts::default());

    assert!(
        entry_charge(&charged.main).is_some_and(|n| n > 0),
        "main lowered from source must open with a charge: {:?}",
        charged.main
    );

    let bodies: Vec<_> = charged
        .defs
        .iter()
        .filter_map(|d| match d {
            Def::Function(f) => f.body.as_ref(),
            _ => None,
        })
        .collect();
    assert!(!bodies.is_empty(), "the source declares a function");
    for body in bodies {
        assert!(
            entry_charge(&body.stmts).is_some_and(|n| n > 0),
            "a lowered function body must open with a charge: {:?}",
            body.stmts
        );
    }
}

#[test]
fn more_statements_cost_more() {
    // What the deleted memoization test was really watching: charging is a
    // function of the program, so handing the pass a longer program must
    // debit more than the short one, never repeat the earlier answer.
    let short = add_charges(&lower("var x = 1;\n"), ChargeOpts::default());
    let long = add_charges(
        &lower("var x = 1;\nvar y = 2;\nvar z = 3;\n"),
        ChargeOpts::default(),
    );

    let short_charge = entry_charge(&short.main).expect("charged");
    let long_charge = entry_charge(&long.main).expect("charged");
    assert!(
        long_charge > short_charge,
        "three statements ({long_charge}) must cost more than one ({short_charge})"
    );
}
