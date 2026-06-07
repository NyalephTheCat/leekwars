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
use leek_hir::{Def, HirFile, Stmt, SwitchStmt};
use leek_span::Span;

use super::structural::{expr_key, has_side_effect};
use crate::LintRule;

pub struct DuplicateCase;

impl LintRule for DuplicateCase {
    fn name(&self) -> &'static str {
        "duplicate-case"
    }

    fn code(&self) -> leek_diagnostics::Code {
        codes::DUPLICATE_CASE
    }

    fn check(&self, file: &HirFile, out: &mut Vec<Diagnostic>) {
        // `for_each_block` recurses through nested blocks but treats each
        // switch arm as its own block — it does not hand us the `Switch`
        // statement itself. Walk statements directly instead.
        let main = leek_hir::Block {
            stmts: file.main.clone(),
            span: Span::synthetic(),
        };
        walk_stmts(&main.stmts, out);
        for def in &file.defs {
            match def {
                Def::Function(fun) => {
                    if let Some(body) = &fun.body {
                        walk_stmts(&body.stmts, out);
                    }
                }
                Def::Class(cls) => {
                    for m in cls.methods.iter().chain(cls.constructors.iter()) {
                        if let Some(body) = &m.body {
                            walk_stmts(&body.stmts, out);
                        }
                    }
                }
                Def::Global(_) | Def::Local(_) => {}
            }
        }
    }
}

fn walk_stmts(stmts: &[Stmt], out: &mut Vec<Diagnostic>) {
    for s in stmts {
        walk(s, out);
    }
}

fn walk(s: &Stmt, out: &mut Vec<Diagnostic>) {
    match s {
        Stmt::Switch(sw) => {
            check_switch(sw, out);
            // Recurse into arm bodies for nested switches.
            for arm in &sw.arms {
                walk_stmts(&arm.body, out);
            }
        }
        Stmt::If(i) => {
            walk(&i.then_branch, out);
            if let Some(e) = &i.else_branch {
                walk(e, out);
            }
        }
        Stmt::While(w) => walk(&w.body, out),
        Stmt::DoWhile(d) => walk(&d.body, out),
        Stmt::For(fr) => {
            if let Some(init) = &fr.init {
                walk(init, out);
            }
            walk(&fr.body, out);
        }
        Stmt::Foreach(fe) => walk(&fe.body, out),
        Stmt::Block(b) => walk_stmts(&b.stmts, out),
        _ => {}
    }
}

fn check_switch(sw: &SwitchStmt, out: &mut Vec<Diagnostic>) {
    // Track the span of the first label carrying each fingerprint so the
    // diagnostic can point back at the original.
    let mut seen: Vec<(String, Span)> = Vec::new();
    for arm in &sw.arms {
        let Some(case) = &arm.case else { continue };
        if has_side_effect(case) {
            continue;
        }
        let key = expr_key(case);
        if let Some((_, first)) = seen.iter().find(|(k, _)| k == &key) {
            out.push(diagnostic(case.span, *first));
        } else {
            seen.push((key, case.span));
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
        DuplicateCase.check(&hir, &mut out);
        out
    }

    #[test]
    fn flags_duplicate_literal_case() {
        let d = run("function f(x) {\n  switch (x) {\n    case 1: return 1\n    case 1: return 2\n  }\n  return 0\n}\n");
        assert_eq!(d.len(), 1, "got {d:?}");
        assert_eq!(d[0].labels.len(), 1, "{d:?}");
    }

    #[test]
    fn ignores_distinct_cases() {
        let d = run("function f(x) {\n  switch (x) {\n    case 1: return 1\n    case 2: return 2\n  }\n  return 0\n}\n");
        assert!(d.is_empty(), "got {d:?}");
    }

    #[test]
    fn ignores_default_arm() {
        let d = run("function f(x) {\n  switch (x) {\n    case 1: return 1\n    default: return 2\n  }\n  return 0\n}\n");
        assert!(d.is_empty(), "got {d:?}");
    }
}
