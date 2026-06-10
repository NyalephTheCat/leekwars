//! L0040 `ArrayLiteralMembership` (nursery, LeekScript 4+) — flag
//! membership tests against an array literal and teach set literals:
//!
//! ```leekscript
//! if (cell in [1, 5, 9]) { … }          // linear scan
//! if (inArray([1, 5, 9], cell)) { … }   // same, builtin form
//! if (cell in <1, 5, 9>) { … }          // hash lookup, says "one of"
//! ```
//!
//! An array is an *ordered sequence*; using one for "is this value
//! one of these?" both reads wrong and scans linearly. A set literal
//! `<a, b, c>` is the v4 structure built for membership: O(1) lookup
//! and the intent is in the type.
//!
//! Gated on [`crate::LintOptions::version`] ≥ 4 — older scripts don't
//! have sets.

use leek_diagnostics::{Diagnostic, codes};
use leek_hir::{BinaryOp, Callee, Expr, ExprKind, NameRef};

use crate::LintGroup;
use crate::pass::{LintCx, LintMeta, LintPass};

pub struct ArrayLiteralMembership {
    /// Target language version; the lint is silent below 4.
    pub version: u8,
}

static META: LintMeta = LintMeta {
    name: "array-literal-membership",
    code: codes::ARRAY_LITERAL_MEMBERSHIP,
    group: LintGroup::Nursery,
    description: "membership test against an array literal — a set literal `<a, b, c>` is the structure for \"one of these\"",
};

impl LintPass for ArrayLiteralMembership {
    fn meta(&self) -> &'static LintMeta {
        &META
    }

    fn check_expr(&mut self, cx: &mut LintCx<'_, '_>, e: &Expr) {
        if self.version < 4 {
            return;
        }
        let found = match &e.kind {
            // `x in [a, b, c]` / `x not in [a, b, c]`.
            ExprKind::Binary(BinaryOp::In | BinaryOp::NotIn, _, hay) => is_multi_array(hay),
            // `inArray([a, b, c], x)`.
            ExprKind::Call(call) => {
                matches!(&call.callee, Callee::Function(NameRef::Builtin(n)) if n == "inArray")
                    && matches!(&call.args[..], [hay, _] if is_multi_array(hay))
            }
            _ => false,
        };
        if !found {
            return;
        }
        cx.emit(
            Diagnostic::new(
                codes::ARRAY_LITERAL_MEMBERSHIP,
                leek_diagnostics::Severity::Hint,
                e.span,
                "membership test against an array literal".to_string(),
            )
            .with_note(
                "a set literal says \"one of these values\" directly and checks membership by hash instead of scanning: `x in <a, b, c>`",
            ),
        );
    }
}

/// An array literal worth turning into a set (a single-element
/// literal is just an awkward `==`).
fn is_multi_array(e: &Expr) -> bool {
    matches!(&e.kind, ExprKind::Array(elems) if elems.len() >= 2)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{lint_one, lint_one_v};
    use leek_syntax::Version;

    fn run(src: &str) -> Vec<Diagnostic> {
        lint_one(ArrayLiteralMembership { version: 4 }, src)
    }

    #[test]
    fn flags_in_against_array_literal() {
        let d = run("function f(x) {\n  if (x in [1, 5, 9]) { return 1 }\n  return 0\n}\n");
        assert_eq!(d.len(), 1, "got {d:?}");
        assert!(d[0].notes[0].contains("<a, b, c>"), "{d:?}");
    }

    #[test]
    fn flags_not_in_against_array_literal() {
        let d = run("function f(x) {\n  if (x not in [1, 5]) { return 1 }\n  return 0\n}\n");
        assert_eq!(d.len(), 1, "got {d:?}");
    }

    #[test]
    fn flags_in_array_builtin_with_literal() {
        let d = run("function f(x) {\n  return inArray([1, 5, 9], x)\n}\n");
        assert_eq!(d.len(), 1, "got {d:?}");
    }

    #[test]
    fn ignores_membership_in_variable() {
        let d = run("function f(x, arr) {\n  if (x in arr) { return 1 }\n  return 0\n}\n");
        assert!(d.is_empty(), "got {d:?}");
    }

    #[test]
    fn ignores_single_element_literal() {
        let d = run("function f(x) {\n  if (x in [1]) { return 1 }\n  return 0\n}\n");
        assert!(d.is_empty(), "got {d:?}");
    }

    #[test]
    fn ignores_set_literal() {
        let d = run("function f(x) {\n  if (x in <1, 5, 9>) { return 1 }\n  return 0\n}\n");
        assert!(d.is_empty(), "got {d:?}");
    }

    #[test]
    fn silent_below_v4() {
        let d = lint_one_v(
            ArrayLiteralMembership { version: 2 },
            "function f(x) {\n  return inArray([1, 5, 9], x)\n}\n",
            Version::V2,
        );
        assert!(d.is_empty(), "got {d:?}");
    }
}
