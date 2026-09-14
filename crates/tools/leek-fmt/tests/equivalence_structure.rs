//! The safety net must see *structure*, not just tokens.
//!
//! The formatter is allowed to add and remove `;`, `,`, `(` / `)` and
//! `{` / `}`, so those tokens can't be compared directly — which leaves
//! a token-only check blind to exactly the edits that change what a
//! program means. These tests pin both halves: the meaning-changing
//! rewrites are rejected, and the legal ones still pass under every
//! option set that restructures the tree.

use leek_fmt::{
    ControlBraces, EquivalenceError, FormatOptions, QuoteStyle, Semicolons, TrailingComma,
    check_equivalence, format_source_checked,
};
use leek_span::SourceId;
use leek_syntax::Version;

fn check(before: &str, after: &str) -> Result<(), EquivalenceError> {
    check_equivalence(before, after, Version::V4)
}

/// Default options plus the two option sets that exercise every
/// tree-restructuring rewrite (braces, parens, else-if collapsing,
/// semicolons, commas, quotes).
fn option_sets() -> Vec<(&'static str, FormatOptions)> {
    let rewrite_heavy = FormatOptions {
        control_braces: ControlBraces::Always,
        semicolons: Semicolons::Always,
        trailing_comma: TrailingComma::Always,
        quote_style: QuoteStyle::Single,
        remove_redundant_parens: true,
        ..FormatOptions::default()
    };
    let compacting = FormatOptions {
        control_braces: ControlBraces::Never,
        trailing_comma: TrailingComma::Never,
        quote_style: QuoteStyle::Double,
        collapse_else_if: true,
        remove_redundant_parens: true,
        ..FormatOptions::default()
    };
    vec![
        ("default", FormatOptions::default()),
        ("rewrite-heavy", rewrite_heavy),
        ("compacting", compacting),
    ]
}

#[test]
fn meaning_changing_rewrites_are_rejected() {
    // Each pair has the same significant tokens once layout punctuation
    // is dropped, so only the tree tells them apart.
    let cases = [
        // A needed paren peeled off: precedence changes.
        ("var x = (a + b) * c;\n", "var x = a + b * c;\n"),
        // A brace dropped: `b()` leaves the `if` body.
        ("if (x) { a(); b(); }\n", "if (x) a();\nb();\n"),
        // A brace added: `b()` joins it.
        ("if (x) a();\nb();\n", "if (x) { a(); b(); }\n"),
        // A semicolon dropped: `1 (b)` becomes a call.
        ("var a = 1; (b)();\n", "var a = 1 (b)();\n"),
        // An `else` re-parented onto the inner `if`.
        (
            "if (a) { if (b) { x(); } } else { y(); }\n",
            "if (a) if (b) { x(); } else { y(); }\n",
        ),
    ];
    let mut survivors = Vec::new();
    for (before, after) in cases {
        match check(before, after) {
            Err(
                EquivalenceError::ShapeMismatch { .. } | EquivalenceError::ParseRegression { .. },
            ) => {}
            other => survivors.push(format!("{before:?} -> {after:?}: {other:?}")),
        }
    }
    assert!(
        survivors.is_empty(),
        "{} meaning-changing rewrite(s) passed the net:\n{}",
        survivors.len(),
        survivors.join("\n")
    );
}

#[test]
fn legal_rewrites_still_pass() {
    // The rewrites the formatter is allowed to make, spelled out by
    // hand so a normalization that goes missing fails here rather than
    // making `miku fmt` refuse correct files.
    let cases = [
        // control_braces, both directions
        ("if (x) a();\n", "if (x) {\n    a();\n}\n"),
        ("while (x) {\n    a();\n}\n", "while (x) a();\n"),
        (
            "for (var i = 0; i < 3; i++) a();\n",
            "for (var i = 0; i < 3; i++) {\n    a();\n}\n",
        ),
        (
            "for (var v in c) a();\n",
            "for (var v in c) {\n    a();\n}\n",
        ),
        // collapse_else_if
        (
            "if (x) { a(); } else { if (y) { b(); } }\n",
            "if (x) {\n    a();\n} else if (y) {\n    b();\n}\n",
        ),
        // remove_redundant_parens
        ("var x = ((a));\n", "var x = a;\n"),
        ("if ((x)) { a(); }\n", "if (x) {\n    a();\n}\n"),
        ("return (f(1));\n", "return f(1);\n"),
        // semicolons / trailing commas / quotes
        ("var x = 1\n", "var x = 1;\n"),
        (
            "var x = [\n    a,\n    b\n];\n",
            "var x = [\n    a,\n    b,\n];\n",
        ),
        ("var s = 'hi';\n", "var s = \"hi\";\n"),
    ];
    for (before, after) in cases {
        check(before, after)
            .unwrap_or_else(|e| panic!("legal rewrite rejected: {before:?} -> {after:?}: {e}"));
    }
}

#[test]
fn structural_constructs_survive_every_option_set() {
    // The false-positive guard: real formatting of nested control flow,
    // classes and expression trees must pass the net under every option
    // set, and stay passing on the second run.
    let snippets = [
        "if (a) if (b) x(); else y();\n",
        "if (a) { if (b) { x(); } } else { y(); }\n",
        "while (a) { if (b) continue; else break; }\n",
        "do { x(); } while (a);\n",
        "for (var i = 0; i < 10; i++) if (i % 2 == 0) push(r, i);\n",
        "for (var v in [1, 2, 3]) debug(v);\n",
        "var x = ((a + b)) * (c - d) / (e % f);\n",
        "var y = (a ? (b) : (c))[0].field(1, 2);\n",
        "class A { private x = 1; public m(a, b) { return (a + b) * 2; } }\n",
        "function f(a, b = (1 + 2)) { return a ? b : (a); }\n",
        "var g = (a, b) -> { if (a) return b; return (a); };\n",
        "// fmt: off\nvar   weird=1\n// fmt: on\nvar ok = 2;\n",
        "// fmt-skip\nvar   skipped = 1;\nvar ok = 2;\n",
    ];
    let mut failures = Vec::new();
    for (label, opts) in option_sets() {
        for src in snippets {
            let once =
                match format_source_checked(src, SourceId::new(1).unwrap(), Version::V4, &opts) {
                    Ok(text) => text,
                    Err(e) => {
                        failures.push(format!("{label}: {src:?}: {e}"));
                        continue;
                    }
                };
            match format_source_checked(&once, SourceId::new(1).unwrap(), Version::V4, &opts) {
                Ok(twice) if twice == once => {}
                Ok(twice) => failures.push(format!(
                    "{label}: {src:?} not idempotent\n--- once ---\n{once}--- twice ---\n{twice}"
                )),
                Err(e) => failures.push(format!("{label}: {src:?} on the second run: {e}")),
            }
        }
    }
    assert!(
        failures.is_empty(),
        "{} case(s) failed:\n\n{}",
        failures.len(),
        failures.join("\n\n")
    );
}
