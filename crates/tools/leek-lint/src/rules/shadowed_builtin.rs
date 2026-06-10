//! L0038 `ShadowedBuiltin` (nursery) — flag declarations that reuse a
//! builtin function's name:
//!
//! ```leekscript
//! var count = 0            // shadows the builtin count(...)
//! function debug(x) { … }  // shadows the builtin debug(...)
//! ```
//!
//! The local wins from then on, so a later `count(items)` call stops
//! meaning what it says — a confusing failure mode for newcomers who
//! don't yet know the builtin catalog. Sibling of
//! [`shadowed-binding`](super::shadowed_binding), but against the
//! standard library instead of outer locals.

use leek_diagnostics::{Diagnostic, codes};
use leek_hir::Stmt;
use leek_resolver::builtins::is_builtin_name;

use crate::LintGroup;
use crate::pass::{Body, BodyKind, LintCx, LintMeta, LintPass};

pub struct ShadowedBuiltin;

static META: LintMeta = LintMeta {
    name: "shadowed-builtin",
    code: codes::SHADOWED_BUILTIN,
    group: LintGroup::Nursery,
    description: "declaration reuses a builtin function's name — calls to the builtin now hit the local",
};

impl LintPass for ShadowedBuiltin {
    fn meta(&self) -> &'static LintMeta {
        &META
    }

    fn check_body(&mut self, cx: &mut LintCx<'_, '_>, body: &Body<'_>) {
        // A user function named like a builtin replaces it everywhere.
        if body.kind == BodyKind::Function
            && let Some(name) = body.name
            && is_builtin_name(name)
        {
            cx.emit(diagnostic("function", name, body.span));
        }
        let mut findings = Vec::new();
        for p in body.params {
            if is_builtin_name(&p.name) {
                findings.push(diagnostic("parameter", &p.name, p.span));
            }
        }
        for d in findings {
            cx.emit(d);
        }
    }

    fn check_stmt(&mut self, cx: &mut LintCx<'_, '_>, s: &Stmt) {
        let mut findings = Vec::new();
        match s {
            Stmt::VarDecl(v) if is_builtin_name(&v.name) => {
                findings.push(diagnostic("variable", &v.name, v.span));
            }
            Stmt::Foreach(fe) => {
                if let Some(k) = &fe.key
                    && is_builtin_name(&k.name)
                {
                    findings.push(diagnostic("loop binding", &k.name, k.span));
                }
                if is_builtin_name(&fe.value.name) {
                    findings.push(diagnostic("loop binding", &fe.value.name, fe.value.span));
                }
            }
            _ => {}
        }
        for d in findings {
            cx.emit(d);
        }
    }
}

fn diagnostic(what: &str, name: &str, span: leek_span::Span) -> Diagnostic {
    Diagnostic::new(
        codes::SHADOWED_BUILTIN,
        leek_diagnostics::Severity::Hint,
        span,
        format!("{what} `{name}` shadows the builtin function `{name}`"),
    )
    .with_note(format!(
        "later calls to `{name}(...)` will use this {what} instead of the builtin — rename it to keep the standard library reachable"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::lint_one;

    fn run(src: &str) -> Vec<Diagnostic> {
        lint_one(ShadowedBuiltin, src)
    }

    #[test]
    fn flags_variable_named_like_builtin() {
        let d = run("function f(items) {\n  var count = 0\n  return count\n}\n");
        assert_eq!(d.len(), 1, "got {d:?}");
        assert!(d[0].message.contains("variable `count`"), "{d:?}");
    }

    #[test]
    fn flags_function_named_like_builtin() {
        let d = run("function debug(x) {\n  return x\n}\n");
        assert_eq!(d.len(), 1, "got {d:?}");
        assert!(d[0].message.contains("function `debug`"), "{d:?}");
    }

    #[test]
    fn flags_parameter_named_like_builtin() {
        let d = run("function f(min) {\n  return min\n}\n");
        assert_eq!(d.len(), 1, "got {d:?}");
        assert!(d[0].message.contains("parameter `min`"), "{d:?}");
    }

    #[test]
    fn flags_foreach_binding_named_like_builtin() {
        let d = run(
            "function f(items) {\n  for (var push in items) {\n    return push\n  }\n  return null\n}\n",
        );
        assert_eq!(d.len(), 1, "got {d:?}");
    }

    #[test]
    fn ignores_ordinary_names() {
        let d = run(
            "function f(items) {\n  var total = 0\n  for (var item in items) {\n    total += item\n  }\n  return total\n}\n",
        );
        assert!(d.is_empty(), "got {d:?}");
    }
}
