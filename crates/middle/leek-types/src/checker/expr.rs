use super::prelude::*;

impl Checker {
    pub(crate) fn infer_expr(&mut self, e: &Expr) -> Type {
        let ty = self.infer_expr_kind(e);
        let range = e.syntax().text_range();
        self.typed_exprs.push(TypedExpr {
            span: Span::new(self.source, range.start().into(), range.end().into()),
            ty: ty.clone(),
        });
        ty
    }

    pub(crate) fn infer_expr_kind(&mut self, e: &Expr) -> Type {
        match e {
            Expr::Literal(lit) => {
                let Some(tok) = lit.token() else {
                    return Type::Any;
                };
                match tok.kind() {
                    // An `L`-suffixed integer literal (`2L`) is a big_integer.
                    SyntaxKind::IntLiteral if tok.text().ends_with('L') => Type::BigInteger,
                    SyntaxKind::IntLiteral => Type::Integer,
                    SyntaxKind::RealLiteral => Type::Real,
                    SyntaxKind::StringLiteral => Type::String,
                    SyntaxKind::KwTrue | SyntaxKind::KwFalse => Type::Boolean,
                    SyntaxKind::KwNull => Type::Null,
                    SyntaxKind::Lemniscate | SyntaxKind::Pi => Type::Real,
                    _ => Type::Any,
                }
            }
            Expr::Name(n) => {
                // `this` / `super` are keyword tokens, not Idents —
                // type them as instances (of the enclosing class and
                // its parent respectively) so hover and member lookups
                // see the right type.
                match name_ref_self_keyword(n) {
                    Some(SyntaxKind::KwThis) => {
                        return match self.current_class() {
                            Some(c) => Type::ClassInstance(c.to_string(), Vec::new()),
                            None => Type::Any,
                        };
                    }
                    Some(SyntaxKind::KwSuper) => {
                        return match self.current_super_class() {
                            Some(c) => Type::ClassInstance(c.to_string(), Vec::new()),
                            None => Type::Any,
                        };
                    }
                    _ => {}
                }
                if let Some(ident) = n.ident()
                    && let Some(t) = self.lookup(ident.text())
                {
                    t.clone()
                } else {
                    Type::Any
                }
            }
            Expr::Binary(b) => self.infer_binary(b),
            Expr::Unary(u) => {
                if let Some(o) = u.operand() {
                    self.infer_expr(&o);
                }
                Type::Any
            }
            Expr::Paren(p) => p.inner().map_or(Type::Any, |i| self.infer_expr(&i)),
            Expr::Array(a) => {
                let elems: Vec<Type> = a.elements().map(|el| self.infer_expr(&el)).collect();
                // Experimental tuple shapes: a literal's per-position
                // types are exactly known, so model short literals as
                // `Array[T0, T1, …]` — assignable position-wise to a
                // tuple annotation and member-wise to plain `Array<T>`
                // slots (`Type::assignable_to`).
                if self.opts.experimental_types
                    && !elems.is_empty()
                    && elems.len() <= MAX_INFERRED_TUPLE
                {
                    return Type::Tuple(elems);
                }
                // Infer a homogeneous element type so `[1, 2, 3][0]`
                // types as `integer`; mixed elements widen to `any`.
                let mut elem: Option<Type> = None;
                for t in elems {
                    elem = Some(match elem {
                        Some(prev) => unify_types(&prev, &t),
                        None => t,
                    });
                }
                Type::Array(Box::new(elem.unwrap_or(Type::Any)))
            }
            Expr::Map(m) => self.infer_map(m),
            Expr::Set(s) => {
                // Homogeneous element type, like array literals.
                let mut elem: Option<Type> = None;
                let join = |t: Type, elem: &mut Option<Type>| {
                    *elem = Some(match elem.take() {
                        Some(prev) => unify_types(&prev, &t),
                        None => t,
                    });
                };
                for child in s.syntax().children() {
                    if child.kind() == SyntaxKind::SetRangeElement {
                        // `start..end` expands to integers; still infer the
                        // bound expressions for their own diagnostics.
                        for bound in child.children().filter_map(Expr::cast) {
                            self.infer_expr(&bound);
                        }
                        join(Type::Integer, &mut elem);
                    } else if let Some(e) = Expr::cast(child) {
                        let t = self.infer_expr(&e);
                        join(t, &mut elem);
                    }
                }
                Type::Set(Box::new(elem.unwrap_or(Type::Any)))
            }
            Expr::Object(o) => {
                for child in o.syntax().children() {
                    if let Some(e) = Expr::cast(child) {
                        self.infer_expr(&e);
                    }
                }
                Type::Object
            }
            Expr::Lambda(l) => self.infer_lambda(l),
            Expr::New(n) => self.infer_new(n),
            Expr::Interval(i) => {
                for child in i.syntax().children() {
                    if let Some(e) = Expr::cast(child) {
                        self.infer_expr(&e);
                    }
                }
                Type::Interval
            }
            Expr::Call(c) => self.check_call(c),
            Expr::Field(f) => self.infer_field(f),
            Expr::Index(idx) => self.infer_index(idx),
            Expr::Ternary(t) => self.infer_ternary(t),
            // The rest fall back to Any with sub-expression recursion
            // for side effects.
            Expr::Cast(_) | Expr::Postfix(_) | Expr::Slice(_) => {
                for child in e.syntax().children() {
                    if let Some(sub) = Expr::cast(child) {
                        self.infer_expr(&sub);
                    }
                }
                Type::Any
            }
        }
    }

    /// Member access `recv.field`. A field resolves to its declared
    /// type; a method reference (no call) to a function value. Walks
    /// the inheritance chain. Static access (`Class.field`) is handled
    /// via the receiver naming a known class.
    pub(crate) fn infer_field(&mut self, f: &leek_parser::ast::FieldExpr) -> Type {
        let Some(base) = f.base() else {
            return Type::Any;
        };
        let Some((class, args)) = self.base_class_instance(&base) else {
            // Still infer the field name target's class is unknown; the
            // base was already inferred by `base_class_instance`.
            return Type::Any;
        };
        let Some(field) = f.field().map(|t| t.text().to_string()) else {
            return Type::Any;
        };
        // Generic class field: substitute the class's type variables with
        // the instance's bound arguments (`Box<integer>.value` → integer),
        // walking the inheritance chain and re-mapping type args at each
        // `extends Parent<…>` boundary.
        if let Some(t) = self.resolve_generic_field(&class, &args, &field)
            && !matches!(t, Type::Any)
        {
            return t;
        }
        if let Some(t) = self.lookup_field_type(&class, &field) {
            return t;
        }
        if let Some(ret) = self.lookup_method_return(&class, &field) {
            // `obj.method` (no call) is a first-class function value.
            // Parameter types aren't tracked here (the method's arity
            // would require a separate lookup), so leave them empty.
            return match ret {
                Type::Any => Type::Function,
                r => Type::function_with(Vec::new(), r),
            };
        }
        Type::Any
    }

    /// Index access `base[i]`: element type for arrays, value type for
    /// maps, a one-char string for string indexing.
    pub(crate) fn infer_index(&mut self, idx: &leek_parser::ast::IndexExpr) -> Type {
        let base_ty = idx.base().map_or(Type::Any, |b| self.infer_expr(&b));
        if let Some(i) = idx.index() {
            self.infer_expr(&i);
        }
        match base_ty {
            Type::Array(el) => *el,
            // Tuple shape: a constant index picks that position's
            // exact type; a dynamic index joins all member types.
            Type::Tuple(members) => match index_literal_int(idx) {
                Some(i) => members.get(i).cloned().unwrap_or(Type::Any),
                None => members
                    .iter()
                    .fold(None, |acc: Option<Type>, m| {
                        Some(match acc {
                            Some(prev) => unify_types(&prev, m),
                            None => m.clone(),
                        })
                    })
                    .unwrap_or(Type::Any),
            },
            Type::Map(_, v) => *v,
            Type::String => Type::String,
            _ => Type::Any,
        }
    }

    /// Ternary `cond ? a : b`. Narrows inside each arm and unifies the
    /// two arm types.
    pub(crate) fn infer_ternary(&mut self, t: &leek_parser::ast::TernaryExpr) -> Type {
        let mut exprs = t.syntax().children().filter_map(Expr::cast);
        let cond = exprs.next();
        let then_e = exprs.next();
        let else_e = exprs.next();
        let (pos, neg) = match &cond {
            Some(c) => {
                self.infer_expr(c);
                self.condition_narrowings(c)
            }
            None => (Vec::new(), Vec::new()),
        };
        let then_ty = match then_e {
            Some(e) => {
                self.push_scope();
                self.apply_narrowings(&pos);
                let t = self.infer_expr(&e);
                self.pop_scope();
                t
            }
            None => Type::Any,
        };
        let else_ty = match else_e {
            Some(e) => {
                self.push_scope();
                self.apply_narrowings(&neg);
                let t = self.infer_expr(&e);
                self.pop_scope();
                t
            }
            None => Type::Any,
        };
        unify_types(&then_ty, &else_ty)
    }

    /// The class a member receiver denotes, plus any bound generic type
    /// arguments — its `ClassInstance` type (instances, `this`, `super`,
    /// typed vars) or, for static `Class.member`, the receiver naming a
    /// known class (no arguments). Infers `base` exactly once.
    pub(crate) fn base_class_instance(&mut self, base: &Expr) -> Option<(String, Vec<Type>)> {
        let ty = self.infer_expr(base);
        if let Some(name) = class_name_of_type(&ty) {
            return Some((name, crate::ty::instance_type_args(&ty)));
        }
        if let Expr::Name(nr) = base
            && let Some(id) = nr.ident()
            && (self.class_field_types.contains_key(id.text())
                || self.class_method_returns.contains_key(id.text()))
        {
            return Some((id.text().to_string(), Vec::new()));
        }
        None
    }

    pub(crate) fn infer_map(&mut self, m: &leek_parser::ast::MapExpr) -> Type {
        // Detect MAP_DUPLICATED_KEY at v4: literal keys with the same
        // canonical form repeated in a single map literal. v1-v3
        // silently overwrite.
        let mut seen_keys: std::collections::HashSet<String> = std::collections::HashSet::new();
        let mut first = true;
        let mut last_key_text: Option<String> = None;
        let mut last_key_span: Option<Span> = None;
        // Accumulate homogeneous key/value types (`["a": 1]` → Map<string, integer>).
        let mut key_ty: Option<Type> = None;
        let mut val_ty: Option<Type> = None;
        for child in m.syntax().children() {
            let Some(e) = Expr::cast(child) else { continue };
            let t = self.infer_expr(&e);
            if first {
                key_ty = Some(match key_ty {
                    Some(prev) => unify_types(&prev, &t),
                    None => t,
                });
                last_key_text = literal_key_canonical(&e);
                let r = e.syntax().text_range();
                last_key_span = Some(Span::new(
                    self.source,
                    u32::from(r.start()),
                    u32::from(r.end()),
                ));
                first = false;
            } else {
                val_ty = Some(match val_ty {
                    Some(prev) => unify_types(&prev, &t),
                    None => t,
                });
                // This child is a value — next loop iteration will
                // see a new key.
                if self.version == Version::V4
                    && let Some(key) = last_key_text.take()
                    && let Some(span) = last_key_span.take()
                    && !seen_keys.insert(key.clone())
                {
                    self.err(
                        codes::MAP_DUPLICATED_KEY,
                        span,
                        format!("duplicate map key `{key}`"),
                    );
                }
                first = true;
            }
        }
        Type::Map(
            Box::new(key_ty.unwrap_or(Type::Any)),
            Box::new(val_ty.unwrap_or(Type::Any)),
        )
    }

    pub(crate) fn infer_lambda(&mut self, l: &leek_parser::ast::LambdaExpr) -> Type {
        self.push_function();
        self.declare_params_as_any(l.syntax());
        // Parameter types (typed params keep their type; untyped → Any).
        let mut params = Vec::new();
        if let Some(pl) = l
            .syntax()
            .children()
            .find(|n| n.kind() == SyntaxKind::ParamList)
        {
            for p in pl.children().filter(|n| n.kind() == SyntaxKind::Param) {
                let t = p
                    .children()
                    .find(|n| n.kind() == SyntaxKind::TypeRef)
                    .map_or(Type::Any, |n| self.resolve_type_node(&n));
                params.push(t);
            }
        }
        // Walk the body; for an arrow-expression lambda (`x => expr`) the
        // body expression's type is the inferred return.
        let mut body_ret: Option<Type> = None;
        for child in l.syntax().children() {
            if let Some(b) = Block::cast(child.clone()) {
                self.check_block(&b);
            } else if let Some(e) = Expr::cast(child) {
                body_ret = Some(self.infer_expr(&e));
            }
        }
        self.pop_scope();
        // Explicit `=> R` annotation wins; otherwise fall back to the
        // arrow-body type.
        let ret = fn_return_type(l.syntax())
            .map(|t| self.substitute_aliases(t))
            .or(body_ret);
        match ret {
            Some(r) => Type::function_with(params, r),
            None if params.iter().all(|t| matches!(t, Type::Any)) => Type::Function,
            None => Type::function_with(params, Type::Any),
        }
    }

    pub(crate) fn infer_new(&mut self, n: &leek_parser::ast::NewExpr) -> Type {
        // Class name is the first Ident token.
        let class = n
            .syntax()
            .children_with_tokens()
            .filter_map(rowan::NodeOrToken::into_token)
            .find(|t| t.kind() == SyntaxKind::Ident)
            .map(|t| t.text().to_string());
        // Infer constructor argument types, in order.
        let mut arg_types = Vec::new();
        for child in n.syntax().children() {
            for sub in child.children() {
                if let Some(e) = Expr::cast(sub) {
                    arg_types.push(self.infer_expr(&e));
                }
            }
        }
        let Some(class) = class else {
            return Type::Any;
        };
        // For a generic class, bind its type variables by unifying the
        // constructor's parameter patterns against the argument types
        // (`new Box(5)` with `constructor(T v)` → `Box<integer>`).
        let args = match self.generic_classes.get(&class) {
            Some(info) if !info.type_params.is_empty() => {
                let mut bindings = std::collections::HashMap::new();
                crate::generic::solve(&info.ctor_params, &arg_types, &mut bindings);
                info.type_params
                    .iter()
                    .map(|p| bindings.get(p).cloned().unwrap_or(Type::Any))
                    .collect()
            }
            _ => Vec::new(),
        };
        Type::ClassInstance(class, args)
    }
}

/// The index expression as a constant non-negative integer literal,
/// when it is one (`t[1]` — not `t[i]` or `t[1 + 0]`). Used to pick
/// the exact member type out of a tuple shape.
fn index_literal_int(idx: &leek_parser::ast::IndexExpr) -> Option<usize> {
    let Some(Expr::Literal(lit)) = idx.index() else {
        return None;
    };
    let tok = lit.token()?;
    if tok.kind() != SyntaxKind::IntLiteral {
        return None;
    }
    tok.text().parse().ok()
}

/// When a `NameRef` is a bare self-keyword (`this` / `super`), return
/// that keyword's `SyntaxKind`; otherwise `None`. These hold a keyword
/// token rather than an `Ident`.
fn name_ref_self_keyword(n: &leek_parser::ast::NameRef) -> Option<SyntaxKind> {
    n.syntax()
        .children_with_tokens()
        .filter_map(rowan::NodeOrToken::into_token)
        .find(|t| !t.kind().is_trivia())
        .map(|t| t.kind())
        .filter(|k| matches!(k, SyntaxKind::KwThis | SyntaxKind::KwSuper))
}
