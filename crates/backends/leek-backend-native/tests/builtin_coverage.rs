//! Coverage guard for builtins newly routed through the generic
//! `leek_runtime::call_builtin` path (see `is_generic_builtin`). Each value
//! here was confirmed to match the interpreter (`leekc --emit run`); these
//! assertions lock that in so a future change can't silently regress a
//! builtin back to `Unsupported` or — worse — a wrong value.

use leek_backend_native::{NativeOptions, run};
use leek_hir::lower_file_versioned;
use leek_parser::{ast::AstNode, ast::SourceFile, parse};
use leek_span::SourceId;
use leek_syntax::{SyntaxNode, Version};

fn native(src: &str) -> String {
    let source = SourceId::new(1).unwrap();
    let parsed = parse(src, source, Version::V4);
    let sf = SourceFile::cast(SyntaxNode::new_root(parsed.green)).expect("parse");
    let hir = lower_file_versioned(&sf, source, 4).0;
    let opts = NativeOptions::release().with_lang(4, false);
    match run(&hir, &opts) {
        Ok(v) => v.to_string(),
        Err(e) => format!("ERR: {e}"),
    }
}

#[test]
fn newly_supported_builtins_match_interpreter() {
    let cases: &[(&str, &str)] = &[
        ("return arraySize([3, 1, 2, 1])", "4"),
        ("return distinct([1, 1, 2, 3, 3])", "[1, 2, 3]"),
        ("return range(2, 5)", "[2, 3, 4, 5]"),
        ("return getRed(0xFF8040)", "255"),
        ("return getGreen(0xFF8040)", "128"),
        ("return getBlue(0xFF8040)", "64"),
        ("return hash(\"abc\")", "210631466959"),
        ("return hashCode(\"abc\")", "210631466959"),
        // `shuffle` is deterministic in the runtime (1-arg = identity).
        ("return shuffle([7, 8, 9])", "[7, 8, 9]"),
    ];
    for (src, expected) in cases {
        let got = native(&format!("// @version: 4\n{src}\n"));
        assert_eq!(
            &got, expected,
            "native `{src}` = {got:?}, expected {expected:?}"
        );
    }
}
