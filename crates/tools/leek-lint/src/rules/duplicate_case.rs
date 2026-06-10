//! L0023 `DuplicateCase` — flag a `switch` with two `case` labels that
//! test the same value. The second label is dead: the first match always
//! wins, so its body can never run.
//!
//! ```leekscript
//! switch (x) {
//!   case 1: ...
//!   case 1: ...   // unreachable — same label as above
//! }
//! ```
//!
//! Only side-effect-free case expressions (literals, names, arithmetic)
//! are compared; a label that calls a function might legitimately differ
//! between evaluations, so it is left alone.

use leek_diagnostics::{Diagnostic, codes};
use leek_hir::Stmt;
use leek_span::Span;

use super::structural::{expr_key, has_side_effect};
use crate::LintGroup;
use crate::pass::{LintCx, LintMeta, LintPass};

pub struct DuplicateCase;

static META: LintMeta = LintMeta {
    name: "duplicate-case",
    code: codes::DUPLICATE_CASE,
    group: LintGroup::Correctness,
    description: "`case` label identical to an earlier one — never matches",
};

impl LintPass for DuplicateCase {
    fn meta(&self) -> &'static LintMeta {
        &META
    }

    fn check_stmt(&mut self, cx: &mut LintCx<'_, '_>, s: &Stmt) {
        let Stmt::Switch(sw) = s else { return };
        // Track the span of the first label carrying each fingerprint so
        // the diagnostic can point back at the original.
        let mut seen: Vec<(String, Span)> = Vec::new();
        for arm in &sw.arms {
            let Some(case) = &arm.case else { continue };
            if has_side_effect(case) {
                continue;
            }
            let key = expr_key(case);
            if let Some((_, first)) = seen.iter().find(|(k, _)| k == &key) {
                cx.emit(diagnostic(case.span, *first));
            } else {
                seen.push((key, case.span));
            }
        }
    }
}

fn diagnostic(span: Span, first: Span) -> Diagnostic {
    Diagnostic::warning(
        codes::DUPLICATE_CASE,
        span,
        "this `case` label is identical to an earlier one".to_string(),
    )
    .with_label(first, "first matched here")
    .with_note("the first matching label always wins, so this arm can never run — give it a distinct value")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::lint_one;

    fn run(src: &str) -> Vec<Diagnostic> {
        lint_one(DuplicateCase, src)
    }

    #[test]
    fn flags_duplicate_literal_case() {
        let d = run(
            "function f(x) {\n  switch (x) {\n    case 1: return 1\n    case 1: return 2\n  }\n  return 0\n}\n",
        );
        assert_eq!(d.len(), 1, "got {d:?}");
        assert_eq!(d[0].labels.len(), 1, "{d:?}");
    }

    #[test]
    fn ignores_distinct_cases() {
        let d = run(
            "function f(x) {\n  switch (x) {\n    case 1: return 1\n    case 2: return 2\n  }\n  return 0\n}\n",
        );
        assert!(d.is_empty(), "got {d:?}");
    }

    #[test]
    fn ignores_default_arm() {
        let d = run(
            "function f(x) {\n  switch (x) {\n    case 1: return 1\n    default: return 2\n  }\n  return 0\n}\n",
        );
        assert!(d.is_empty(), "got {d:?}");
    }
}
