//! Targeted regression test for `Real.MIN_VALUE` formatting.
//!
//! Java's `Double.toString(Double.MIN_VALUE)` returns `"4.9E-324"`
//! (Steele-White algorithm). Rust's default formatter (ryu)
//! returns `"5E-324"`. Both round-trip to the same `f64`, but
//! upstream corpus tests compare strings.

use leek_backend_interp::run_with_limit_version;
use leek_diagnostics::Severity;
use leek_hir::lower_file;
use leek_parser::{ast::AstNode, ast::SourceFile, parse};
use leek_span::SourceId;
use leek_syntax::{SyntaxNode, Version};

fn run(src: &str) -> String {
    let source = SourceId::new(1).unwrap();
    let parsed = parse(src, source, Version::V4);
    assert!(
        !parsed
            .diagnostics
            .iter()
            .any(|d| d.severity == Severity::Error),
        "parse errors: {:?}",
        parsed.diagnostics,
    );
    let root = SyntaxNode::new_root(parsed.green.clone());
    let file = SourceFile::cast(root).expect("source file");
    let (hir, _) = lower_file(&file, source);
    let r = run_with_limit_version(&hir, 1_000_000, 4);
    assert!(r.error.is_none(), "{:?}", r.error);
    r.value.to_string()
}

#[test]
fn real_min_value_renders_like_java() {
    assert_eq!(run("return Real.MIN_VALUE"), "4.9E-324");
}

#[test]
fn negated_real_min_value_renders_like_java() {
    assert_eq!(run("return -Real.MIN_VALUE"), "-4.9E-324");
}

#[test]
fn real_max_value_still_renders_normally() {
    // Not a subnormal — should follow the standard E-form path.
    let s = run("return Real.MAX_VALUE");
    assert!(s.contains("1.7976931348623157E308"), "got: {s}");
}
