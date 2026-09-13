//! Nested index assignments (`a[i][j] = v`) lower every sub-expression
//! exactly once, and the v1-v3 promotion write-back is an uncharged
//! `Synthetic` store (#49, #79).

use leek_mir::{MirProgram, Place, Rvalue, Statement, lower_file};
use leek_parser::{ast::AstNode, parse};
use leek_span::SourceId;
use leek_syntax::{SyntaxNode, Version};

fn lower(src: &str) -> MirProgram {
    let s = SourceId::new(1).unwrap();
    let p = parse(src, s, Version::V4);
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

#[test]
fn nested_index_assignment_calls_each_index_once() {
    let p = lower(
        "function f() { return 0 } function g() { return 1 } \
         var a = [[0, 0]] a[f()][g()] = 1",
    );
    let calls = main_statements(&p)
        .into_iter()
        .filter(|s| matches!(s, Statement::Call { .. }))
        .count();
    assert_eq!(calls, 2, "`f()` and `g()` must each be called exactly once");
}

#[test]
fn three_level_assignment_reads_each_level_once() {
    let p = lower("var a = [[[0]]] a[0][0][0] = 1");
    let stmts = main_statements(&p);
    let reads = stmts
        .iter()
        .filter(|s| matches!(s, Statement::Assign(_, Rvalue::Index(..))))
        .count();
    // `a[0]` and `a[0][0]` are read once each for the place; the
    // post-assignment read-back of `a[0][0][0]` is the third.
    assert_eq!(reads, 3);
    let writebacks: Vec<_> = stmts
        .iter()
        .filter_map(|s| match s {
            Statement::Assign(Place::Index(base, _), Rvalue::Synthetic(inner)) => {
                Some((*base, (**inner).clone()))
            }
            _ => None,
        })
        .collect();
    assert_eq!(writebacks.len(), 2, "one write-back per nested level");
    // Innermost level first: `a[0][0]` is stored into `a[0]` before
    // `a[0]` is stored into `a`.
    let Rvalue::Use(leek_mir::Operand::Local(first_value)) = &writebacks[0].1 else {
        panic!("write-back must store a local: {:?}", writebacks[0].1);
    };
    assert_ne!(*first_value, writebacks[1].0);
    let Rvalue::Use(leek_mir::Operand::Local(second_value)) = &writebacks[1].1 else {
        panic!("write-back must store a local: {:?}", writebacks[1].1);
    };
    assert_eq!(*second_value, writebacks[0].0);
}
