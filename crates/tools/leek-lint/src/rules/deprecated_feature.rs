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

use leek_diagnostics::{Diagnostic, codes, diag};
use leek_hir::{Call, Callee, Expr, ExprKind, NameRef};
use leek_span::Span;

use crate::registry::declare_lint;
use crate::pass::{LintCx, LintMeta, LintPass};

#[derive(Default)]
pub struct DeprecatedFeature;

declare_lint!(
    DeprecatedFeature,
    "deprecated-feature",
    codes::DEPRECATED_FEATURE,
    Style,
    "call to a deprecated builtin that has a newer replacement"
);

impl LintPass for DeprecatedFeature {
    fn meta(&self) -> &'static LintMeta {
        &META
    }

    fn check_expr(&mut self, cx: &mut LintCx<'_, '_>, e: &Expr) {
        if let ExprKind::Call(c) = &e.kind
            && let Callee::Function(NameRef::Builtin(name) | NameRef::Unresolved(name)) = &c.callee
            && let Some(replacement) = deprecated_replacement(name)
        {
            cx.emit(diagnostic(name, replacement, e.span, c));
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

fn diagnostic(name: &str, replacement: &str, span: Span, call: &Call) -> Diagnostic {
    let d = diag!(
        codes::DEPRECATED_FEATURE,
        span,
        "`{name}` is deprecated; use `{replacement}` instead"
    )
    .with_note(format!(
        "`{name}` still works at this version but will be removed in a future release"
    ));
    match rename_fix(name, replacement, call) {
        Some(fix) => d.with_suggestion(fix),
        // A `subArray` call whose arity we don't recognise: renaming
        // alone would silently drop the last element, and we can't
        // tell which argument is the end index. Same call
        // `leek-migrate`'s v3→v4 pass makes — flag it, fix nothing.
        None if name == "subArray" => d.with_note(format!(
            "no automatic fix here — `{replacement}`'s end index is exclusive, \
             so this call needs rewriting by hand"
        )),
        // Only reachable when the callee span isn't the name token,
        // i.e. the file has a parse error. Nothing safe to offer.
        None => d,
    }
}

/// The quick fix for one deprecated call, or `None` when no faithful
/// edit exists.
///
/// The rename edit targets [`Call::callee_span`] — the name token
/// alone. Using the call expression's span would replace the argument
/// list too, turning `randFloat(0, 1)` into a bare `randReal`.
fn rename_fix(name: &str, replacement: &str, call: &Call) -> Option<leek_diagnostics::Suggestion> {
    use leek_diagnostics::{Applicability, Suggestion, TextEdit};

    let callee = call.callee_span;
    // Guard against a callee span that isn't the name token — a
    // parse error makes `lower_call` fall back to the whole call.
    // Replacing that range would delete the arguments.
    if callee.start < call.span.start
        || callee.end > call.span.end
        || (callee.end - callee.start) as usize != name.len()
    {
        return None;
    }
    let mut edits = vec![TextEdit {
        span: callee,
        replacement: replacement.to_string(),
    }];

    // `subArray(a, i, j)`'s end index is inclusive; `arraySlice`'s is
    // exclusive. Bump the third argument so the rename keeps the same
    // elements — the rewrite `leek-migrate`'s v3→v4 pass performs.
    let applicability = if name == "subArray" {
        let [_, _, end] = call.args.as_slice() else {
            return None;
        };
        let src = end.span.source;
        edits.push(TextEdit {
            span: Span::new(src, end.span.start, end.span.start),
            replacement: "(".to_string(),
        });
        edits.push(TextEdit {
            span: Span::new(src, end.span.end, end.span.end),
            replacement: ") + 1".to_string(),
        });
        // Faithful, but it reshapes an argument the author wrote —
        // worth a glance, and kept out of `source.fixAll`.
        Applicability::MaybeIncorrect
    } else {
        Applicability::MachineApplicable
    };

    Some(Suggestion {
        message: format!("rename to `{replacement}`"),
        edits,
        applicability,
    })
}

#[cfg(test)]
mod tests {
    use leek_diagnostics::Applicability;
    use leek_rewrite::EditSet;

    use super::*;
    use crate::testing::{assert_suggestions_fix, lint_one};

    fn run(src: &str) -> Vec<Diagnostic> {
        lint_one(DeprecatedFeature, src)
    }

    /// Apply the single suggestion on the single finding.
    fn fix(src: &str) -> String {
        let d = run(src);
        assert_eq!(d.len(), 1, "expected one finding, got {d:?}");
        let sug = d[0].suggestions.first().expect("a suggestion");
        let mut edits = EditSet::new(src.len());
        edits.push_suggestion(sug).expect("valid edits");
        edits.apply(src).expect("edits apply to their own source")
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

    /// Regression: the rename used to target the whole call
    /// expression, so applying it turned `randFloat(0, 1)` into a
    /// bare `randReal` and the argument list was lost.
    #[test]
    fn rename_keeps_the_argument_list() {
        assert_eq!(fix("var x = randFloat(0, 1)\n"), "var x = randReal(0, 1)\n");
        assert_eq!(
            fix("var m = [:]\nremoveKey(m, \"k\")\n"),
            "var m = [:]\nmapRemove(m, \"k\")\n"
        );
    }

    /// A method named like a deprecated builtin keeps its receiver:
    /// the edit is the name token, never the call.
    #[test]
    fn rename_inside_a_bigger_expression() {
        assert_eq!(
            fix("var x = 1 + randFloat(0, 1) * 2\n"),
            "var x = 1 + randReal(0, 1) * 2\n"
        );
    }

    /// `subArray`'s end index is inclusive, `arraySlice`'s exclusive —
    /// the fix compensates, like `leek-migrate`'s v3→v4 pass.
    #[test]
    fn subarray_fix_bumps_the_end_index() {
        assert_eq!(
            fix("var x = subArray([1, 2, 3], 0, 2)\n"),
            "var x = arraySlice([1, 2, 3], 0, (2) + 1)\n"
        );
        assert_eq!(
            fix("var a = [1]\nvar x = subArray(a, 0, count(a) - 1)\n"),
            "var a = [1]\nvar x = arraySlice(a, 0, (count(a) - 1) + 1)\n"
        );
    }

    /// The end-index bump reshapes an argument, so it stays out of
    /// `source.fixAll`; a pure rename does not.
    #[test]
    fn applicability_matches_the_edit() {
        let d = run("var x = randFloat(0, 1)\n");
        assert_eq!(
            d[0].suggestions[0].applicability,
            Applicability::MachineApplicable
        );
        let d = run("var x = subArray([1, 2, 3], 0, 2)\n");
        assert_eq!(
            d[0].suggestions[0].applicability,
            Applicability::MaybeIncorrect
        );
    }

    /// A `subArray` call we can't rewrite faithfully gets no fix at
    /// all rather than a rename that drops the last element.
    #[test]
    fn no_fix_for_unexpected_subarray_arity() {
        let d = run("var x = subArray([1, 2, 3], 0)\n");
        assert_eq!(d.len(), 1);
        assert!(d[0].suggestions.is_empty(), "got {:?}", d[0].suggestions);
        assert!(d[0].notes.iter().any(|n| n.contains("by hand")));
    }

    #[test]
    fn suggestions_are_applicable() {
        for src in [
            "var x = randFloat(0, 1)\n",
            "var x = subArray([1, 2, 3], 0, 2)\n",
        ] {
            assert_suggestions_fix(|| DeprecatedFeature, src);
        }
    }
}
