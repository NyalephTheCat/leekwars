//! L0003 `ShadowedBinding` — flag local variables / parameters
//! that declare a name already in scope from an outer binding.
//!
//! Catches accidental re-declarations like
//!
//! ```text
//! var i = 0
//! for (var i = 0; i < 10; i++) { … }  // ← inner `i` shadows outer
//! ```
//!
//! ## Detection
//!
//! Walk each body keeping a stack of currently-in-scope locals. Push
//! on every block entry, pop on exit; each `Stmt::VarDecl` and
//! foreach binding both declares the new name AND triggers the
//! check. Function parameters seed the outermost scope.
//!
//! This rule keeps its own statement walk (instead of the driver's
//! `check_stmt` hook) because the scope stack must mirror block
//! nesting exactly — push/pop bracketing doesn't map onto flat
//! per-statement callbacks.
//!
//! ## Known limitations
//!
//! - **Shadowing globals isn't flagged.** Globals are an explicit
//!   namespace; declaring a local `count` even though there's a
//!   `global count` is usually intentional.
//! - **Lambdas don't open a checked scope.** Their parameter
//!   names CAN shadow outer locals, but flagging that is noisy
//!   in the common arrayMap(arr, x => …) pattern.

use std::collections::HashSet;

use leek_diagnostics::{Diagnostic, codes};
use leek_hir::{ForStmt, ForeachStmt, Stmt, VarDecl};

use crate::LintGroup;
use crate::pass::{Body, BodyKind, LintCx, LintMeta, LintPass};

pub struct ShadowedBinding;

static META: LintMeta = LintMeta {
    name: "shadowed-binding",
    code: codes::SHADOWED_BINDING,
    group: LintGroup::Style,
    description: "binding that shadows an outer binding with the same name",
};

impl LintPass for ShadowedBinding {
    fn meta(&self) -> &'static LintMeta {
        &META
    }

    fn check_body(&mut self, cx: &mut LintCx<'_, '_>, body: &Body<'_>) {
        if body.kind == BodyKind::Lambda {
            return; // see "Known limitations" above
        }
        let mut out = Vec::new();
        // Parameters seed the outermost scope; the body's own
        // statements get a fresh scope on top so a body `var x`
        // that reuses a param name fires the shadow check.
        let mut scopes: Vec<HashSet<String>> = vec![HashSet::new(), HashSet::new()];
        for p in body.params {
            scopes[0].insert(p.name.clone());
        }
        for s in body.stmts {
            walk_stmt(s, &mut scopes, &mut out);
        }
        for d in out {
            cx.emit(d);
        }
    }
}

fn walk_stmt(s: &Stmt, scopes: &mut Vec<HashSet<String>>, out: &mut Vec<Diagnostic>) {
    match s {
        Stmt::VarDecl(v) => declare(v, scopes, out),
        Stmt::Block(b) => {
            scopes.push(HashSet::new());
            for inner in &b.stmts {
                walk_stmt(inner, scopes, out);
            }
            scopes.pop();
        }
        Stmt::If(i) => {
            scopes.push(HashSet::new());
            walk_stmt(&i.then_branch, scopes, out);
            scopes.pop();
            if let Some(e) = &i.else_branch {
                scopes.push(HashSet::new());
                walk_stmt(e, scopes, out);
                scopes.pop();
            }
        }
        Stmt::While(w) => {
            scopes.push(HashSet::new());
            walk_stmt(&w.body, scopes, out);
            scopes.pop();
        }
        Stmt::DoWhile(d) => {
            scopes.push(HashSet::new());
            walk_stmt(&d.body, scopes, out);
            scopes.pop();
        }
        Stmt::For(fr) => walk_for(fr, scopes, out),
        Stmt::Foreach(fe) => walk_foreach(fe, scopes, out),
        Stmt::Switch(sw) => {
            for arm in &sw.arms {
                scopes.push(HashSet::new());
                for inner in &arm.body {
                    walk_stmt(inner, scopes, out);
                }
                scopes.pop();
            }
        }
        _ => {}
    }
}

fn declare(v: &VarDecl, scopes: &mut [HashSet<String>], out: &mut Vec<Diagnostic>) {
    if v.is_global {
        // Globals are their own namespace; ignore.
        return;
    }
    let shadows = scopes[..scopes.len() - 1]
        .iter()
        .any(|s| s.contains(&v.name));
    if shadows {
        out.push(diagnostic(&v.name, v.span));
    }
    scopes
        .last_mut()
        .expect("at least one scope")
        .insert(v.name.clone());
}

fn walk_for(fr: &ForStmt, scopes: &mut Vec<HashSet<String>>, out: &mut Vec<Diagnostic>) {
    // The init's scope wraps the body so a `var i` in the header
    // is visible inside but not after the loop.
    scopes.push(HashSet::new());
    if let Some(init) = &fr.init {
        walk_stmt(init, scopes, out);
    }
    walk_stmt(&fr.body, scopes, out);
    scopes.pop();
}

fn walk_foreach(fe: &ForeachStmt, scopes: &mut Vec<HashSet<String>>, out: &mut Vec<Diagnostic>) {
    scopes.push(HashSet::new());
    // The key + value bindings are foreach-owned; flag if they
    // shadow an outer local.
    if let Some(k) = &fe.key {
        let shadows = scopes[..scopes.len() - 1]
            .iter()
            .any(|s| s.contains(&k.name));
        if shadows {
            out.push(diagnostic(&k.name, k.span));
        }
        scopes.last_mut().unwrap().insert(k.name.clone());
    }
    let v_shadows = scopes[..scopes.len() - 1]
        .iter()
        .any(|s| s.contains(&fe.value.name));
    if v_shadows {
        out.push(diagnostic(&fe.value.name, fe.value.span));
    }
    scopes.last_mut().unwrap().insert(fe.value.name.clone());
    walk_stmt(&fe.body, scopes, out);
    scopes.pop();
}

fn diagnostic(name: &str, span: leek_span::Span) -> Diagnostic {
    use leek_diagnostics::Severity;
    Diagnostic::new(
        codes::SHADOWED_BINDING,
        Severity::Hint,
        span,
        format!("`{name}` shadows an outer binding with the same name"),
    )
    .with_note("rename one of the two for clarity, or `@allow(L0003)` if intentional")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::lint_one;

    fn run(src: &str) -> Vec<Diagnostic> {
        lint_one(ShadowedBinding, src)
    }

    #[test]
    fn flags_inner_shadows_outer() {
        let d = run("var i = 0\nfor (var i = 0; i < 10; ++i) { var x = i }\n");
        assert_eq!(d.len(), 1, "got {d:?}");
        assert!(d[0].message.contains("shadows"));
    }

    #[test]
    fn flags_param_shadow_in_body() {
        let d = run("function f(integer x) { var x = 1 }\n");
        assert_eq!(d.len(), 1);
    }

    #[test]
    fn no_finding_when_disjoint_scopes() {
        let d = run("if (true) { var x = 1 }\nif (true) { var x = 2 }\n");
        assert!(d.is_empty(), "got {d:?}");
    }

    #[test]
    fn flags_foreach_bind_shadow() {
        let d = run("var v = 1\nfor (var v in [1, 2]) { var x = v }\n");
        assert_eq!(d.len(), 1);
    }
}
