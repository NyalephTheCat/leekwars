//! L0018 `UnusedParameter` — flag a function / method / constructor
//! parameter that is never read in its body.
//!
//! Reported as a *hint* (not a warning): an unused parameter is
//! sometimes required by a signature the function must match (a
//! callback, an overridden method). A `_`-prefixed name silences it.
//!
//! Lambda parameters are deliberately skipped: lambdas are usually
//! callbacks whose arity is dictated by the caller (`arrayMap`
//! passes the index whether you want it or not), so flagging them
//! would be noisy.

use std::collections::HashSet;

use leek_diagnostics::{Diagnostic, codes};
use leek_hir::{Callee, DefId, ExprKind, NameRef};

use super::for_each_expr_deep_in_stmts;
use crate::LintGroup;
use crate::pass::{Body, BodyKind, LintCx, LintMeta, LintPass};

pub struct UnusedParameter;

static META: LintMeta = LintMeta {
    name: "unused-parameter",
    code: codes::UNUSED_PARAMETER,
    group: LintGroup::Suspicious,
    description: "function parameter that is never read in the body",
};

impl LintPass for UnusedParameter {
    fn meta(&self) -> &'static LintMeta {
        &META
    }

    fn check_body(&mut self, cx: &mut LintCx<'_, '_>, body: &Body<'_>) {
        if body.kind == BodyKind::Lambda || body.params.is_empty() {
            return;
        }
        // A parameter used only inside a nested lambda is still used —
        // hence the *deep* walker, which descends lambda bodies. A
        // parameter invoked as a callback (`cb()`) is referenced
        // through `Callee::Function`, not an `ExprKind::Name`.
        let mut used: HashSet<DefId> = HashSet::new();
        for_each_expr_deep_in_stmts(body.stmts, &mut |e| match &e.kind {
            ExprKind::Name(NameRef::Local(d)) => {
                used.insert(*d);
            }
            ExprKind::Call(call) => {
                if let Callee::Function(NameRef::Local(d)) = &call.callee {
                    used.insert(*d);
                }
            }
            _ => {}
        });

        for p in body.params {
            // `_`-prefixed names opt out (the conventional "intentionally
            // unused" marker).
            if p.name.starts_with('_') {
                continue;
            }
            if !used.contains(&p.def) {
                cx.emit(diagnostic(&p.name, p.span));
            }
        }
    }
}

fn diagnostic(name: &str, span: leek_span::Span) -> Diagnostic {
    use leek_diagnostics::Severity;
    Diagnostic::new(
        codes::UNUSED_PARAMETER,
        Severity::Hint,
        span,
        format!("parameter `{name}` is never used"),
    )
    .with_note(format!(
        "remove it if the signature allows, or rename it to `_{name}` to mark it intentionally unused"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::lint_one;

    fn run(src: &str) -> Vec<Diagnostic> {
        lint_one(UnusedParameter, src)
    }

    #[test]
    fn flags_unused_param() {
        let d = run("function f(x, y) { return x }\n");
        assert_eq!(d.len(), 1, "got {d:?}");
        assert!(d[0].message.contains('y'), "{d:?}");
    }

    #[test]
    fn silent_when_all_used() {
        let d = run("function f(x, y) { return x + y }\n");
        assert!(d.is_empty(), "got {d:?}");
    }

    #[test]
    fn underscore_param_is_ignored() {
        let d = run("function f(x, _unused) { return x }\n");
        assert!(d.is_empty(), "got {d:?}");
    }

    #[test]
    fn param_used_in_lambda_counts() {
        // `y` is used only inside the lambda body — must not be flagged.
        let d = run("function f(y) { var g = x -> x + y return g(1) }\n");
        assert!(d.is_empty(), "got {d:?}");
    }

    #[test]
    fn lambda_params_are_skipped() {
        // The lambda ignores its own `i` — callbacks often must accept
        // arguments they don't use, so this stays silent.
        let d = run("function f(xs) { var g = (x, i) -> x return g(xs, 0) }\n");
        assert!(d.is_empty(), "got {d:?}");
    }
}
