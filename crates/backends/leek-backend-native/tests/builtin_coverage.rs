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

/// The names the runtime dispatches under two spellings, plus the two it
/// answers with null. Each of these used to fail the *whole program* with
/// `unsupported: builtin X` (#188); `tests/builtin_dispatch_parity.rs` stops
/// the list from drifting again, and this checks the values that come out.
#[test]
fn alias_named_builtins_run_and_match_their_twin() {
    let cases: &[(&str, &str)] = &[
        ("return stringRepeat(\"ab\", 3)", "\"ababab\""),
        ("return stringCharCodeAt(\"abc\", 1)", "98"),
        ("return getDate()", "0"),
        ("return getTime()", "0"),
        ("return print(\"x\")", "null"),
        ("return println(\"x\")", "null"),
        (
            "var s = arrayToSet([1, 2, 3])\nvar out = []\nsetForEach(s, function(v) { push(out, v) })\nreturn count(out)",
            "3",
        ),
    ];
    for (src, expected) in cases {
        let got = native(&format!("// @version: 4\n{src}\n"));
        assert_eq!(
            &got, expected,
            "native `{src}` = {got:?}, expected {expected:?}"
        );
    }

    // The values above are measured here, not sourced from upstream, so the
    // load-bearing assertion is this one: the two spellings of one runtime
    // operation have to produce the same thing, whatever it is.
    for (a, b) in [
        ("repeat(\"ab\", 3)", "stringRepeat(\"ab\", 3)"),
        ("charCodeAt(\"abc\", 1)", "stringCharCodeAt(\"abc\", 1)"),
        ("getDate()", "getTime()"),
        ("getOperations()", "getInstructionsCount()"),
    ] {
        let left = native(&format!("// @version: 4\nreturn {a}\n"));
        let right = native(&format!("// @version: 4\nreturn {b}\n"));
        assert_eq!(left, right, "`{a}` and `{b}` are the same operation");
    }
}
