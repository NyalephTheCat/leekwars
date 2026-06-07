//! End-to-end interpreter tests focused on lambda behaviour.

use leek_backend_interp::{RunResult, Value, run_with_limit_version};
use leek_diagnostics::Severity;
use leek_hir::lower_file;
use leek_parser::{ast::AstNode, ast::SourceFile, parse};
use leek_span::SourceId;
use leek_syntax::{SyntaxNode, Version};

fn run(src: &str, version: Version) -> RunResult {
    let source = SourceId::new(1).unwrap();
    let parse_result = parse(src, source, version);
    assert!(
        !parse_result
            .diagnostics
            .iter()
            .any(|d| d.severity == Severity::Error),
        "parse errors: {:?}",
        parse_result.diagnostics,
    );
    let root = SyntaxNode::new_root(parse_result.green.clone());
    let file = SourceFile::cast(root).expect("source file");
    let (hir, _diags) = lower_file(&file, source);
    let v = match version {
        Version::V1 => 1,
        Version::V2 => 2,
        Version::V3 => 3,
        Version::V4 => 4,
    };
    run_with_limit_version(&hir, 1_000_000, v)
}

#[test]
fn recursive_lambda_factorial() {
    let r = run(
        "var fact = function(x) { if (x == 1) { return 1 } else { return fact(x - 1) * x } } return fact(8);",
        Version::V4,
    );
    assert!(r.error.is_none(), "unexpected error: {:?}", r.error);
    assert!(
        matches!(r.value, Value::Int(40320)),
        "expected 40320, got {:?}",
        r.value
    );
}

#[test]
fn nested_lambda_captures_propagate() {
    let r = run(
        "function outer(x) { return function(y) { return function(z) { return x + y + z } } } return outer(1)(2)(3)",
        Version::V4,
    );
    assert!(r.error.is_none(), "unexpected error: {:?}", r.error);
    assert!(
        matches!(r.value, Value::Int(6)),
        "expected 6, got {:?}",
        r.value
    );
}
