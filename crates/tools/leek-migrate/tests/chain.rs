//! Chained migration tests: walk a v1 source all the way to v4
//! and verify (a) the changes happen, (b) every comment is
//! preserved byte-for-byte in the surviving text, (c) the final
//! pragma reads `@version:4`.

use leek_migrate::migrate_text;
use leek_span::SourceId;
use leek_syntax::Version;

fn id() -> SourceId {
    SourceId::new(1).unwrap()
}

#[test]
fn v1_to_v4_full_chain() {
    let src = "\
// @version:1
// power accumulator
var x = 5

// double-up each turn
x ^= 2

// pick a random offset
var roll = randFloat(0, 1)

// keep the early elements
var head = subArray([1, 2, 3, 4], 0, 2)

return x
";
    let out = migrate_text(src, id(), Version::V1, Version::V4).text;

    // Pragma now reads v4.
    assert!(out.starts_with("// @version:4\n"), "got:\n{out}");

    // The two power/rand/subArray rewrites landed. The slice's
    // end index is wrapped with `(orig) + 1` to keep the
    // inclusive→exclusive semantics intact.
    assert!(out.contains("x **= 2"), "missing **= rewrite:\n{out}");
    assert!(out.contains("randReal(0, 1)"), "missing randReal:\n{out}");
    assert!(
        out.contains("arraySlice([1, 2, 3, 4], 0, (2) + 1)"),
        "missing arraySlice rewrite:\n{out}"
    );

    // No traces of the deprecated names remain.
    assert!(!out.contains("randFloat"), "stale randFloat:\n{out}");
    assert!(!out.contains("subArray"), "stale subArray:\n{out}");
    assert!(!out.contains("^="), "stale ^=:\n{out}");

    // Every original comment survived.
    for c in [
        "// power accumulator",
        "// double-up each turn",
        "// pick a random offset",
        "// keep the early elements",
    ] {
        assert!(out.contains(c), "lost comment {c:?}:\n{out}");
    }

    // Blank lines between paragraphs are preserved.
    assert!(out.contains("\n\n"), "lost blank lines:\n{out}");
}

#[test]
fn v3_only_jump_to_v4() {
    let src = "// @version:3\nvar n = randFloat(0, 1)\n";
    let out = migrate_text(src, id(), Version::V3, Version::V4).text;
    assert!(out.starts_with("// @version:4\n"));
    assert!(out.contains("randReal(0, 1)"));
}

#[test]
fn same_version_only_normalizes_pragma() {
    // Already at v4 — the chain should be a no-op apart from
    // inserting the pragma if it was missing.
    let src = "var x = 1\n";
    let out = migrate_text(src, id(), Version::V4, Version::V4).text;
    assert!(out.starts_with("// @version:4\n"));
    assert!(out.contains("var x = 1"));
}

#[test]
fn downgrade_v4_to_v1_runs_full_chain() {
    // Downgrade chain now applies inverse rewrites:
    //   - randReal → randFloat
    //   - subArray end-index becomes inclusive (j - 1)
    //   - **= → ^= (which means power-assign in v1)
    let src = "// @version:4\nvar x = 5\nx **= 2\nvar n = randReal(0, 1)\n\
               var head = arraySlice([1, 2, 3, 4], 0, 3)\nreturn [x, n, head]\n";
    let out = migrate_text(src, id(), Version::V4, Version::V1).text;
    assert!(out.starts_with("// @version:1\n"), "pragma: {out}");
    assert!(
        out.contains("x ^= 2"),
        "power-assign downgrade missing: {out}"
    );
    assert!(out.contains("randFloat(0, 1)"), "randFloat missing: {out}");
    assert!(
        out.contains("subArray([1, 2, 3, 4], 0, (3) - 1)"),
        "subArray rewrite missing: {out}",
    );
}

#[test]
fn preserves_indentation_inside_function() {
    let src = "\
// @version:1
function step(integer n) {
    var result = n
    // square it up
    result ^= 2
    return result
}
";
    let out = migrate_text(src, id(), Version::V1, Version::V4).text;
    // The function body's 4-space indent stays intact, only the
    // operator on that line changes.
    assert!(
        out.contains("    result **= 2"),
        "expected `    result **= 2` line, got:\n{out}"
    );
    assert!(out.contains("    // square it up"));
}
