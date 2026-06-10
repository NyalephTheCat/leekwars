//! L0031 `ApproxConstant` (pedantic) — flag a real literal that spells
//! out an approximation of a well-known constant:
//!
//! ```leekscript
//! var area = 3.14159 * r * r    // → PI * r * r
//! ```
//!
//! The builtin constant is both more precise and self-documenting.
//! Detection follows clippy's `approx_constant`: the literal's
//! (shortest) decimal text must be a prefix of the constant's digits,
//! at least [`MIN_DIGITS`] characters long — so `3.1` stays quiet but
//! `3.14`, `3.1416`, and a fully spelled-out value all fire.

use leek_diagnostics::{Diagnostic, codes};
use leek_hir::{Expr, ExprKind, Literal};

use crate::LintGroup;
use crate::pass::{LintCx, LintMeta, LintPass};

pub struct ApproxConstant;

/// `(digits, builtin name)` for each known constant. Digits as text so
/// prefix matching is exact (no float-rounding surprises).
const KNOWN: &[(&str, &str)] = &[("3.141592653589793", "PI"), ("2.718281828459045", "E")];

/// Shortest literal text that counts as "trying to write the constant"
/// (counting the `x.` prefix: `3.14` is four chars).
const MIN_DIGITS: usize = 4;

static META: LintMeta = LintMeta {
    name: "approx-constant",
    code: codes::APPROX_CONSTANT,
    group: LintGroup::Pedantic,
    description: "real literal approximating a known constant — use the builtin (`PI`, `E`)",
};

impl LintPass for ApproxConstant {
    fn meta(&self) -> &'static LintMeta {
        &META
    }

    fn check_expr(&mut self, cx: &mut LintCx<'_, '_>, e: &Expr) {
        let ExprKind::Literal(Literal::Real(r)) = &e.kind else {
            return;
        };
        // Shortest round-trip text of the value the user wrote. (HIR
        // doesn't keep the source text, but the shortest repr of the
        // parsed double is exactly the digits that matter.)
        let text = format!("{r}");
        for (digits, name) in KNOWN {
            if text.len() >= MIN_DIGITS && digits.starts_with(&text) {
                cx.emit(
                    Diagnostic::new(
                        codes::APPROX_CONSTANT,
                        leek_diagnostics::Severity::Hint,
                        e.span,
                        format!("`{text}` looks like an approximation of `{name}`"),
                    )
                    .with_note(format!(
                        "use the builtin constant `{name}` — it is more precise and says what you mean"
                    )),
                );
                return;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::lint_one;

    fn run(src: &str) -> Vec<Diagnostic> {
        lint_one(ApproxConstant, src)
    }

    #[test]
    fn flags_short_pi() {
        let d = run("function f(r) {\n  return 3.14 * r\n}\n");
        assert_eq!(d.len(), 1, "got {d:?}");
        assert!(d[0].message.contains("PI"), "{d:?}");
    }

    #[test]
    fn flags_longer_pi() {
        let d = run("function f(r) {\n  return 3.14159265 * r\n}\n");
        assert_eq!(d.len(), 1, "got {d:?}");
    }

    #[test]
    fn flags_e() {
        let d = run("function f(x) {\n  return 2.71828 * x\n}\n");
        assert_eq!(d.len(), 1, "got {d:?}");
        assert!(d[0].message.contains('E'), "{d:?}");
    }

    #[test]
    fn ignores_too_short_prefix() {
        // `3.1` could be anything — too short to assume PI was meant.
        let d = run("function f(r) {\n  return 3.1 * r\n}\n");
        assert!(d.is_empty(), "got {d:?}");
    }

    #[test]
    fn ignores_unrelated_real() {
        let d = run("function f(r) {\n  return 3.15 * r\n}\n");
        assert!(d.is_empty(), "got {d:?}");
    }
}
