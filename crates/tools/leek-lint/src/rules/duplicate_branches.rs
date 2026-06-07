//! L0009 `DuplicateBranches` — flag `if (c) X else X` where the `then`
//! and `else` branches are structurally identical.
//!
//! When both arms do the same thing the condition is pointless — almost
//! always a copy-paste bug where one arm was meant to differ. Comparison
//! is span-insensitive and binding-aware (see [`super::structural`]); a
//! branch that declares its own locals won't collide, so we never report
//! a false duplicate.

use leek_diagnostics::{Diagnostic, codes};
use leek_hir::{Block, Def, HirFile, Stmt};

use super::{for_each_stmt, structural::stmt_key};
use crate::LintRule;

pub struct DuplicateBranches;

impl LintRule for DuplicateBranches {
    fn name(&self) -> &'static str {
        "duplicate-branches"
    }

    fn code(&self) -> leek_diagnostics::Code {
        codes::DUPLICATE_BRANCHES
    }

    fn check(&self, file: &HirFile, out: &mut Vec<Diagnostic>) {
        check_stmts(&file.main, out);
        for def in &file.defs {
            match def {
                Def::Function(fun) => {
                    if let Some(body) = &fun.body {
                        check_stmts(&body.stmts, out);
                    }
                }
                Def::Class(cls) => {
                    for m in cls.methods.iter().chain(cls.constructors.iter()) {
                        if let Some(body) = &m.body {
                            check_stmts(&body.stmts, out);
                        }
                    }
                }
                Def::Global(_) | Def::Local(_) => {}
            }
        }
    }
}

fn check_stmts(stmts: &[Stmt], out: &mut Vec<Diagnostic>) {
    let wrapper = Block {
        stmts: stmts.to_vec(),
        span: leek_span::Span::synthetic(),
    };
    for_each_stmt(&wrapper, &mut |s| {
        if let Stmt::If(i) = s
            && let Some(else_b) = &i.else_branch
            // An `else if` chain has an `If` else-branch; skip those (the
            // inner `if` is visited separately) so we only compare a real
            // then/else pair.
            && !matches!(&**else_b, Stmt::If(_))
            && !is_empty_branch(&i.then_branch)
            && stmt_key(&i.then_branch) == stmt_key(else_b)
        {
            out.push(diagnostic(i.span));
        }
    });
}

/// An empty `{ }` branch is the `EmptyBlock` lint's territory, not ours.
fn is_empty_branch(s: &Stmt) -> bool {
    matches!(s, Stmt::Block(b) if b.stmts.is_empty())
}

fn diagnostic(span: leek_span::Span) -> Diagnostic {
    Diagnostic::warning(
        codes::DUPLICATE_BRANCHES,
        span,
        "both branches of this `if` are identical".to_string(),
    )
    .with_note(
        "the condition has no effect — e.g. `if (c) { f() } else { f() }` is just `f()`. \
         Did one branch mean to do something different?",
    )
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
        DuplicateBranches.check(&hir, &mut out);
        out
    }

    #[test]
    fn flags_identical_branches() {
        let d = run("function f(x) {\n  if (x) { return 1 } else { return 1 }\n}\n");
        assert_eq!(d.len(), 1, "got {d:?}");
    }

    #[test]
    fn ignores_differing_branches() {
        let d = run("function f(x) {\n  if (x) { return 1 } else { return 2 }\n}\n");
        assert!(d.is_empty(), "got {d:?}");
    }

    #[test]
    fn ignores_branch_with_local_decl() {
        // Each branch declares its own `y` (distinct DefIds) → not a
        // reported duplicate (conservative).
        let d = run("function f(x) {\n  if (x) { var y = 1 } else { var y = 1 }\n}\n");
        assert!(d.is_empty(), "got {d:?}");
    }

    #[test]
    fn ignores_else_if_chain() {
        let d = run("function f(x, z) {\n  if (x) { return 1 } else if (z) { return 2 }\n}\n");
        assert!(d.is_empty(), "got {d:?}");
    }
}
