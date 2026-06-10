//! Bitwise / shift operators on `real` operands truncate to integer and
//! yield an integer (matching the interpreter), rather than skipping.

use leek_backend_native::{NativeOptions, run};
use leek_hir::lower_file_versioned;
use leek_parser::{ast::AstNode, ast::SourceFile, parse};
use leek_span::SourceId;
use leek_syntax::{SyntaxNode, Version};

fn native(src: &str) -> String {
    let source = SourceId::new(1).unwrap();
    let parsed = parse(&format!("// @version: 4\n{src}\n"), source, Version::V4);
    let sf = SourceFile::cast(SyntaxNode::new_root(parsed.green)).expect("parse");
    let hir = lower_file_versioned(&sf, source, 4).0;
    match run(&hir, &NativeOptions::release().with_lang(4, false)) {
        Ok(v) => v.to_string(),
        Err(e) => format!("ERR: {e}"),
    }
}

#[test]
fn bitwise_on_reals_truncates_to_int() {
    // Each value was confirmed against `leekc --emit run`.
    assert_eq!(native("var r = 6.9 return r | 1"), "7");
    assert_eq!(native("var r = 5.5 return r & 3"), "1");
    assert_eq!(native("return 10.0 << 2"), "40");
    assert_eq!(native("var a = 12.0 var b = 10.0 return a ^ b"), "6");
    // The result is an integer, so it composes with integer arithmetic.
    assert_eq!(native("var r = 6.9 return (r | 1) + 1"), "8");
}
