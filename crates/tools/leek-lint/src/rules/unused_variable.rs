//! L0001 `UnusedVariable` — warn on local variables that are
//! declared but never referenced inside their enclosing body.
//!
//! Heuristic: per body (main, each function/method/constructor, each
//! block-bodied lambda), collect every `Stmt::VarDecl { is_global:
//! false, .. }`, then scan every expression — *including* nested
//! lambda bodies, so a captured variable counts as used — for
//! `ExprKind::Name(NameRef::Local(def_id))`. Any declared def_id that
//! never appears as a reference is reported.
//!
//! ## Known limitations
//!
//! - **Assignment counts as use.** `x = 5;` keeps `x` alive even
//!   if `x` is never read. A stricter `UnusedAssignment` rule can
//!   live in its own slot later.
//! - **Parameters are not checked** here; see `unused-parameter`.

use std::collections::{HashMap, HashSet};

use leek_diagnostics::{Diagnostic, codes};
use leek_hir::{Callee, ExprKind, NameRef, Stmt};
use leek_span::Span;

use super::{for_each_expr_deep_in_stmts, for_each_stmt};
use crate::LintGroup;
use crate::pass::{Body, LintCx, LintMeta, LintPass};

pub struct UnusedVariable;

static META: LintMeta = LintMeta {
    name: "unused-variable",
    code: codes::UNUSED_VARIABLE,
    group: LintGroup::Suspicious,
    description: "local variable declared but never used",
};

impl LintPass for UnusedVariable {
    fn meta(&self) -> &'static LintMeta {
        &META
    }

    fn check_body(&mut self, cx: &mut LintCx<'_, '_>, body: &Body<'_>) {
        let mut decls: HashMap<leek_hir::DefId, DeclInfo> = HashMap::new();
        let mut used: HashSet<leek_hir::DefId> = HashSet::new();

        // Pass 1: collect every VarDecl in this body (recursively
        // including nested blocks, but *not* nested lambda bodies —
        // those are their own `Body` and get their own check).
        for_each_stmt(body.stmts, &mut |s| {
            if let Stmt::VarDecl(v) = s
                && !v.is_global
            {
                decls.insert(
                    v.def,
                    DeclInfo {
                        name: v.name.clone(),
                        span: v.span,
                    },
                );
            }
        });

        // Pass 2: scan every expression for local references. The
        // *deep* walker descends into lambda bodies so a variable
        // captured by a lambda counts as used. Calling a local
        // (`g(1)`) references it through `Callee::Function`, not an
        // `ExprKind::Name`, so check both.
        for_each_expr_deep_in_stmts(body.stmts, &mut |e| match &e.kind {
            ExprKind::Name(NameRef::Local(def_id)) => {
                used.insert(*def_id);
            }
            ExprKind::Call(call) => {
                if let Callee::Function(NameRef::Local(def_id)) = &call.callee {
                    used.insert(*def_id);
                }
            }
            _ => {}
        });

        // Pass 3: emit findings.
        let mut findings: Vec<_> = decls
            .into_iter()
            .filter(|(d, _)| !used.contains(d))
            .collect();
        findings.sort_by_key(|(_, info)| info.span.start);

        for (_, info) in findings {
            cx.emit(diagnostic_unused(&info.name, info.span));
        }
    }
}

struct DeclInfo {
    name: String,
    /// Full statement span — used as the deletion target for the
    /// quick-fix suggestion.
    span: Span,
}

fn diagnostic_unused(name: &str, span: Span) -> Diagnostic {
    use leek_diagnostics::{Applicability, Suggestion, TextEdit};
    Diagnostic::warning(
        codes::UNUSED_VARIABLE,
        span,
        format!("variable `{name}` is declared but never used"),
    )
    .with_note(format!(
        "if this is intentional, prefix `{name}` with `_` or annotate `@unused` once wired up"
    ))
    .with_suggestion(Suggestion {
        message: format!("remove unused `{name}`"),
        edits: vec![TextEdit {
            span,
            replacement: String::new(),
        }],
        // Deletion can theoretically change observable behavior
        // (e.g. removing an initializer with side effects), so
        // MaybeIncorrect — the LSP surfaces it but doesn't mark it
        // preferred for auto-apply.
        applicability: Applicability::MaybeIncorrect,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::lint_one;

    fn run(src: &str) -> Vec<Diagnostic> {
        lint_one(UnusedVariable, src)
    }

    #[test]
    fn flags_unused_local() {
        let d = run("function f() {\n  var x = 1\n  return 0\n}\n");
        assert_eq!(d.len(), 1, "got {d:?}");
        assert!(d[0].message.contains('x'), "{d:?}");
    }

    #[test]
    fn silent_when_used() {
        let d = run("function f() {\n  var x = 1\n  return x\n}\n");
        assert!(d.is_empty(), "got {d:?}");
    }

    #[test]
    fn variable_used_only_in_lambda_is_used() {
        // `y` is captured by the lambda — that's a use.
        let d = run("function f() {\n  var y = 2\n  var g = x -> x + y\n  return g(1)\n}\n");
        assert!(d.is_empty(), "got {d:?}");
    }

    #[test]
    fn unused_inside_lambda_body_is_flagged() {
        // The lambda's own body is a scope of its own; its dead local
        // is still found.
        let d = run(
            "function f() {\n  var g = function() {\n    var dead = 1\n    return 0\n  }\n  return g()\n}\n",
        );
        assert_eq!(d.len(), 1, "got {d:?}");
        assert!(d[0].message.contains("dead"), "{d:?}");
    }
}
