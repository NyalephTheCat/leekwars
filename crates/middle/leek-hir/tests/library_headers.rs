//! The generated library signature headers must parse (with bodiless
//! function signatures) and lower cleanly — a guard that the generator's
//! type mapping only emits valid Leekscript.

use leek_diagnostics::Severity;
use leek_parser::{ParseFeatures, ast::AstNode, ast::SourceFile, parse_with_features};
use leek_span::SourceId;
use leek_syntax::{SyntaxNode, Version};

fn check(src: &str, min_fns: usize) {
    let source = SourceId::new(1).unwrap();
    let parsed = parse_with_features(
        src,
        source,
        Version::V4,
        ParseFeatures {
            function_signatures: true,
            generics: true,
        },
    );
    let parse_errs: Vec<_> = parsed
        .diagnostics
        .iter()
        .filter(|d| d.severity == Severity::Error)
        .collect();
    assert!(parse_errs.is_empty(), "header parse errors: {parse_errs:?}");

    let ast = SourceFile::cast(SyntaxNode::new_root(parsed.green)).expect("source file");
    let (hir, diags) = leek_hir::lower_file(&ast, source);
    let lower_errs: Vec<_> = diags
        .iter()
        .filter(|d| d.severity == Severity::Error)
        .collect();
    assert!(
        lower_errs.is_empty(),
        "header lowering errors: {lower_errs:?}"
    );

    let n = hir
        .defs
        .iter()
        .filter(|d| matches!(d, leek_hir::Def::Function(_)))
        .count();
    assert!(n >= min_fns, "expected >= {min_fns} functions, got {n}");
}

#[test]
fn leekwars_header_parses_and_lowers() {
    check(leek_prelude::LEEKWARS_SRC, 200);
}

#[test]
fn stdlib_header_parses_and_lowers() {
    check(leek_prelude::STDLIB_SRC, 100);
}
