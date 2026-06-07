//! Quick end-to-end smoke test — not part of the v0.1 fixture
//! suite, just enough to confirm the formatter runs.

use leek_fmt::{FormatOptions, format_source};
use leek_span::SourceId;
use leek_syntax::Version;

fn fmt(src: &str) -> String {
    format_source(
        src,
        SourceId::new(1).unwrap(),
        Version::V4,
        &FormatOptions::default(),
    )
}

#[test]
fn formats_hello() {
    let out = fmt("var x = 1;\nreturn x;\n");
    assert!(out.contains("var x = 1"));
    assert!(out.contains("return x"));
}

#[test]
fn idempotent_on_hello() {
    let src = "var x = 1;\nreturn x;\n";
    let once = fmt(src);
    let twice = fmt(&once);
    assert_eq!(once, twice, "format should be idempotent");
}

#[test]
fn preserves_doc_comment_above_function() {
    let src = "/// docs for foo\nfunction foo() { return 1; }\n";
    let out = fmt(src);
    assert!(
        out.contains("/// docs for foo"),
        "doc comment lost: {out:?}"
    );
    assert!(out.contains("function foo"));
    // The doc comment must sit immediately before the function (no
    // blank line between them).
    let doc_idx = out.find("///").unwrap();
    let fn_idx = out.find("function").unwrap();
    let between = &out[doc_idx..fn_idx];
    assert_eq!(
        between.matches('\n').count(),
        1,
        "doc comment should be tight against function (one newline). got: {between:?}"
    );
}

#[test]
fn preserves_block_doc_comment() {
    let src = "/**\n * Multi-line doc.\n * @param x foo\n */\nfunction foo(x) { return x; }\n";
    let out = fmt(src);
    assert!(out.contains("/**"), "block doc missing: {out:?}");
    assert!(out.contains("* Multi-line doc"));
    assert!(out.contains("* @param x foo"));
}
