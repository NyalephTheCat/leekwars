use super::prelude::*;

impl Checker {
    pub(crate) fn check_stmt(&mut self, stmt: &Stmt) {
        match stmt {
            Stmt::VarDecl(v) => self.check_var_decl(v),
            Stmt::Return(r) => self.check_return(r),
            Stmt::Expr(e) => {
                if let Some(inner) = e.expr() {
                    self.infer_expr(&inner);
                }
            }
            Stmt::If(i) => self.check_if(i),
            Stmt::While(w) => self.check_while(w),
            Stmt::DoWhile(dw) => {
                if let Some(body) = dw.syntax().children().find_map(Stmt::cast) {
                    self.check_stmt(&body);
                }
                if let Some(cond) = dw.syntax().children().find_map(Expr::cast) {
                    self.infer_expr(&cond);
                }
            }
            Stmt::For(f) => {
                self.push_scope();
                for child in f.syntax().children() {
                    if let Some(s) = Stmt::cast(child.clone()) {
                        self.check_stmt(&s);
                    } else if let Some(e) = Expr::cast(child) {
                        self.infer_expr(&e);
                    }
                }
                self.pop_scope();
            }
            Stmt::Foreach(fe) => {
                self.push_scope();
                let iter_ty = fe
                    .syntax()
                    .children()
                    .find_map(Expr::cast)
                    .map_or(Type::Any, |iter| self.infer_expr(&iter));
                // Collect the binding idents in order (before `in`). Two idents
                // means `key : value`; one means just the value — mirrors the
                // HIR `lower_foreach` walk.
                let mut idents: Vec<String> = Vec::new();
                let mut seen_in = false;
                for el in fe.syntax().children_with_tokens() {
                    if let rowan::NodeOrToken::Token(t) = el {
                        match t.kind() {
                            SyntaxKind::KwIn => seen_in = true,
                            SyntaxKind::Ident if !seen_in => idents.push(t.text().to_string()),
                            _ => {}
                        }
                    }
                }
                // Type the value binding from the iterable's element type and
                // the key (if present) from its key/index type. Falls back to
                // `Any` for unknown iterables, preserving prior behaviour.
                match idents.as_slice() {
                    [value] => self.declare(value, foreach_element_type(&iter_ty)),
                    [key, value] => {
                        self.declare(key, foreach_key_type(&iter_ty));
                        self.declare(value, foreach_element_type(&iter_ty));
                    }
                    other => {
                        for nm in other {
                            self.declare(nm, Type::Any);
                        }
                    }
                }
                if let Some(body) = fe.syntax().children().filter_map(Stmt::cast).last() {
                    self.check_stmt(&body);
                }
                self.pop_scope();
            }
            Stmt::Switch(s) => {
                for child in s.syntax().children() {
                    if let Some(e) = Expr::cast(child.clone()) {
                        self.infer_expr(&e);
                    } else if child.kind() == SyntaxKind::SwitchCase {
                        for cc in child.children() {
                            if let Some(e) = Expr::cast(cc.clone()) {
                                self.infer_expr(&e);
                            } else if let Some(s) = Stmt::cast(cc) {
                                self.check_stmt(&s);
                            }
                        }
                    }
                }
            }
            Stmt::Block(b) => self.check_block(b),
            Stmt::Break(_) | Stmt::Continue(_) | Stmt::Include(_) | Stmt::Import(_) => {}
        }
    }

    pub(crate) fn check_var_decl(&mut self, v: &VarDeclStmt) {
        let init = v.syntax().children().find_map(Expr::cast);
        let init_ty = init.as_ref().map_or(Type::Any, |e| self.infer_expr(e));

        // Track `x = []` / `x = [:]` (empty-literal initializers)
        // for the strict-v4 index-assign check below.
        let init_is_empty_collection = init.as_ref().is_some_and(|e| {
            matches!(e, Expr::Array(a) if a.elements().next().is_none())
                || matches!(e, Expr::Map(m)
                    if m.syntax().children().find_map(Expr::cast).is_none())
        });

        let names: Vec<SyntaxToken> = v
            .syntax()
            .children_with_tokens()
            .filter_map(rowan::NodeOrToken::into_token)
            .filter(|t| t.kind() == SyntaxKind::Ident)
            .collect();

        // Two-mode declaration:
        // - Typed (`integer a = 2`): record the *declared* type from
        //   the TypeRef. Future reassignments are checked against it.
        // - Plain `var`/`global`: dynamic. We only record the type if
        //   the initializer is `null`, since that unlocks the
        //   ASSIGNMENT_INCOMPATIBLE_TYPE pattern for indexing or
        //   compound-op on a null-bound var.
        if let Some(type_ref) = v
            .syntax()
            .children()
            .find(|n| n.kind() == SyntaxKind::TypeRef)
        {
            let declared = type_from_node(&type_ref);
            for name in &names {
                self.declare(name.text(), declared.clone());
            }
        } else if matches!(init_ty, Type::Null) {
            // Plain `var x = null` — null-binding tracked regardless
            // of strict, since indexing and compound-assign checks
            // already gate themselves on strict mode.
            for name in &names {
                self.declare(name.text(), Type::Null);
            }
        } else if (self.opts.strict || self.opts.seed_library) && !matches!(init_ty, Type::Any) {
            // Under strict mode, even `var` declarations commit to their
            // initializer's type — reassigning to an incompatible type
            // errors. The LSP (`seed_library`) also commits the inferred
            // type so hover/member-access see it (e.g. `var u = a / b`
            // with real operands resolves `u` to `real`); the
            // reassignment-incompatibility diagnostic stays strict-gated,
            // so this only enriches inference, it doesn't add errors.
            for name in &names {
                self.declare(name.text(), init_ty.clone());
            }
        }
        let has_type_annotation = v
            .syntax()
            .children()
            .any(|n| n.kind() == SyntaxKind::TypeRef);
        if init_is_empty_collection
            && !has_type_annotation
            && self.opts.strict
            && self.version == Version::V4
        {
            for name in &names {
                self.empty_collection_vars.insert(name.text().to_string());
            }
        }
    }

    pub(crate) fn check_return(&mut self, r: &ReturnStmt) {
        let value_ty = r.value().map(|v| self.infer_expr(&v));
        if !self.opts.strict {
            return;
        }
        let Some(expected) = self.return_types.last().and_then(std::clone::Clone::clone) else {
            return;
        };
        // Find the `return` keyword token for span attribution.
        let span = r
            .syntax()
            .children_with_tokens()
            .filter_map(rowan::NodeOrToken::into_token)
            .find(|t| t.kind() == SyntaxKind::KwReturn)
            .map_or_else(
                || {
                    let rng = r.syntax().text_range();
                    Span::new(self.source, u32::from(rng.start()), u32::from(rng.end()))
                },
                |t| self.span_of(&t),
            );
        match (value_ty, &expected) {
            // Bare `return` from a function declared to return something.
            (None, Type::Void | Type::Any) => {}
            (None, _) => {
                self.err(
                    codes::INCOMPATIBLE_TYPE,
                    span,
                    format!(
                        "function returns {}, but `return` has no value",
                        type_name(&expected),
                    ),
                );
            }
            // `return value` from a void function.
            (Some(_), Type::Void) => {
                self.err(
                    codes::INCOMPATIBLE_TYPE,
                    span,
                    "cannot return a value from a void function".to_string(),
                );
            }
            (Some(actual), _) => {
                if !Type::assignable_to(&actual, &expected) {
                    self.err(
                        codes::INCOMPATIBLE_TYPE,
                        span,
                        format!(
                            "function returns {}, but value is {}",
                            type_name(&expected),
                            type_name(&actual),
                        ),
                    );
                }
            }
        }
    }

    pub(crate) fn check_if(&mut self, i: &IfStmt) {
        let (pos, neg) = match i.condition() {
            Some(cond) => {
                self.infer_expr(&cond);
                self.condition_narrowings(&cond)
            }
            None => (Vec::new(), Vec::new()),
        };
        if let Some(t) = i.then_branch() {
            self.push_scope();
            self.apply_narrowings(&pos);
            self.check_stmt(&t);
            self.pop_scope();
        }
        if let Some(e) = i.else_branch() {
            self.push_scope();
            self.apply_narrowings(&neg);
            self.check_stmt(&e);
            self.pop_scope();
        }
    }

    pub(crate) fn check_while(&mut self, w: &WhileStmt) {
        let pos = match w.condition() {
            Some(cond) => {
                self.infer_expr(&cond);
                self.condition_narrowings(&cond).0
            }
            None => Vec::new(),
        };
        if let Some(body) = w.body() {
            self.push_scope();
            self.apply_narrowings(&pos);
            self.check_stmt(&body);
            self.pop_scope();
        }
    }
}

/// Element type yielded by `foreach (… in iterable)` for the value binding.
/// Unknown iterables fall back to `Any` (the prior, permissive behaviour).
fn foreach_element_type(iter_ty: &Type) -> Type {
    match iter_ty {
        Type::Array(t) | Type::Set(t) => (**t).clone(),
        Type::Map(_, v) => (**v).clone(),
        Type::Interval => Type::Integer,
        Type::Nullable(inner) => foreach_element_type(inner),
        _ => Type::Any,
    }
}

/// Key/index type for the optional `key` binding of a `foreach`: a map's key
/// type, or the integer index for arrays/sets/intervals.
fn foreach_key_type(iter_ty: &Type) -> Type {
    match iter_ty {
        Type::Map(k, _) => (**k).clone(),
        Type::Array(_) | Type::Set(_) | Type::Interval => Type::Integer,
        Type::Nullable(inner) => foreach_key_type(inner),
        _ => Type::Any,
    }
}
