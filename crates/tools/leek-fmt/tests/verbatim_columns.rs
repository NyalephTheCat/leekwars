//! Raw source must not corrupt the printer's column (#198).
//!
//! `format_raw` and its siblings hand the printer a node's source
//! text with the node's own newlines still in it. Counting that whole
//! run as if it were one line left `col` tens of columns past where
//! the cursor actually stood, so every group after a multi-line raw
//! node was measured against a budget that had already been spent —
//! and broke a line that was never long. [`Doc::Verbatim`] is the
//! variant that tells the printer to re-base its column on the text
//! after the run's last `\n`.
//!
//! The mirror obligation is that none of this rewrites the raw text:
//! a `// fmt: off` region exists precisely so the formatter keeps its
//! hands off, so its bytes come back out unchanged.
//!
//! [`Doc::Verbatim`]: leek_fmt::doc::Doc::Verbatim

use leek_fmt::{FormatOptions, format_source, format_source_checked};
use leek_span::SourceId;
use leek_syntax::Version;

fn opts() -> FormatOptions {
    FormatOptions::default()
}

/// Format `src`, assert the equivalence net accepts the result and
/// that a second pass is a no-op. Returns the output.
fn fmt(src: &str) -> String {
    let out = format_source_checked(src, SourceId::new(1).unwrap(), Version::V4, &opts())
        .unwrap_or_else(|e| panic!("formatter changed the program for {src:?}: {e}"));
    let again = format_source(&out, SourceId::new(1).unwrap(), Version::V4, &opts());
    assert_eq!(out, again, "formatting is not idempotent for {src:?}");
    out
}

/// A collection literal carrying a comment the per-construct
/// formatter has nowhere to put falls back to `format_verbatim`, so
/// the array below reaches the printer as one multi-line run. The
/// method call after it is 28 columns on a line that starts at 1 —
/// nowhere near the 100-column budget — yet the printer used to
/// measure it against `col` inflated by the whole array, exploding it
/// into one argument per line.
#[test]
fn a_call_after_a_multi_line_raw_node_still_prints_flat() {
    let src = "\
var x = [
    111111111, // a comment long enough to outrun the column budget on its own
    222222222
].someMethod(aaa, bbb, ccc);
";
    let out = fmt(src);
    assert!(
        out.contains("].someMethod(aaa, bbb, ccc);"),
        "call after a multi-line raw node was broken up: {out:?}"
    );
    let over: Vec<&str> = out
        .lines()
        .filter(|l| l.chars().count() > opts().max_line_length)
        .collect();
    assert!(over.is_empty(), "lines past the budget: {over:?}");
}

/// The same column, one construct further in: an index whose base is
/// the multi-line run. Pins that the re-basing is the printer's, not
/// a property of the call syntax that happened to follow.
#[test]
fn an_index_after_a_multi_line_raw_node_still_prints_flat() {
    let src = "\
function f() {
    var y = [
        111111111, // a comment long enough to outrun the column budget on its own
        222222222
    ][someIndexCall(aaa, bbb, ccc)];
}
";
    let out = fmt(src);
    assert!(
        out.contains("][someIndexCall(aaa, bbb, ccc)];"),
        "index after a multi-line raw node was broken up: {out:?}"
    );
}

/// The mirror: re-basing the column must not touch the bytes. A
/// `// fmt: off` region is the case that would notice first, since
/// reproducing it exactly is its entire purpose.
#[test]
fn an_off_region_still_comes_back_byte_for_byte() {
    let region = "function    weird(  x,y ){\n    return x+y;\n}";
    let src = format!(
        "function ok() {{ return 1; }}\n// fmt: off\n{region}\n// fmt: on\nfunction nice() {{ return foo(1, 2, 3); }}\n"
    );
    let out = fmt(&src);
    assert!(
        out.contains(region),
        "off region was not reproduced verbatim: {out:?}"
    );
    assert!(out.contains("// fmt: off"), "`off` marker lost: {out:?}");
    assert!(out.contains("// fmt: on"), "`on` marker lost: {out:?}");
    assert!(
        out.contains("function nice() {\n    return foo(1, 2, 3);\n}"),
        "code after the off region was not formatted: {out:?}"
    );
}

/// A run that breaks its own line cannot be flattened, so a group
/// holding one breaks — the rule hard lines already follow. Without
/// it the argument separators would be joined onto one line around a
/// body that still breaks, which is the shape #199 ruled against.
#[test]
fn a_group_around_a_multi_line_raw_node_breaks_its_separators() {
    let src = "\
foo(aaa, [
    1, // c
    2
], bbb);
";
    let out = fmt(src);
    assert!(
        !out.starts_with("foo(aaa, ["),
        "group flattened around a run that breaks its own line: {out:?}"
    );
    assert!(
        out.contains("    1, // c\n    2\n]"),
        "raw run was re-laid out instead of reproduced: {out:?}"
    );
}
