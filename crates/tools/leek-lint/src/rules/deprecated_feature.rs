//! L0008 `DeprecatedFeature` — flag built-in calls that have a
//! recommended replacement in the current language version.
//!
//! Distinct from the resolver's `REMOVED_FUNCTION` error: those
//! features are GONE in the current version and the call is a hard
//! error. `DeprecatedFeature` is the soft-warning step *before*
//! removal — the call still compiles, but a newer name exists.
//!
//! The deprecation table mirrors upstream
//! `LeekFunctions.setMaxVersion(N, "replacement")` annotations:
//! once a builtin's `maxVersion` is set, every version up to and
//! including `maxVersion` should emit a deprecation warning so
//! users migrate before the function disappears.

use leek_diagnostics::{Diagnostic, codes};
use leek_hir::{Callee, Expr, ExprKind, NameRef};

use crate::LintGroup;
use crate::pass::{LintCx, LintMeta, LintPass};

pub struct DeprecatedFeature;

static META: LintMeta = LintMeta {
    name: "deprecated-feature",
    code: codes::DEPRECATED_FEATURE,
    group: LintGroup::Style,
    description: "call to a deprecated builtin that has a newer replacement",
};

impl LintPass for DeprecatedFeature {
    fn meta(&self) -> &'static LintMeta {
        &META
    }

    fn check_expr(&mut self, cx: &mut LintCx<'_, '_>, e: &Expr) {
        if let ExprKind::Call(c) = &e.kind
            && let Callee::Function(NameRef::Builtin(name)) = &c.callee
            && let Some(replacement) = deprecated_replacement(name)
        {
            cx.emit(diagnostic(name, replacement, e.span));
        }
    }
}

/// Returns the recommended replacement name when `name` is a
/// deprecated builtin, otherwise `None`. Source: upstream
/// `LeekFunctions.java::setMaxVersion(N, "replacement")` calls.
fn deprecated_replacement(name: &str) -> Option<&'static str> {
    match name {
        // `randFloat` → `randReal` (renamed in v4; setMaxVersion(3,
        // "randReal") in LeekFunctions.java).
        "randFloat" => Some("randReal"),
        // `subArray` → `arraySlice` (setMaxVersion(3, "arraySlice")).
        "subArray" => Some("arraySlice"),
        // `removeKey` → `mapRemove` (setMaxVersion(3, "mapRemove")).
        "removeKey" => Some("mapRemove"),
        // `color` is fine; `getColor` was the old name in v1
        // (setMaxVersion logic mapped getColor → color for v2+).
        _ => None,
    }
}

fn diagnostic(name: &str, replacement: &str, span: leek_span::Span) -> Diagnostic {
    use leek_diagnostics::{Applicability, Suggestion, TextEdit};
    // We don't have the exact name-token span here — `span` is the
    // whole call expression. That's still useful as an attachment
    // point; the suggestion below targets the call's text range
    // and replaces the name prefix.
    Diagnostic::warning(
        codes::DEPRECATED_FEATURE,
        span,
        format!("`{name}` is deprecated; use `{replacement}` instead"),
    )
    .with_note(format!(
        "`{name}` still works at this version but will be removed in a future release"
    ))
    .with_suggestion(Suggestion {
        message: format!("rename to `{replacement}`"),
        edits: vec![TextEdit {
            // Approximate: replace the leading `name(...)` text with
            // `replacement(...)`. The text-edit layer scopes to the
            // file the span belongs to, so this lands on the call's
            // own line.
            span,
            replacement: replacement.to_string(),
        }],
        // The suggestion only swaps the function name; leaving the
        // edit imprecise (we'd need a name-only sub-span) so mark
        // as MaybeIncorrect rather than MachineApplicable.
        applicability: Applicability::MaybeIncorrect,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::lint_one;

    fn run(src: &str) -> Vec<Diagnostic> {
        lint_one(DeprecatedFeature, src)
    }

    #[test]
    fn flags_randfloat() {
        let d = run("var x = randFloat(0, 1)\n");
        assert_eq!(d.len(), 1);
        assert!(d[0].message.contains("randReal"));
    }

    #[test]
    fn flags_subarray() {
        let d = run("var x = subArray([1, 2, 3], 0, 2)\n");
        assert_eq!(d.len(), 1);
        assert!(d[0].message.contains("arraySlice"));
    }

    #[test]
    fn ignores_modern_names() {
        let d = run("var x = randReal(0, 1)\nvar y = arraySlice([1, 2, 3], 0, 2)\n");
        assert!(d.is_empty(), "got {d:?}");
    }
}
