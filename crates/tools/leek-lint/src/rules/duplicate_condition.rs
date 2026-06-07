//! L0017 `DuplicateCondition` — flag an `if … else if` chain that tests
//! the same (side-effect-free) condition twice. A later arm with a
//! condition identical to an earlier one is unreachable.

use leek_diagnostics::{Diagnostic, codes};
use leek_hir::{Block, Def, HirFile, IfStmt, Stmt};

use super::structural::{expr_key, has_side_effect};
use crate::LintRule;

pub struct DuplicateCondition;

impl LintRule for DuplicateCondition {
    fn name(&self) -> &'static str {
        "duplicate-condition"
    }

    fn code(&self) -> leek_diagnostics::Code {
        codes::DUPLICATE_CONDITION
    }

    fn check(&self, file: &HirFile, out: &mut Vec<Diagnostic>) {
        let main = Block {
            stmts: file.main.clone(),
            span: leek_span::Span::synthetic(),
        };
        walk(&Stmt::Block(main), out);
        for def in &file.defs {
            match def {
                Def::Function(fun) => {
                    if let Some(body) = &fun.body {
                        walk(&Stmt::Block(body.clone()), out);
                    }
                }
                Def::Class(cls) => {
                    for m in cls.methods.iter().chain(cls.constructors.iter()) {
                        if let Some(body) = &m.body {
                            walk(&Stmt::Block(body.clone()), out);
                        }
                    }
                }
                Def::Global(_) | Def::Local(_) => {}
            }
        }
    }
}

/// Recurse into every statement, handling `if … else if` chains as a
/// unit so each chain is checked exactly once.
fn walk(s: &Stmt, out: &mut Vec<Diagnostic>) {
    match s {
        Stmt::If(i) => walk_chain(i, out),
        Stmt::While(w) => walk(&w.body, out),
        Stmt::DoWhile(d) => walk(&d.body, out),
        Stmt::For(f) => {
            if let Some(init) = &f.init {
                walk(init, out);
            }
            walk(&f.body, out);
        }
        Stmt::Foreach(fe) => walk(&fe.body, out),
        Stmt::Block(b) => {
            for s in &b.stmts {
                walk(s, out);
            }
        }
        Stmt::Switch(sw) => {
            for arm in &sw.arms {
                for s in &arm.body {
                    walk(s, out);
                }
            }
        }
        _ => {}
    }
}

fn walk_chain(first: &IfStmt, out: &mut Vec<Diagnostic>) {
    let mut seen: Vec<String> = Vec::new();
    let mut cur = first;
    loop {
        // Only deterministic conditions can be "definitely" duplicated;
        // a side-effecting condition might legitimately differ on re-eval.
        if !has_side_effect(&cur.cond) {
            let key = expr_key(&cur.cond);
            if seen.iter().any(|k| k == &key) {
                out.push(diagnostic(cur.cond.span));
            } else {
                seen.push(key);
            }
        }
        walk(&cur.then_branch, out);
        match cur.else_branch.as_deref() {
            Some(Stmt::If(next)) => cur = next,
            Some(other) => {
                walk(other, out);
                break;
            }
            None => break,
        }
    }
}

fn diagnostic(span: leek_span::Span) -> Diagnostic {
    Diagnostic::warning(
        codes::DUPLICATE_CONDITION,
        span,
        "this condition is identical to an earlier branch in the chain".to_string(),
    )
    .with_note("the earlier branch always wins, so this arm can never run — did you mean a different condition?")
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
        DuplicateCondition.check(&hir, &mut out);
        out
    }

    #[test]
    fn flags_repeated_condition() {
        let d = run("function f(x) {\n  if (x > 0) { return 1 } else if (x > 0) { return 2 }\n  return 0\n}\n");
        assert_eq!(d.len(), 1, "got {d:?}");
    }

    #[test]
    fn ignores_distinct_conditions() {
        let d = run("function f(x) {\n  if (x > 0) { return 1 } else if (x < 0) { return 2 }\n  return 0\n}\n");
        assert!(d.is_empty(), "got {d:?}");
    }

    #[test]
    fn flags_third_arm_duplicate() {
        let d = run("function f(x) {\n  if (x == 1) { return 1 } else if (x == 2) { return 2 } else if (x == 1) { return 3 }\n  return 0\n}\n");
        assert_eq!(d.len(), 1, "got {d:?}");
    }
}
