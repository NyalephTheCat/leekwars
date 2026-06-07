//! The parser must not overflow the stack while *parsing* pathologically
//! nested input. The LSP parses untrusted buffers on every keystroke, and a
//! stack overflow aborts the process (it can't be caught by `catch_unwind`),
//! so the recursive expression/type productions are depth-guarded
//! (`Parser::enter_recursion`, cap `MAX_RECURSION_DEPTH`). Once the cap is
//! hit the parser emits a "nests too deeply" diagnostic and stops descending.
//!
//! Note: this guards parse-time *recursion* depth. A separate, pre-existing
//! concern is that a very long *flat* left-associative chain (`1-1-1-…`,
//! ~20k terms) builds a deep tree whose recursive drop can overflow — that is
//! a rowan green-tree limitation, independent of this guard, and is not
//! exercised here (the depths below stay well under that regime).

use leek_parser::parse;
use leek_span::SourceId;
use leek_syntax::Version;

fn src() -> SourceId {
    SourceId::new(1).unwrap()
}

fn has_depth_error(diags: &[leek_diagnostics::Diagnostic]) -> bool {
    diags.iter().any(|d| d.message.contains("too deeply"))
}

/// Depth comfortably past `MAX_RECURSION_DEPTH` (256) but well below the
/// flat-tree drop-overflow regime.
const DEEP: usize = 1500;

#[test]
fn deeply_nested_parens_do_not_overflow() {
    // `((((…1…))))` — recurses through `expr_bp` once per paren.
    let text = format!("return {}1{}", "(".repeat(DEEP), ")".repeat(DEEP));
    let result = parse(&text, src(), Version::LATEST);
    // Reaching here at all means we didn't blow the stack while parsing.
    assert!(
        has_depth_error(&result.diagnostics),
        "expected a depth-limit diagnostic on {DEEP}-deep parens",
    );
}

#[test]
fn deeply_nested_unary_prefix_does_not_overflow() {
    // `-----…1` — recurses through `expr_bp` once per prefix operator.
    let text = format!("return {}1", "-".repeat(DEEP));
    let result = parse(&text, src(), Version::LATEST);
    assert!(
        has_depth_error(&result.diagnostics),
        "expected a depth-limit diagnostic on {DEEP}-deep unary prefixes",
    );
}

#[test]
fn normal_nesting_still_parses_cleanly() {
    // Well within the budget — must parse without a depth error.
    let text = "return ((((((((((1 + 2))))))))))";
    let result = parse(text, src(), Version::LATEST);
    assert!(
        !has_depth_error(&result.diagnostics),
        "shallow nesting should not trip the depth guard",
    );
}
