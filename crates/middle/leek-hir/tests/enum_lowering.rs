//! Experimental `enum` declarations lower to a class with static
//! final integer fields — the variants must carry the right values
//! (auto-increment from 0, explicit `= n` resets the counter, a
//! negative value is honoured).

use leek_diagnostics::Severity;
use leek_parser::{ParseFeatures, ast::AstNode, ast::SourceFile, parse_with_features};
use leek_span::SourceId;
use leek_syntax::{SyntaxNode, Version};

fn lower(src: &str) -> leek_hir::HirFile {
    let source = SourceId::new(1).unwrap();
    let parsed = parse_with_features(
        src,
        source,
        Version::V4,
        ParseFeatures {
            enums: true,
            ..Default::default()
        },
    );
    let parse_errs: Vec<_> = parsed
        .diagnostics
        .iter()
        .filter(|d| d.severity == Severity::Error)
        .collect();
    assert!(parse_errs.is_empty(), "parse errors: {parse_errs:?}");

    let ast = SourceFile::cast(SyntaxNode::new_root(parsed.green)).expect("source file");
    let flags = leek_span::FeatureFlags {
        enums: true,
        ..Default::default()
    };
    let (hir, diags) = leek_hir::lower::lower_file_versioned_with_flags(&ast, source, 4, flags);
    let lower_errs: Vec<_> = diags
        .iter()
        .filter(|d| d.severity == Severity::Error)
        .collect();
    assert!(lower_errs.is_empty(), "lowering errors: {lower_errs:?}");
    hir
}

/// The variants of the single lowered enum class, as `(name, value)`.
fn variants(hir: &leek_hir::HirFile, class: &str) -> Vec<(String, i64)> {
    let cls = hir
        .defs
        .iter()
        .find_map(|d| match d {
            leek_hir::Def::Class(c) if c.name == class => Some(c),
            _ => None,
        })
        .unwrap_or_else(|| panic!("enum `{class}` should lower to a class"));
    cls.fields
        .iter()
        .map(|f| {
            assert!(f.is_static, "{}.{} must be static", class, f.name);
            assert!(f.is_final, "{}.{} must be final", class, f.name);
            assert_eq!(
                f.ty,
                Some(leek_hir::Type::Integer),
                "{}.{} must be integer-typed",
                class,
                f.name
            );
            let Some(init) = &f.init else {
                panic!("{}.{} must have an initializer", class, f.name);
            };
            let leek_hir::ExprKind::Literal(leek_hir::Literal::Int(v)) = &init.kind else {
                panic!("{}.{} initializer must be an int literal", class, f.name);
            };
            (f.name.clone(), *v)
        })
        .collect()
}

#[test]
fn enum_lowers_to_class_with_integer_statics() {
    let hir = lower("enum Color { RED, GREEN, BLUE = 10 }\n");
    assert_eq!(
        variants(&hir, "Color"),
        [
            ("RED".to_string(), 0),
            ("GREEN".to_string(), 1),
            ("BLUE".to_string(), 10),
        ]
    );
}

#[test]
fn enum_auto_increment_continues_after_explicit_value() {
    let hir = lower("enum Status { OK = 200, CREATED, NOT_FOUND = 404, GONE }\n");
    assert_eq!(
        variants(&hir, "Status"),
        [
            ("OK".to_string(), 200),
            ("CREATED".to_string(), 201),
            ("NOT_FOUND".to_string(), 404),
            ("GONE".to_string(), 405),
        ]
    );
}

#[test]
fn enum_negative_value_is_honoured() {
    let hir = lower("enum Temp { FREEZING = -10, NEXT }\n");
    assert_eq!(
        variants(&hir, "Temp"),
        [("FREEZING".to_string(), -10), ("NEXT".to_string(), -9)]
    );
}
