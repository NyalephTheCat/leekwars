//! Verify the resolver pre-declares the var name before resolving
//! a lambda init, so `var f = function() { f() }` records the
//! inner `f` as a reference to the outer var.

use leek_diagnostics::Severity;
use leek_parser::{ast::AstNode, ast::SourceFile, parse};
use leek_resolver::resolve_collecting;
use leek_span::SourceId;
use leek_syntax::{SyntaxNode, Version};

#[test]
fn recursive_var_lambda_resolves_self_ref() {
    let src = "var fact = function(x) { if (x == 1) { return 1 } else { return fact(x - 1) * x } } return fact(8)";
    let source = SourceId::new(1).unwrap();
    let parsed = parse(src, source, Version::V4);
    assert!(
        !parsed
            .diagnostics
            .iter()
            .any(|d| d.severity == Severity::Error)
    );
    let root = SyntaxNode::new_root(parsed.green);
    let sf = SourceFile::cast(root).expect("source file");
    let result = resolve_collecting(&sf, source, Version::V4, leek_resolver::Options::default());
    // Both `fact` references in the source (inner `fact(x-1)`
    // and outer `fact(8)`) should land in the references table.
    eprintln!("symbols: {:?}", result.table.symbols);
    eprintln!("references: {:?}", result.table.references);
    let fact_def = result
        .table
        .symbols
        .iter()
        .find(|s| s.name == "fact")
        .expect("fact symbol");
    let refs: Vec<_> = result
        .table
        .references
        .iter()
        .filter(|r| r.target == fact_def.id)
        .collect();
    assert!(
        refs.len() >= 2,
        "expected ≥2 refs to fact (inner + outer call), got {refs:?}",
    );
}
