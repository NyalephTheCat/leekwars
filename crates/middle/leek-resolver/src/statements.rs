//! Statement-level resolution: var decls, function bodies, class
//! bodies, blocks, control flow.

use leek_parser::ast::{
    AstNode, Block, ClassDecl, ClassField, ClassMethod, Expr, FnDecl, ForStmt, ForeachStmt, IfStmt,
    ImportStmt, Stmt, VarDeclStmt, WhileStmt,
};
use leek_syntax::SyntaxKind;

use crate::Resolver;
use crate::codes;
use crate::scope::{FnMeta, SymbolKind};
use crate::util::{
    collect_foreach_var_names, find_name_ref_in, first_ident, fn_arity, idents_after_keyword,
    lambda_arity, method_name,
};

/// True for statements that unconditionally exit their enclosing
/// block: `return`, `break`, `continue`, `throw`. Used to detect
/// dead-code-after-terminator.
pub(crate) fn is_block_terminator(stmt: &Stmt) -> bool {
    match stmt {
        // `return? expr` is a soft return — flow can continue if the
        // condition isn't met, so don't treat it as a terminator.
        Stmt::Return(r) => !r
            .syntax()
            .children_with_tokens()
            .filter_map(rowan::NodeOrToken::into_token)
            .any(|t| t.kind() == SyntaxKind::Question),
        Stmt::Break(_) | Stmt::Continue(_) => true,
        _ => false,
    }
}

impl Resolver {
    /// Pre-declare top-level names that participate in forward
    /// references. Functions and classes are hoisted via
    /// [`Resolver::declare_fn`] and
    /// [`Resolver::declare_class`](crate::Resolver::declare_class);
    /// vars/globals are NOT hoisted — they're declared in source
    /// order during the second pass so the `name_in_outer_scope`
    /// check sees the right state.
    // Intentional no-op (see above), but kept as a `&mut self` method for
    // symmetry with `declare_fn`/`declare_class` and a future hoisting hook.
    #[allow(clippy::unused_self)]
    pub(crate) fn declare_top_stmt(&mut self, _stmt: &Stmt) {}

    pub(crate) fn declare_fn(&mut self, decl: &FnDecl) {
        let Some(name) = crate::util::first_ident_after(decl.syntax(), SyntaxKind::KwFunction)
        else {
            return;
        };
        let nm = name.text().to_string();
        let (_, redecl) = self.declare(&name, SymbolKind::Function);
        let overloads = self.opts.experimental_overloads;
        if redecl && !overloads {
            self.err(
                codes::REDECLARED_SYMBOL,
                self.span_of(&name),
                format!("`{nm}` is already declared"),
            );
        }
        self.check_param_defaults(decl.syntax());
        let (min_args, max_args) = fn_arity(decl.syntax());
        if redecl && overloads {
            // Overload: widen the recorded arity to cover this
            // declaration too, so a call matching any signature passes.
            let entry = self.fn_meta.entry(nm).or_insert(FnMeta {
                min_args,
                max_args,
                min_version: 1,
            });
            entry.min_args = entry.min_args.min(min_args);
            entry.max_args = entry.max_args.max(max_args);
        } else {
            self.fn_meta.insert(
                nm,
                FnMeta {
                    min_args,
                    max_args,
                    min_version: 1,
                },
            );
        }
    }

    pub(crate) fn declare_var_names(&mut self, decl: &VarDeclStmt, kind: SymbolKind) {
        for ident in idents_after_keyword(decl.syntax()) {
            let nm = ident.text().to_string();
            // Shadowing an outer-scope binding is a hard error in
            // Leekscript (per upstream `VARIABLE_NAME_UNAVAILABLE`).
            // Builtins don't count — we already allow user code to
            // shadow stdlib names like `search`.
            if self.name_in_outer_scope(&nm) {
                self.err(
                    codes::VARIABLE_NAME_UNAVAILABLE,
                    self.span_of(&ident),
                    format!("`{nm}` shadows an outer-scope binding"),
                );
            }
            let (_, redecl) = self.declare(&ident, kind);
            if redecl {
                self.err(
                    codes::REDECLARED_SYMBOL,
                    self.span_of(&ident),
                    format!("`{nm}` is already declared"),
                );
            }
        }
    }

    pub(crate) fn resolve_fn_body(&mut self, decl: &FnDecl) {
        // Loop-depth doesn't carry across function boundaries.
        let saved_loop = std::mem::take(&mut self.loop_depth);
        let saved_breakable = std::mem::take(&mut self.breakable_depth);
        self.push_function_scope();
        if let Some(params) = decl
            .syntax()
            .children()
            .find(|n| n.kind() == SyntaxKind::ParamList)
        {
            for p in params.children() {
                if p.kind() != SyntaxKind::Param {
                    continue;
                }
                if let Some(ident) = first_ident(&p) {
                    self.declare_param(&ident);
                }
            }
        }
        if let Some(body) = decl.syntax().children().find_map(Block::cast) {
            self.resolve_block_body(&body);
        }
        self.pop_scope();
        self.loop_depth = saved_loop;
        self.breakable_depth = saved_breakable;
    }

    pub(crate) fn resolve_class(&mut self, decl: &ClassDecl) {
        // Only enable the strict in-class unknown-name check when the
        // class doesn't inherit — we don't track parent fields yet.
        let extends = decl
            .syntax()
            .children_with_tokens()
            .filter_map(rowan::NodeOrToken::into_token)
            .any(|t| t.kind() == SyntaxKind::KwExtends);
        let saved = self.in_class;
        let saved_current = self.current_class.clone();
        self.in_class = !extends;
        self.current_class = crate::util::first_ident_after(decl.syntax(), SyntaxKind::KwClass)
            .map(|t| t.text().to_string());
        self.push_scope();
        if let Some(body) = decl
            .syntax()
            .children()
            .find(|n| n.kind() == SyntaxKind::ClassBody)
        {
            // Pre-declare fields and methods in the class scope so
            // methods can call each other and reference fields via
            // plain identifiers.
            for member in body.children() {
                match member.kind() {
                    SyntaxKind::ClassField => {
                        if let Some(ident) = ClassField::cast(member.clone())
                            .and_then(|f| crate::util::field_name(&f))
                        {
                            let _ = self.declare(&ident, SymbolKind::Field);
                        }
                    }
                    SyntaxKind::ClassMethod => {
                        if let Some(ident) =
                            ClassMethod::cast(member.clone()).and_then(|m| method_name(&m))
                        {
                            let name_text = ident.text().to_string();
                            let _ = self.declare(&ident, SymbolKind::Function);
                            // Drop any builtin arity entry — the
                            // method may be overloaded with different
                            // arities than the global builtin.
                            self.fn_meta.remove(name_text.as_str());
                        }
                    }
                    _ => {}
                }
            }
            // Walk member bodies.
            for member in body.children() {
                if let Some(m) = ClassMethod::cast(member.clone()) {
                    self.resolve_class_method_body(&m);
                } else if member.kind() == SyntaxKind::ClassConstructor {
                    self.resolve_class_constructor_body(&member);
                } else if let Some(f) = ClassField::cast(member.clone())
                    && let Some(init) = f.syntax().children().find_map(Expr::cast)
                {
                    self.resolve_expr(&init);
                }
            }
        }
        self.pop_scope();
        self.in_class = saved;
        self.current_class = saved_current;
    }

    fn resolve_class_method_body(&mut self, m: &ClassMethod) {
        let saved_loop = std::mem::take(&mut self.loop_depth);
        let saved_breakable = std::mem::take(&mut self.breakable_depth);
        self.push_function_scope();
        if let Some(params) = m
            .syntax()
            .children()
            .find(|n| n.kind() == SyntaxKind::ParamList)
        {
            for p in params.children() {
                if p.kind() != SyntaxKind::Param {
                    continue;
                }
                if let Some(ident) = first_ident(&p) {
                    self.declare_param(&ident);
                }
            }
        }
        if let Some(body) = m.syntax().children().find_map(Block::cast) {
            self.resolve_block_body(&body);
        }
        self.pop_scope();
        self.loop_depth = saved_loop;
        self.breakable_depth = saved_breakable;
    }

    fn resolve_class_constructor_body(&mut self, ctor: &leek_syntax::SyntaxNode) {
        let saved_loop = std::mem::take(&mut self.loop_depth);
        let saved_breakable = std::mem::take(&mut self.breakable_depth);
        let saved_ctor = std::mem::replace(&mut self.in_constructor, true);
        self.push_function_scope();
        if let Some(params) = ctor.children().find(|n| n.kind() == SyntaxKind::ParamList) {
            for p in params.children() {
                if p.kind() != SyntaxKind::Param {
                    continue;
                }
                if let Some(ident) = first_ident(&p) {
                    self.declare_param(&ident);
                }
            }
        }
        if let Some(body) = ctor.children().find_map(Block::cast) {
            self.resolve_block_body(&body);
        }
        self.pop_scope();
        self.loop_depth = saved_loop;
        self.breakable_depth = saved_breakable;
        self.in_constructor = saved_ctor;
    }

    pub(crate) fn resolve_block_body(&mut self, block: &Block) {
        self.push_scope();
        let mut terminated_at: Option<leek_span::Span> = None;
        for stmt in block.stmts() {
            self.resolve_stmt(&stmt);
            if terminated_at.is_none() && is_block_terminator(&stmt) {
                terminated_at = Some(self.node_span(stmt.syntax()));
            } else if terminated_at.is_some() {
                // Anything after a terminator in the same block is
                // dead code — upstream emits this as an error rather
                // than a warning.
                self.err(
                    codes::CANT_ADD_INSTRUCTION_AFTER_BREAK,
                    self.node_span(stmt.syntax()),
                    "cannot add instruction after a terminator (return/break/continue)".to_string(),
                );
                // One diagnostic per block — bail out so we don't
                // flood on long dead tails.
                break;
            }
        }
        self.pop_scope();
    }

    pub(crate) fn resolve_stmt(&mut self, stmt: &Stmt) {
        match stmt {
            Stmt::VarDecl(v) => self.resolve_var_decl(v),
            Stmt::Return(r) => {
                if let Some(e) = r.value() {
                    self.resolve_expr(&e);
                }
            }
            Stmt::Expr(e) => {
                if let Some(inner) = e.expr() {
                    self.resolve_expr(&inner);
                }
            }
            Stmt::If(i) => self.resolve_if(i),
            Stmt::While(w) => {
                self.loop_depth += 1;
                self.breakable_depth += 1;
                self.resolve_while(w);
                self.loop_depth -= 1;
                self.breakable_depth -= 1;
            }
            Stmt::DoWhile(dw) => {
                self.loop_depth += 1;
                self.breakable_depth += 1;
                if let Some(body) = dw.syntax().children().find_map(Stmt::cast) {
                    self.resolve_stmt(&body);
                }
                if let Some(cond) = dw.syntax().children().find_map(Expr::cast) {
                    self.resolve_expr(&cond);
                }
                self.loop_depth -= 1;
                self.breakable_depth -= 1;
            }
            Stmt::For(f) => {
                self.loop_depth += 1;
                self.breakable_depth += 1;
                self.resolve_for(f);
                self.loop_depth -= 1;
                self.breakable_depth -= 1;
            }
            Stmt::Foreach(fe) => {
                self.loop_depth += 1;
                self.breakable_depth += 1;
                self.resolve_foreach(fe);
                self.loop_depth -= 1;
                self.breakable_depth -= 1;
            }
            Stmt::Break(b) => {
                if self.breakable_depth == 0 {
                    self.err(
                        codes::BREAK_OR_CONTINUE_OUT_OF_LOOP,
                        self.node_span(b.syntax()),
                        "`break` used outside a loop or switch".to_string(),
                    );
                }
            }
            Stmt::Continue(c) => {
                if self.loop_depth == 0 {
                    self.err(
                        codes::BREAK_OR_CONTINUE_OUT_OF_LOOP,
                        self.node_span(c.syntax()),
                        "`continue` used outside a loop".to_string(),
                    );
                }
            }
            Stmt::Switch(s) => {
                self.breakable_depth += 1;
                for child in s.syntax().children() {
                    if let Some(e) = Expr::cast(child.clone()) {
                        self.resolve_expr(&e);
                    } else if child.kind() == SyntaxKind::SwitchCase {
                        for cc in child.children() {
                            if let Some(e) = Expr::cast(cc.clone()) {
                                self.resolve_expr(&e);
                            } else if let Some(s) = Stmt::cast(cc) {
                                self.resolve_stmt(&s);
                            }
                        }
                    }
                }
                self.breakable_depth -= 1;
            }
            Stmt::Include(_) => {}
            Stmt::Import(i) => self.resolve_import(i),
            Stmt::Block(b) => self.resolve_block_body(b),
        }
    }

    fn resolve_import(&mut self, import: &ImportStmt) {
        if !self.opts.experimental_imports {
            self.err(
                codes::AI_NOT_EXISTING,
                self.node_span(import.syntax()),
                "`import` is experimental; enable `// @experimental:imports`".to_string(),
            );
            return;
        }

        let Some(path) = import_path(import) else {
            self.err(
                codes::AI_NOT_EXISTING,
                self.node_span(import.syntax()),
                "invalid import statement".to_string(),
            );
            return;
        };

        let Some(symbols) = crate::builtins::library_symbols(&path) else {
            self.err(
                codes::AI_NOT_EXISTING,
                self.node_span(import.syntax()),
                format!("builtin library `{path}` does not exist"),
            );
            return;
        };

        for name in symbols {
            self.imported_library_symbols.insert(name);
        }
    }

    /// Resolve a var/global declaration: walk initializers, record
    /// lambda arities and class types, then declare the names.
    fn resolve_var_decl(&mut self, v: &VarDeclStmt) {
        // Recursive-var-lambda support: when the init is a lambda
        // and there's exactly one declared name, pre-declare it
        // so the lambda body's references to that name resolve to
        // the var (not to a builtin / "unresolved"). The HIR
        // lowerer does the same dance — keeping the two in sync
        // means lint reports the inner reference, hover finds
        // it, and the Java backend can outline the closure
        // correctly.
        let init_node = v.syntax().children().find_map(Expr::cast);
        let init_is_lambda = matches!(&init_node, Some(Expr::Lambda(_)));
        let names = idents_after_keyword(v.syntax());
        let pre_declared = if init_is_lambda && names.len() == 1 {
            let name_tok = &names[0];
            let nm = name_tok.text().to_string();
            if self.name_in_outer_scope(&nm) {
                false
            } else {
                let (_, _) = self.declare(name_tok, SymbolKind::Local);
                true
            }
        } else {
            false
        };

        // Resolve initializer expressions (lookups happen in
        // the outer scope, possibly with the var name now
        // pre-declared for the lambda-init case above).
        for child in v.syntax().children() {
            if let Some(e) = Expr::cast(child) {
                self.resolve_expr(&e);
            }
        }
        // The pre-declared lambda case has already declared the
        // name; signal `declare_var_names` below to skip it.
        let skip_declare = pre_declared;
        // If the var binds a lambda, record its arity under the var's
        // name. If the var binds a `new C(...)` (or constructor
        // shorthand), track the class type for later checks.
        let init = v.syntax().children().find_map(Expr::cast);
        let lambda_arity = init.as_ref().and_then(|e| {
            if let Expr::Lambda(l) = e {
                Some(lambda_arity(l.syntax()))
            } else {
                None
            }
        });
        let has_explicit_type = v
            .syntax()
            .children()
            .any(|n| n.kind() == SyntaxKind::TypeRef);
        let new_class = init.as_ref().and_then(|e| match e {
            Expr::New(n) => first_ident(n.syntax()).map(|t| t.text().to_string()),
            // `A a = A(...)` — constructor-shorthand call also
            // produces an instance of A.
            Expr::Call(c) => match c.callee() {
                Some(Expr::Name(name)) => name.ident().and_then(|t| {
                    let txt = t.text().to_string();
                    if matches!(self.lookup(&txt), Some(SymbolKind::Class)) {
                        Some(txt)
                    } else {
                        None
                    }
                }),
                _ => None,
            },
            _ => None,
        });
        if !skip_declare {
            self.declare_var_names(v, SymbolKind::Local);
        }
        if let Some(name) = idents_after_keyword(v.syntax()).first() {
            if let Some((min_args, max_args)) = lambda_arity {
                self.fn_meta.insert(
                    name.text().to_string(),
                    FnMeta {
                        min_args,
                        max_args,
                        min_version: 1,
                    },
                );
            }
            if let Some(cls) = new_class {
                self.set_var_class(name.text().to_string(), cls, has_explicit_type);
            }
        }
    }

    fn resolve_if(&mut self, i: &IfStmt) {
        if let Some(cond) = i.condition() {
            self.resolve_expr(&cond);
        }
        if let Some(t) = i.then_branch() {
            self.resolve_stmt(&t);
        }
        if let Some(e) = i.else_branch() {
            self.resolve_stmt(&e);
        }
    }

    fn resolve_while(&mut self, w: &WhileStmt) {
        if let Some(cond) = w.condition() {
            self.resolve_expr(&cond);
        }
        if let Some(body) = w.body() {
            self.resolve_stmt(&body);
        }
    }

    fn resolve_for(&mut self, f: &ForStmt) {
        self.push_scope();
        // The init/cond/step/body children are interleaved with
        // tokens. Walk all child nodes in order.
        for child in f.syntax().children() {
            if let Some(s) = Stmt::cast(child.clone()) {
                self.resolve_stmt(&s);
            } else if let Some(e) = Expr::cast(child) {
                self.resolve_expr(&e);
            }
        }
        self.pop_scope();
    }

    fn resolve_foreach(&mut self, fe: &ForeachStmt) {
        self.push_scope();
        // Collect the names of any new loop bindings (those preceded
        // by `var`) so we can check the iterable expression doesn't
        // reference them before they exist.
        let loop_var_names = collect_foreach_var_names(fe);
        // Resolve the iterable expression first.
        if let Some(iter) = fe.syntax().children().find_map(Expr::cast) {
            self.resolve_expr(&iter);
            // `for (var x in x) {}` / `for (var x in [x]) {}` — the
            // container shouldn't reference the about-to-be-declared
            // loop variable. We scan the iter subtree for any
            // NameRef matching a fresh loop var; if no outer-scope
            // binding shadows it, emit UNKNOWN_VARIABLE.
            for name in &loop_var_names {
                if self.lookup(name).is_some() {
                    continue;
                }
                if let Some(tok) = find_name_ref_in(&iter, name) {
                    self.err(
                        codes::UNKNOWN_VARIABLE,
                        self.span_of(&tok),
                        format!("unknown variable or function `{name}`"),
                    );
                    break;
                }
            }
        }
        // Declare loop bindings. The `var` keyword distinguishes a
        // new declaration from a reuse: `for (x in arr)` reuses an
        // outer `x`, while `for (var x in arr)` declares a new one
        // (and shadowing checks apply to the latter).
        let mut seen_in = false;
        let mut pending_var = false;
        for el in fe.syntax().children_with_tokens() {
            match el {
                rowan::NodeOrToken::Token(t) => match t.kind() {
                    SyntaxKind::KwIn => seen_in = true,
                    SyntaxKind::KwVar if !seen_in => pending_var = true,
                    SyntaxKind::Colon if !seen_in => {
                        // Resets between the key and value binding.
                        pending_var = false;
                    }
                    SyntaxKind::Ident if !seen_in => {
                        if pending_var {
                            if self.name_in_outer_scope(t.text()) {
                                self.err(
                                    codes::VARIABLE_NAME_UNAVAILABLE,
                                    self.span_of(&t),
                                    format!("`{}` shadows an outer-scope binding", t.text()),
                                );
                            }
                            let _ = self.declare(&t, SymbolKind::Local);
                            pending_var = false;
                        }
                    }
                    _ => {}
                },
                rowan::NodeOrToken::Node(_) => {}
            }
        }
        // Body.
        if let Some(body) = fe.syntax().children().filter_map(Stmt::cast).last() {
            self.resolve_stmt(&body);
        }
        self.pop_scope();
    }
}

fn import_path(stmt: &ImportStmt) -> Option<String> {
    if let Some(tok) = stmt
        .syntax()
        .children_with_tokens()
        .filter_map(rowan::NodeOrToken::into_token)
        .find(|t| t.kind() == SyntaxKind::StringLiteral)
    {
        let raw = tok.text();
        if raw.len() >= 2 {
            return Some(raw[1..raw.len() - 1].to_string());
        }
    }

    let mut parts: Vec<String> = Vec::new();
    for tok in stmt
        .syntax()
        .children_with_tokens()
        .filter_map(rowan::NodeOrToken::into_token)
    {
        match tok.kind() {
            SyntaxKind::Ident => parts.push(tok.text().to_string()),
            SyntaxKind::KwImport
            | SyntaxKind::Dot
            | SyntaxKind::LParen
            | SyntaxKind::RParen
            | SyntaxKind::Semicolon => {}
            k if k.is_trivia() => {}
            _ => break,
        }
    }
    if parts.is_empty() {
        None
    } else {
        Some(parts.join("."))
    }
}
