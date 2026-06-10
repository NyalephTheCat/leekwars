//! End-to-end execution of experimental `enum` declarations: the
//! class-with-integer-statics desugar must run on the native backend
//! with no enum-specific backend support.

use leek_backend_native::{NativeOptions, run};
use leek_parser::{ParseFeatures, ast::AstNode, ast::SourceFile, parse_with_features};
use leek_span::SourceId;
use leek_syntax::{SyntaxNode, Version};

fn jit_enums(src: &str) -> String {
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
    let file = SourceFile::cast(SyntaxNode::new_root(parsed.green)).expect("parse");
    let flags = leek_span::FeatureFlags {
        enums: true,
        ..Default::default()
    };
    let (hir, _d) = leek_hir::lower::lower_file_versioned_with_flags(&file, source, 4, flags);
    match run(&hir, &NativeOptions::debug()) {
        Ok(v) => v.to_string(),
        Err(e) => format!("ERR: {e}"),
    }
}

#[test]
fn enum_variants_execute_as_integers() {
    assert_eq!(
        jit_enums("enum Color { RED, GREEN, BLUE = 10 }\nreturn Color.GREEN + Color.BLUE\n"),
        "11"
    );
}

#[test]
fn enum_variant_in_condition() {
    assert_eq!(
        jit_enums(
            "enum State { IDLE, RUNNING, DONE = 99 }\n\
             var s = State.RUNNING\n\
             if (s == State.RUNNING) { return State.DONE }\n\
             return State.IDLE\n"
        ),
        "99"
    );
}
