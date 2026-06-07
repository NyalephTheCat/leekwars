//! L0001 `UnusedVariable` — warn on local variables that are
//! declared but never referenced inside their enclosing scope.
//!
//! Heuristic: per function body (and the top-level main block),
//! collect every `Stmt::VarDecl { is_global: false, .. }`, then
//! scan every expression in that body for `ExprKind::Name(
//! NameRef::Local(def_id))`. Any declared def_id that doesn't
//! appear as a reference is reported.
//!
//! ## Known limitations (v0.1)
//!
//! - **Assignment counts as use.** `x = 5;` keeps `x` alive even
//!   if `x` is never read. A stricter `UnusedAssignment` rule can
//!   live in its own slot later.
//! - **Parameters are not checked.** Many interfaces are declared
//!   wider than the body consumes (callbacks, framework hooks);
//!   flagging them would be noisy without `@unused` annotations.
//! - **Lambdas aren't descended.** The shared HIR walker stops at
//!   lambda boundaries to keep the rule cheap.

use std::collections::{HashMap, HashSet};

use leek_diagnostics::{Diagnostic, codes};
use leek_hir::{Block, Def, ExprKind, HirFile, NameRef, Stmt};
use leek_span::Span;

use super::{for_each_expr_in_block, for_each_stmt};
use crate::LintRule;

pub struct UnusedVariable;

impl LintRule for UnusedVariable {
    fn name(&self) -> &'static str {
        "unused-variable"
    }

    fn code(&self) -> leek_diagnostics::Code {
        codes::UNUSED_VARIABLE
    }

    fn check(&self, file: &HirFile, out: &mut Vec<Diagnostic>) {
        // Top-level main block.
        check_block(&file.main, out, file);

        // Every user function and method body.
        for def in &file.defs {
            match def {
                Def::Function(fun) => {
                    if let Some(body) = &fun.body {
                        check_block(&body.stmts, out, file);
                    }
                }
                Def::Class(cls) => {
                    for m in cls.methods.iter().chain(cls.constructors.iter()) {
                        if let Some(body) = &m.body {
                            check_block(&body.stmts, out, file);
                        }
                    }
                }
                Def::Global(_) | Def::Local(_) => {}
            }
        }
    }
}

/// Collect declared local DefIds in `stmts`, scan for references,
/// and emit a diagnostic for each declared local with zero refs.
///
/// We treat the entire `stmts` slice as one scope. Nested blocks
/// follow the same rule because the walker descends through them
/// when collecting references — a local declared in an outer block
/// is "used" if any inner block references it.
fn check_block(stmts: &[Stmt], out: &mut Vec<Diagnostic>, _file: &HirFile) {
    let mut decls: HashMap<leek_hir::DefId, DeclInfo> = HashMap::new();
    let mut used: HashSet<leek_hir::DefId> = HashSet::new();

    // Pass 1: collect every VarDecl in this scope (recursively
    // including nested blocks — the rule fires regardless of which
    // sub-block the unused var was declared in).
    let block = Block {
        stmts: stmts.to_vec(),
        span: Span::synthetic(),
    };
    for_each_stmt(&block, &mut |s| {
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

    // Pass 2: scan every expression for local references.
    for_each_expr_in_block(&block, &mut |e| {
        if let ExprKind::Name(NameRef::Local(def_id)) = e.kind {
            used.insert(def_id);
        }
    });

    // Pass 3: emit findings.
    let mut findings: Vec<_> = decls
        .into_iter()
        .filter(|(d, _)| !used.contains(d))
        .collect();
    findings.sort_by_key(|(_, info)| info.span.start);

    for (_, info) in findings {
        out.push(diagnostic_unused(&info.name, info.span));
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
