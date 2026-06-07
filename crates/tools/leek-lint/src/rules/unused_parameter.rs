//! L0018 `UnusedParameter` — flag a function / method / constructor
//! parameter that is never read in its body.
//!
//! Reported as a *hint* (not a warning): an unused parameter is
//! sometimes required by a signature the function must match (a
//! callback, an overridden method). A `_`-prefixed name silences it.

use std::collections::HashSet;

use leek_diagnostics::{Diagnostic, codes};
use leek_hir::{Block, Def, DefId, Expr, ExprKind, HirFile, LambdaBody, NameRef, Param, Stmt};

use crate::LintRule;

pub struct UnusedParameter;

impl LintRule for UnusedParameter {
    fn name(&self) -> &'static str {
        "unused-parameter"
    }

    fn code(&self) -> leek_diagnostics::Code {
        codes::UNUSED_PARAMETER
    }

    fn check(&self, file: &HirFile, out: &mut Vec<Diagnostic>) {
        for def in &file.defs {
            match def {
                Def::Function(fun) => check_params(&fun.params, fun.body.as_ref(), out),
                Def::Class(cls) => {
                    for m in cls.methods.iter().chain(cls.constructors.iter()) {
                        check_params(&m.params, m.body.as_ref(), out);
                    }
                }
                Def::Global(_) | Def::Local(_) => {}
            }
        }
    }
}

fn check_params(params: &[Param], body: Option<&Block>, out: &mut Vec<Diagnostic>) {
    let Some(body) = body else {
        return; // signature only — nothing to analyze
    };
    if params.is_empty() {
        return;
    }
    let mut used: HashSet<DefId> = HashSet::new();
    collect_block(body, &mut used);

    for p in params {
        // `_`-prefixed names opt out (the conventional "intentionally
        // unused" marker).
        if p.name.starts_with('_') {
            continue;
        }
        if !used.contains(&p.def) {
            out.push(diagnostic(&p.name, p.span));
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

// ---- reference collection (recurses into lambda bodies) ----

fn collect_block(b: &Block, out: &mut HashSet<DefId>) {
    for s in &b.stmts {
        collect_stmt(s, out);
    }
}

fn collect_stmt(s: &Stmt, out: &mut HashSet<DefId>) {
    match s {
        Stmt::Expr(e) => collect_expr(e, out),
        Stmt::VarDecl(v) => {
            if let Some(i) = &v.init {
                collect_expr(i, out);
            }
        }
        Stmt::Return(o) => {
            if let Some(e) = o {
                collect_expr(e, out);
            }
        }
        Stmt::If(i) => {
            collect_expr(&i.cond, out);
            collect_stmt(&i.then_branch, out);
            if let Some(e) = &i.else_branch {
                collect_stmt(e, out);
            }
        }
        Stmt::While(w) => {
            collect_expr(&w.cond, out);
            collect_stmt(&w.body, out);
        }
        Stmt::DoWhile(d) => {
            collect_stmt(&d.body, out);
            collect_expr(&d.cond, out);
        }
        Stmt::For(f) => {
            if let Some(i) = &f.init {
                collect_stmt(i, out);
            }
            if let Some(c) = &f.cond {
                collect_expr(c, out);
            }
            if let Some(st) = &f.step {
                collect_expr(st, out);
            }
            collect_stmt(&f.body, out);
        }
        Stmt::Foreach(fe) => {
            collect_expr(&fe.iter, out);
            collect_stmt(&fe.body, out);
        }
        Stmt::Block(b) => collect_block(b, out),
        Stmt::Switch(sw) => {
            collect_expr(&sw.discriminant, out);
            for arm in &sw.arms {
                if let Some(c) = &arm.case {
                    collect_expr(c, out);
                }
                for s in &arm.body {
                    collect_stmt(s, out);
                }
            }
        }
        Stmt::Break(_) | Stmt::Continue(_) | Stmt::Include(_) | Stmt::Import(_) | Stmt::Charge(_) => {}
    }
}

fn collect_expr(e: &Expr, out: &mut HashSet<DefId>) {
    if let ExprKind::Name(NameRef::Local(d)) = &e.kind {
        out.insert(*d);
    }
    // `walk_expr_children` treats a lambda as a leaf, so descend into its
    // body (and param defaults) by hand — a parameter used only inside a
    // nested lambda must still count as used.
    if let ExprKind::Lambda(l) = &e.kind {
        for p in &l.params {
            if let Some(d) = &p.default {
                collect_expr(d, out);
            }
        }
        match &l.body {
            LambdaBody::Block(b) => collect_block(b, out),
            LambdaBody::Expr(ex) => collect_expr(ex, out),
        }
    }
    leek_hir::walk_expr_children(e, &mut |c| collect_expr(c, out));
}

#[cfg(test)]
mod tests {
    use super::*;
    use leek_parser::ast::{AstNode, SourceFile};
    use leek_parser::parse;
    use leek_span::SourceId;
    use leek_syntax::{SyntaxNode, Version};

    fn run(src: &str) -> Vec<Diagnostic> {
        let source = SourceId::new(1).unwrap();
        let parsed = parse(src, source, Version::V4);
        let ast = SourceFile::cast(SyntaxNode::new_root(parsed.green)).unwrap();
        let (hir, _) = leek_hir::lower_file(&ast, source);
        let mut out = Vec::new();
        UnusedParameter.check(&hir, &mut out);
        out
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
}
