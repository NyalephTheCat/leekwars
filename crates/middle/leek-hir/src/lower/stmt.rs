//! Lower the parser AST into HIR — statement lowering for the public API.

use leek_parser::ast::{self, AstNode, Expr as AstExpr, Stmt as AstStmt};
use leek_span::Span;
use leek_syntax::{SyntaxKind, SyntaxToken};

use crate::ir::{
    Block, Def, DefId, DoWhileStmt, Expr, ForStmt, ForeachBind, ForeachStmt, IfStmt, ImportStmt,
    IncludeStmt, Stmt, SwitchArm, SwitchStmt, VarDecl, WhileStmt,
};

use super::Lowerer;
use super::traits::{LowerExpr, LowerStmt};
use super::util::{import_path, include_path};

impl LowerStmt for Lowerer {
    fn lower_stmt(&mut self, stmt: &AstStmt) -> Stmt {
        let span = self.span_of_node(stmt.syntax());
        match stmt {
            AstStmt::VarDecl(v) => Stmt::VarDecl(self.lower_var_decl(v, /*is_global=*/ false)),
            AstStmt::Return(r) => {
                let value = r.value().map(|e| self.lower_expr(&e));
                // Soft return: `return? x` returns only when the
                // expression is truthy. Detected via a `?` token
                // sitting between the `return` keyword and the
                // value. Lower as `if (value) return value;`.
                // (We duplicate the expression; the corpus only
                // uses pure reads here so the side-effect doubling
                // is harmless in practice.)
                let is_soft = r.syntax().children_with_tokens().any(|el| {
                    el.into_token()
                        .is_some_and(|t| t.kind() == SyntaxKind::Question)
                });
                if is_soft {
                    if let Some(val) = value {
                        return Stmt::If(IfStmt {
                            cond: val.clone(),
                            then_branch: Box::new(Stmt::Return(Some(val))),
                            else_branch: None,
                            span,
                        });
                    }
                    // `return?` with no value — degenerate; treat
                    // as a plain `return null` so the surrounding
                    // function still has a path that can return.
                    return Stmt::Return(None);
                }
                Stmt::Return(value)
            }
            AstStmt::Expr(e) => {
                let inner = self.lower_expr_or_null(e.expr(), span);
                Stmt::Expr(inner)
            }
            AstStmt::If(i) => self.lower_if(i),
            AstStmt::While(w) => self.lower_while(w),
            AstStmt::DoWhile(dw) => self.lower_do_while(dw, span),
            AstStmt::For(f) => self.lower_for(f, span),
            AstStmt::Foreach(fe) => self.lower_foreach(fe, span),
            AstStmt::Switch(s) => self.lower_switch(s, span),
            AstStmt::Block(b) => Stmt::Block(self.lower_block(b)),
            AstStmt::Break(_) => Stmt::Break(span),
            AstStmt::Continue(_) => Stmt::Continue(span),
            AstStmt::Include(inc) => {
                let path = include_path(inc).unwrap_or_default();
                Stmt::Include(IncludeStmt { path, span })
            }
            AstStmt::Import(imp) => {
                let path = import_path(imp).unwrap_or_default();
                Stmt::Import(ImportStmt { path, span })
            }
        }
    }
}

impl Lowerer {
    // ---- Statements ----

    pub(crate) fn lower_block(&mut self, block: &ast::Block) -> Block {
        let span = self.span_of_node(block.syntax());
        self.push_scope();
        let mut stmts = Vec::new();
        for s in block.stmts() {
            self.lower_stmt_flat(&s, &mut stmts);
        }
        self.pop_scope();
        Block { stmts, span }
    }

    /// Lower a statement and append the result(s) to `out`. Handles
    /// the case where a single source statement (like `var a, b = 1`)
    /// produces multiple HIR statements that share the surrounding
    /// scope.
    pub(crate) fn lower_stmt_flat(&mut self, stmt: &AstStmt, out: &mut Vec<Stmt>) {
        if let AstStmt::VarDecl(v) = stmt {
            // `global x = init` at file top level: declare `x` as a
            // real `Def::Global` so functions defined elsewhere can
            // resolve it via `file_decls`. Local `var`s stay local.
            let is_global = v
                .syntax()
                .children_with_tokens()
                .filter_map(rowan::NodeOrToken::into_token)
                .any(|t| t.kind() == SyntaxKind::KwGlobal);
            for d in self.lower_var_decls(v, is_global) {
                out.push(Stmt::VarDecl(d));
            }
            return;
        }
        out.push(self.lower_stmt(stmt));
    }

    pub(crate) fn lower_var_decl(&mut self, v: &ast::VarDeclStmt, is_global: bool) -> VarDecl {
        // Single-declaration form — kept for callers that always
        // get exactly one decl. Use [`lower_var_decls`] for
        // `var a, b, c = 3`.
        let decls = self.lower_var_decls(v, is_global);
        decls.into_iter().next().unwrap_or(VarDecl {
            def: DefId(0),
            name: String::new(),
            ty: None,
            init: None,
            is_global,
            span: self.span_of_node(v.syntax()),
        })
    }

    /// Lower `var a, b = 1, c` into one VarDecl per declarator. The
    /// AST puts identifiers and expressions interleaved as direct
    /// children of `VarDeclStmt`; pair each identifier with the next
    /// `=` expression if present, else `None`.
    pub(crate) fn lower_var_decls(
        &mut self,
        v: &ast::VarDeclStmt,
        is_global: bool,
    ) -> Vec<VarDecl> {
        let span = self.span_of_node(v.syntax());
        // Typed-form `real x = 42` carries a leading TypeRef child
        // — capture it so the interpreter can coerce on assignment.
        let declared_ty = v
            .syntax()
            .children()
            .find(|n| n.kind() == SyntaxKind::TypeRef)
            .map(|n| leek_types::type_from_node(&n));
        // Walk children in source order; the pattern is:
        //   KwVar, [Type], Ident, [Eq, Expr], (Comma, Ident, [Eq, Expr])*
        // We pair each Ident with the next Expr only when an `=`
        // appeared between them, otherwise the init is None.
        // Two flavours of declaration:
        // - For lambda initializers (`var f = function() {...}`),
        //   declare `f` *before* lowering the body so the lambda's
        //   resolver can see its own binding (recursive closures).
        // - For non-lambda inits (`var count = count([1,2,3])`),
        //   declare AFTER lowering so the RHS still sees the outer
        //   scope (the local doesn't shadow itself in its own init).
        let mut decls: Vec<VarDecl> = Vec::new();
        let mut pending_ident: Option<SyntaxToken> = None;
        let mut just_saw_eq = false;
        for child in v.syntax().children_with_tokens() {
            match &child {
                rowan::NodeOrToken::Token(t) => match t.kind() {
                    SyntaxKind::Ident => {
                        if let Some(prev) = pending_ident.take() {
                            decls.push(self.declare_then_make(&prev, None, is_global, span));
                        }
                        pending_ident = Some(t.clone());
                        just_saw_eq = false;
                    }
                    SyntaxKind::Eq => {
                        just_saw_eq = true;
                    }
                    SyntaxKind::Comma => {
                        if !just_saw_eq && let Some(prev) = pending_ident.take() {
                            decls.push(self.declare_then_make(&prev, None, is_global, span));
                        }
                        just_saw_eq = false;
                    }
                    _ => {}
                },
                rowan::NodeOrToken::Node(n) => {
                    if just_saw_eq && let Some(e) = AstExpr::cast(n.clone()) {
                        // Lambda init: pre-declare so the
                        // lambda body sees its own binding.
                        let is_lambda = matches!(&e, AstExpr::Lambda(_));
                        if is_lambda {
                            if let Some(tok) = pending_ident.take() {
                                let name = tok.text().to_string();
                                let ident_span = self.span_of_token(&tok);
                                let def = self.declare_local(&name, ident_span, None);
                                let init = Some(self.lower_expr(&e));
                                decls.push(Self::var_decl_from(&tok, def, init, is_global, span));
                            }
                        } else if let Some(tok) = pending_ident.take() {
                            let init = Some(self.lower_expr(&e));
                            decls.push(self.declare_then_make(&tok, init, is_global, span));
                        }
                        just_saw_eq = false;
                    }
                }
            }
        }
        if let Some(prev) = pending_ident.take() {
            decls.push(self.declare_then_make(&prev, None, is_global, span));
        }
        // Attach the parsed declaration type to every declarator —
        // `real a, b = 1` types both `a` and `b` as real. Inferred
        // types from `var x = init` are handled downstream in MIR
        // (compound-assign coercion only) so we don't conflate
        // them with explicit annotations here.
        if let Some(ty) = declared_ty {
            for d in &mut decls {
                d.ty = Some(ty.clone());
                // For globals, the type lives on the `Def::Global`
                // record too (the interp reads it from there at
                // assignment-time to coerce).
                if d.is_global
                    && let Some(Def::Global(g)) = self.out.defs.get_mut(d.def.0 as usize)
                {
                    g.ty = Some(ty.clone());
                }
            }
        }
        decls
    }

    /// Declare a local from `ident` (allocating its `DefId`) and
    /// return a `VarDecl` bound to it. This is the "declare AFTER
    /// init" path — the local isn't visible while the init is
    /// being lowered, so `var x = x + 1` reads the outer `x`.
    pub(crate) fn declare_then_make(
        &mut self,
        ident: &SyntaxToken,
        init: Option<Expr>,
        is_global: bool,
        span: Span,
    ) -> VarDecl {
        let name = ident.text().to_string();
        let ident_span = self.span_of_token(ident);
        let def = if is_global {
            self.declare_global(&name, ident_span, None)
        } else {
            self.declare_local(&name, ident_span, None)
        };
        VarDecl {
            def,
            name,
            ty: None,
            init,
            is_global,
            span,
        }
    }

    /// Build a `VarDecl` from a token + a `DefId` that was already
    /// allocated/declared at the call site. Used by
    /// [`Self::lower_var_decls`], which pre-declares each name so
    /// recursive lambda initializers can resolve their own
    /// binding from inside the lambda body.
    pub(crate) fn var_decl_from(
        ident: &SyntaxToken,
        def: DefId,
        init: Option<Expr>,
        is_global: bool,
        span: Span,
    ) -> VarDecl {
        let name = ident.text().to_string();
        VarDecl {
            def,
            name,
            ty: None,
            init,
            is_global,
            span,
        }
    }

    pub(crate) fn lower_if(&mut self, i: &ast::IfStmt) -> Stmt {
        let span = self.span_of_node(i.syntax());
        let cond = self.lower_expr_or_null(i.condition(), span);
        let then_branch = self.lower_stmt_or_empty(i.then_branch(), span);
        let else_branch = i.else_branch().map(|s| Box::new(self.lower_stmt(&s)));
        Stmt::If(IfStmt {
            cond,
            then_branch,
            else_branch,
            span,
        })
    }

    pub(crate) fn lower_while(&mut self, w: &ast::WhileStmt) -> Stmt {
        let span = self.span_of_node(w.syntax());
        let cond = self.lower_expr_or_null(w.condition(), span);
        let body = self.lower_stmt_or_empty(w.body(), span);
        Stmt::While(WhileStmt { cond, body, span })
    }

    pub(crate) fn lower_do_while(&mut self, dw: &ast::DoWhileStmt, span: Span) -> Stmt {
        let body = self.lower_stmt_or_empty(dw.syntax().children().find_map(AstStmt::cast), span);
        let cond = self.lower_expr_or_null(dw.syntax().children().find_map(AstExpr::cast), span);
        Stmt::DoWhile(DoWhileStmt { body, cond, span })
    }

    pub(crate) fn lower_for(&mut self, f: &ast::ForStmt, span: Span) -> Stmt {
        self.push_scope();
        // Collect children in source order. The first stmt is init,
        // first/second expr is cond, second/third is step, last stmt
        // is body. Mirroring the AST's loose shape.
        let stmts: Vec<_> = f.syntax().children().filter_map(AstStmt::cast).collect();
        let exprs: Vec<_> = f.syntax().children().filter_map(AstExpr::cast).collect();
        let init = stmts.first().map(|s| Box::new(self.lower_stmt(s)));
        let cond = exprs.first().map(|e| self.lower_expr(e));
        let step = exprs.get(1).map(|e| self.lower_expr(e));
        let body = self.lower_stmt_or_empty(stmts.last().cloned(), span);
        self.pop_scope();
        Stmt::For(ForStmt {
            init,
            cond,
            step,
            body,
            span,
        })
    }

    pub(crate) fn lower_foreach(&mut self, fe: &ast::ForeachStmt, span: Span) -> Stmt {
        self.push_scope();
        // Walk tokens to find key/value bindings.
        let mut key: Option<ForeachBind> = None;
        let mut value: Option<ForeachBind> = None;
        let mut seen_in = false;
        let mut seen_colon = false;
        let mut pending_var = false;
        for el in fe.syntax().children_with_tokens() {
            let Some(t) = el.into_token() else { continue };
            match t.kind() {
                SyntaxKind::KwIn => seen_in = true,
                SyntaxKind::KwVar if !seen_in => pending_var = true,
                SyntaxKind::Colon if !seen_in => seen_colon = true,
                SyntaxKind::Ident if !seen_in => {
                    let nm = t.text().to_string();
                    let tspan = self.span_of_token(&t);
                    let is_new = pending_var;
                    let def = if is_new {
                        self.declare_local(&nm, tspan, None)
                    } else {
                        self.lookup_local(&nm).unwrap_or(DefId(0))
                    };
                    let bind = ForeachBind {
                        def,
                        name: nm,
                        is_new,
                        span: tspan,
                    };
                    if seen_colon {
                        // After the colon, the next binding is the
                        // value, and the earlier one was the key.
                        key = value.take();
                        value = Some(bind);
                    } else if value.is_none() {
                        value = Some(bind);
                    } else {
                        key = value.take();
                        value = Some(bind);
                    }
                    pending_var = false;
                }
                _ => {}
            }
        }
        let iter = self.lower_expr_or_null(fe.syntax().children().find_map(AstExpr::cast), span);
        let body = self.lower_stmt_or_empty(
            fe.syntax().children().filter_map(AstStmt::cast).last(),
            span,
        );
        self.pop_scope();
        Stmt::Foreach(ForeachStmt {
            key,
            value: value.unwrap_or(ForeachBind {
                def: DefId(0),
                name: String::new(),
                is_new: false,
                span,
            }),
            iter,
            body,
            span,
        })
    }

    pub(crate) fn lower_switch(&mut self, s: &ast::SwitchStmt, span: Span) -> Stmt {
        let discriminant =
            self.lower_expr_or_null(s.syntax().children().find_map(AstExpr::cast), span);
        let mut arms = Vec::new();
        for child in s.syntax().children() {
            if child.kind() == SyntaxKind::SwitchCase {
                let case = child
                    .children()
                    .find_map(AstExpr::cast)
                    .map(|e| self.lower_expr(&e));
                let mut body = Vec::new();
                for cc in child.children() {
                    if let Some(st) = AstStmt::cast(cc) {
                        body.push(self.lower_stmt(&st));
                    }
                }
                arms.push(SwitchArm { case, body });
            }
        }
        Stmt::Switch(SwitchStmt {
            discriminant,
            arms,
            span,
        })
    }
}
