//! The [`LintPass`] trait and the single-pass driver.
//!
//! Every lint registers hooks (`check_file`, `check_body`,
//! `check_block`, `check_stmt`, `check_expr`) and the driver fires
//! them all during **one** traversal of the HIR — the tree is walked
//! once regardless of how many lints run, instead of once per rule.
//!
//! The traversal is built on `leek-hir`'s canonical shallow walkers
//! ([`walk_stmt_child_exprs`], [`walk_expr_children`]) so the
//! variant-complete `match`es stay in one crate. The driver adds the
//! two pieces of structure the shallow walkers can't express:
//!
//! - **Bodies.** The main block, each function / method / constructor
//!   body, and each block-bodied lambda fire [`LintPass::check_body`]
//!   with their parameters and statements ([`Body`]). Unlike the old
//!   per-rule walkers, lambda bodies *are* descended — a lambda is a
//!   body like any other, with [`LintCx::depth`] reset at its
//!   boundary.
//! - **Nesting depth.** [`LintCx::depth`] counts control-flow nesting
//!   within the current body. Bare `{}` blocks are transparent and an
//!   `else if` continues its chain at the same depth, so the count
//!   matches how a reader indents the code.

use leek_diagnostics::{Code, Diagnostic};
use leek_hir::{
    Def, Expr, ExprKind, HirFile, LambdaBody, Param, Stmt, walk_expr_children,
    walk_stmt_child_exprs,
};
use leek_span::Span;

use crate::group::LintGroup;

/// Static description of a lint. One `static META: LintMeta` per
/// rule module; [`LintPass::meta`] returns a reference to it.
#[derive(Debug)]
pub struct LintMeta {
    /// Stable kebab-case identifier — also the value users put in
    /// `@allow(<name>)` and (eventually) `--allow=<name>`.
    pub name: &'static str,
    /// The `L0xxx` diagnostic code this lint emits.
    pub code: Code,
    /// Group the lint belongs to; decides default-on vs opt-in.
    pub group: LintGroup,
    /// One-line "what and why" for `--explain`-style output.
    pub description: &'static str,
}

/// What kind of callable a [`Body`] belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BodyKind {
    /// The file's top-level statements.
    Main,
    Function,
    Method,
    Constructor,
    /// A block-bodied lambda (`(x) => { … }`). Expression-bodied
    /// lambdas don't form a body; their expression is just walked.
    Lambda,
}

/// A callable body: its signature plus top-level statements. Borrowed
/// straight from the [`HirFile`] — no cloning.
pub struct Body<'a> {
    pub kind: BodyKind,
    /// `None` for [`BodyKind::Main`] and lambdas.
    pub name: Option<&'a str>,
    pub params: &'a [Param],
    pub stmts: &'a [Stmt],
    /// The defining item's span ([`Span::synthetic`] for `Main`).
    pub span: Span,
}

/// Shared state handed to every hook.
pub struct LintCx<'a, 'o> {
    /// The file under analysis.
    pub file: &'a HirFile,
    /// Control-flow nesting depth within the current body. Statements
    /// directly in the body are at depth 0; each `if` / loop / `switch`
    /// body adds one. `else if` stays at its chain's depth and bare
    /// `{}` blocks are transparent.
    pub depth: usize,
    out: &'o mut Vec<Diagnostic>,
}

impl LintCx<'_, '_> {
    /// Report a finding.
    pub fn emit(&mut self, d: Diagnostic) {
        self.out.push(d);
    }
}

/// A single lint. Implement only the hooks the lint needs; each
/// defaults to a no-op. Hooks take `&mut self` so a pass may keep
/// state across calls (it is constructed fresh for every run).
pub trait LintPass {
    /// Static metadata: name, code, group, description.
    fn meta(&self) -> &'static LintMeta;

    /// Called once, before any traversal. For whole-file lints that
    /// drive their own walk.
    fn check_file(&mut self, cx: &mut LintCx<'_, '_>) {
        let _ = cx;
    }

    /// Called for the main block, every function / method /
    /// constructor body, and every block-bodied lambda.
    fn check_body(&mut self, cx: &mut LintCx<'_, '_>, body: &Body<'_>) {
        let _ = (cx, body);
    }

    /// Called for every statement *sequence*: body statements,
    /// `{}` block contents, and `switch`-arm bodies. Use for lints
    /// about statement ordering (e.g. unreachable code).
    fn check_block(&mut self, cx: &mut LintCx<'_, '_>, stmts: &[Stmt]) {
        let _ = (cx, stmts);
    }

    /// Called for every statement, in source order.
    fn check_stmt(&mut self, cx: &mut LintCx<'_, '_>, stmt: &Stmt) {
        let _ = (cx, stmt);
    }

    /// Called for every expression, including inside lambda bodies.
    fn check_expr(&mut self, cx: &mut LintCx<'_, '_>, expr: &Expr) {
        let _ = (cx, expr);
    }
}

/// Run `passes` over `file` in a single traversal, appending findings
/// to `out`. Callers sort the result; within the walk findings are in
/// visitation order.
pub fn run_passes(file: &HirFile, passes: &mut [Box<dyn LintPass>], out: &mut Vec<Diagnostic>) {
    let mut driver = Driver {
        passes,
        cx: LintCx {
            file,
            depth: 0,
            out,
        },
    };
    driver.run();
}

struct Driver<'a, 'o, 'p> {
    passes: &'p mut [Box<dyn LintPass>],
    cx: LintCx<'a, 'o>,
}

impl Driver<'_, '_, '_> {
    fn run(&mut self) {
        let file = self.cx.file;
        for p in self.passes.iter_mut() {
            p.check_file(&mut self.cx);
        }
        self.body(&Body {
            kind: BodyKind::Main,
            name: None,
            params: &[],
            stmts: &file.main,
            span: Span::synthetic(),
        });
        for def in &file.defs {
            match def {
                Def::Function(f) => {
                    if let Some(b) = &f.body {
                        self.body(&Body {
                            kind: BodyKind::Function,
                            name: Some(&f.name),
                            params: &f.params,
                            stmts: &b.stmts,
                            span: f.span,
                        });
                    }
                }
                Def::Class(c) => {
                    // Field initializers are expressions outside any body.
                    for field in &c.fields {
                        if let Some(init) = &field.init {
                            self.expr(init);
                        }
                    }
                    for (kind, methods) in [
                        (BodyKind::Method, &c.methods),
                        (BodyKind::Constructor, &c.constructors),
                    ] {
                        for m in methods {
                            if let Some(b) = &m.body {
                                self.body(&Body {
                                    kind,
                                    name: Some(&m.name),
                                    params: &m.params,
                                    stmts: &b.stmts,
                                    span: m.span,
                                });
                            }
                        }
                    }
                }
                // Global initializers also appear as `Stmt::VarDecl` in
                // `main`, so they're walked there — visiting them here
                // would double-report.
                Def::Global(_) | Def::Local(_) => {}
            }
        }
    }

    fn body(&mut self, b: &Body<'_>) {
        let saved = self.cx.depth;
        self.cx.depth = 0;
        for p in self.passes.iter_mut() {
            p.check_body(&mut self.cx, b);
        }
        for prm in b.params {
            if let Some(d) = &prm.default {
                self.expr(d);
            }
        }
        self.block(b.stmts);
        self.cx.depth = saved;
    }

    fn block(&mut self, stmts: &[Stmt]) {
        for p in self.passes.iter_mut() {
            p.check_block(&mut self.cx, stmts);
        }
        for s in stmts {
            self.stmt(s);
        }
    }

    fn stmt(&mut self, s: &Stmt) {
        for p in self.passes.iter_mut() {
            p.check_stmt(&mut self.cx, s);
        }
        walk_stmt_child_exprs(s, &mut |e| self.expr(e));
        // Statement descent is spelled out (instead of reusing
        // `walk_stmt_child_stmts`) because depth bookkeeping needs to
        // know *which* child it is descending into: branch and loop
        // bodies nest one deeper, an `else if` continues its chain at
        // the same depth, and a bare `{}` block is transparent.
        match s {
            Stmt::Block(b) => self.block(&b.stmts),
            Stmt::If(i) => {
                self.nested(&i.then_branch);
                if let Some(e) = &i.else_branch {
                    if matches!(**e, Stmt::If(_)) {
                        self.stmt(e);
                    } else {
                        self.nested(e);
                    }
                }
            }
            Stmt::While(w) => self.nested(&w.body),
            Stmt::DoWhile(d) => self.nested(&d.body),
            Stmt::For(f) => {
                if let Some(init) = &f.init {
                    self.stmt(init);
                }
                self.nested(&f.body);
            }
            Stmt::Foreach(fe) => self.nested(&fe.body),
            Stmt::Switch(sw) => {
                self.cx.depth += 1;
                for arm in &sw.arms {
                    self.block(&arm.body);
                }
                self.cx.depth -= 1;
            }
            Stmt::Expr(_)
            | Stmt::VarDecl(_)
            | Stmt::Return(_)
            | Stmt::Break(_)
            | Stmt::Continue(_)
            | Stmt::Include(_)
            | Stmt::Import(_)
            | Stmt::Charge(_) => {}
        }
    }

    fn nested(&mut self, s: &Stmt) {
        self.cx.depth += 1;
        self.stmt(s);
        self.cx.depth -= 1;
    }

    fn expr(&mut self, e: &Expr) {
        for p in self.passes.iter_mut() {
            p.check_expr(&mut self.cx, e);
        }
        // `walk_expr_children` treats lambdas as leaves; the driver
        // descends into them as fresh bodies instead.
        if let ExprKind::Lambda(lam) = &e.kind {
            match &lam.body {
                LambdaBody::Block(b) => self.body(&Body {
                    kind: BodyKind::Lambda,
                    name: None,
                    params: &lam.params,
                    stmts: &b.stmts,
                    span: e.span,
                }),
                LambdaBody::Expr(x) => {
                    for prm in &lam.params {
                        if let Some(d) = &prm.default {
                            self.expr(d);
                        }
                    }
                    self.expr(x);
                }
            }
            return;
        }
        walk_expr_children(e, &mut |c| self.expr(c));
    }
}
