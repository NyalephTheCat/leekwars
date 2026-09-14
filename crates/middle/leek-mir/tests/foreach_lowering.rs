//! `foreach` lowering (#111). The loop reads the snapshot directly — no
//! per-element `[key, value]` pair to materialise and unpack — and the op
//! charges it emits are unchanged by that: they are the corpus baseline's
//! contract, so they are asserted here exactly rather than by inspection.

use leek_mir::{MirProgram, Rvalue, Statement, lower_file};
use leek_parser::{ParseFeatures, ast::AstNode, parse_with_features};
use leek_span::SourceId;
use leek_syntax::{SyntaxNode, Version};

fn lower(src: &str) -> MirProgram {
    let s = SourceId::new(1).unwrap();
    let p = parse_with_features(src, s, Version::V4, ParseFeatures::default());
    let sf = leek_parser::ast::SourceFile::cast(SyntaxNode::new_root(p.green)).expect("parse");
    let (h, _) = leek_hir::lower_file_versioned(&sf, s, 4);
    let (program, errs) = lower_file(&h);
    assert!(errs.is_empty(), "unexpected lowering errors: {errs:?}");
    program
}

fn main_statements(p: &MirProgram) -> Vec<&Statement> {
    p.main()
        .expect("main")
        .blocks
        .iter()
        .flat_map(|b| &b.statements)
        .collect()
}

/// How many times `main` assigns each rvalue shape the foreach lowering can
/// emit: (index reads, snapshot value reads, snapshot key reads).
fn read_counts(p: &MirProgram) -> (usize, usize, usize) {
    let mut counts = (0, 0, 0);
    for s in main_statements(p) {
        let Statement::Assign(_, rv) = s else {
            continue;
        };
        // An `Index` the lowering synthesized and a plain one both count: the
        // pair path emitted one of each.
        let rv = match rv {
            Rvalue::Synthetic(inner) => &**inner,
            other => other,
        };
        match rv {
            Rvalue::Index(..) => counts.0 += 1,
            Rvalue::ForeachValueAt(..) => counts.1 += 1,
            Rvalue::ForeachKeyAt(..) => counts.2 += 1,
            _ => {}
        }
    }
    counts
}

/// `main`'s charge statements, in program order.
fn charges(p: &MirProgram) -> Vec<Statement> {
    main_statements(p)
        .into_iter()
        .filter(|s| matches!(s, Statement::Charge(_) | Statement::ChargeVersioned { .. }))
        .cloned()
        .collect()
}

#[test]
fn a_value_foreach_reads_the_snapshot_without_a_pair() {
    let p = lower("for (var v in [1, 2]) {}");
    // One read per iteration — the value — and no index read at all: the old
    // lowering paid `iter[pos]` for the pair plus `pair[1]` for the value.
    assert_eq!(read_counts(&p), (0, 1, 0));
    assert_eq!(
        main_statements(&p)
            .iter()
            .filter(|s| matches!(s, Statement::Assign(_, Rvalue::MakeForeachIter(_))))
            .count(),
        1,
        "the iterable is snapshotted exactly once"
    );
}

#[test]
fn a_key_value_foreach_reads_key_and_value_without_a_pair() {
    let p = lower("for (var k : var v in [1, 2]) {}");
    assert_eq!(read_counts(&p), (0, 1, 1));
}

#[test]
fn a_bare_binding_still_stores_through_its_l_value() {
    // #370: `for (g in …)` writes the global, not a fresh local. The store's
    // place must survive the switch to a direct snapshot read.
    let p = lower("global g = 0 for (g in [1, 2]) {}");
    let stores: Vec<_> = main_statements(&p)
        .into_iter()
        .filter_map(|s| match s {
            Statement::Assign(place, Rvalue::ForeachValueAt(..)) => Some(place.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(stores.len(), 1);
    assert!(
        matches!(&stores[0], leek_mir::Place::Global(..)),
        "expected a global store, got {:?}",
        stores[0]
    );
}

#[test]
fn the_charge_sequence_is_unchanged() {
    // Value form: 1 op of setup, then per iteration 2 at v1 (the by-value
    // copy-on-set) and 1 at v2+.
    assert_eq!(
        charges(&lower("for (var v in [1, 2]) {}")),
        [
            Statement::Charge(1),
            Statement::ChargeVersioned { v1: 2, vn: 1 }
        ]
    );
    // `@`-by-ref value binding skips the copy: 1 op at every version.
    assert_eq!(
        charges(&lower("for (var @v in [1, 2]) {}")),
        [Statement::Charge(1), Statement::Charge(1)]
    );
    // Key:value form: 1 op before the iterability check, 1 per *declared*
    // slot of setup, then per iteration 1 per non-`@ref` slot at v1 and
    // nothing at v2+.
    assert_eq!(
        charges(&lower("for (var k : var v in [1, 2]) {}")),
        [
            Statement::Charge(1),
            Statement::Charge(2),
            Statement::ChargeVersioned { v1: 2, vn: 0 }
        ]
    );
}
