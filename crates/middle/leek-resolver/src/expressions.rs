//! Expression-level resolution: name references, calls, and the
//! big `resolve_expr` dispatch. Mutating side-effects flow through
//! the assignment / l-value / field-access checks in [`checks`].
//!
//! [`checks`]: super::checks

use leek_diagnostics::Diagnostic;
use leek_parser::ast::{AstNode, Block, CallExpr, Expr, NameRef};
use leek_syntax::{SyntaxKind, SyntaxNode, SyntaxToken, Version};

use crate::Resolver;
use crate::builtins;
use crate::codes;
use crate::scope::{FnMeta, SymbolKind};
use crate::util::{REMOVED_BUILTINS, first_ident, is_assignment_binary};

impl Resolver {
    pub(crate) fn resolve_expr(&mut self, expr: &Expr) {
        match expr {
            Expr::Name(n) => self.resolve_name_ref(n),
            Expr::Literal(_) => {}
            Expr::Binary(b) => self.resolve_binary(b),
            Expr::Unary(u) => {
                // `++x` / `--x` on a function name is CANNOT_REDEFINE_FUNCTION.
                if let (Some(op), Some(operand)) = (u.op(), u.operand())
                    && matches!(op.kind(), SyntaxKind::PlusPlus | SyntaxKind::MinusMinus)
                {
                    self.check_fn_increment(&operand);
                }
                if let Some(o) = u.operand() {
                    self.resolve_expr(&o);
                }
            }
            Expr::Postfix(p) => {
                // `x++` / `x--` on a function name is CANNOT_REDEFINE_FUNCTION.
                let op_kind = p
                    .syntax()
                    .children_with_tokens()
                    .filter_map(rowan::NodeOrToken::into_token)
                    .map(|t| t.kind())
                    .find(|k| {
                        matches!(
                            k,
                            SyntaxKind::PlusPlus | SyntaxKind::MinusMinus | SyntaxKind::Bang
                        )
                    });
                if let (Some(SyntaxKind::PlusPlus | SyntaxKind::MinusMinus), Some(operand)) =
                    (op_kind, p.syntax().children().find_map(Expr::cast))
                {
                    self.check_fn_increment(&operand);
                }
                for child in p.syntax().children() {
                    if let Some(e) = Expr::cast(child) {
                        self.resolve_expr(&e);
                    }
                }
            }
            Expr::Paren(p) => {
                if let Some(i) = p.inner() {
                    self.resolve_expr(&i);
                }
            }
            Expr::Call(c) => self.resolve_call(c),
            Expr::Array(a) => {
                for e in a.elements() {
                    self.resolve_expr(&e);
                }
            }
            Expr::Index(idx) => {
                if let Some(b) = idx.base() {
                    self.resolve_expr(&b);
                }
                if let Some(i) = idx.index() {
                    self.resolve_expr(&i);
                }
            }
            Expr::Field(f) => {
                self.check_field_access(f);
                if let Some(b) = f.base() {
                    self.resolve_expr(&b);
                }
            }
            // Compound literals: walk every child expression.
            Expr::Map(_)
            | Expr::Object(_)
            | Expr::Set(_)
            | Expr::Interval(_)
            | Expr::Slice(_)
            | Expr::Ternary(_)
            | Expr::Cast(_) => {
                for child in expr.syntax().children() {
                    if let Some(e) = Expr::cast(child) {
                        self.resolve_expr(&e);
                    }
                }
            }
            Expr::New(n) => self.resolve_new(n),
            Expr::Lambda(l) => self.resolve_lambda(l),
        }
    }

    fn resolve_binary(&mut self, b: &leek_parser::ast::BinaryExpr) {
        // Detect assignment to a name bound to a function, class, or
        // builtin. Earlier versions of Leekscript allowed reassigning
        // function names (functions were first-class values); v4
        // forbids this. Class assignment is always an error.
        if is_assignment_binary(b)
            && let Some(lhs) = b.lhs()
        {
            self.check_final_field_assignment(&lhs);
            self.check_non_lvalue_assignment(&lhs);
        }
        // Assignment to keyword-shaped LHS (`this = ...`,
        // `class = ...`, `super = ...`). These NameRefs hold a
        // keyword token rather than an Ident, so the dedicated
        // check below handles them first.
        if is_assignment_binary(b)
            && let Some(Expr::Name(n)) = b.lhs()
            && let Some(tok) = n
                .syntax()
                .children_with_tokens()
                .filter_map(rowan::NodeOrToken::into_token)
                .find(|t| !t.kind().is_trivia())
            && matches!(
                tok.kind(),
                SyntaxKind::KwThis | SyntaxKind::KwClass | SyntaxKind::KwSuper
            )
        {
            self.err(
                codes::CANT_ASSIGN_VALUE,
                self.span_of(&tok),
                format!("cannot assign to `{}`", tok.text()),
            );
        }
        if is_assignment_binary(b)
            && let Some(Expr::Name(n)) = b.lhs()
            && let Some(ident) = n.ident()
        {
            self.check_name_assignment(b, &ident);
        }
        if let Some(lhs) = b.lhs() {
            self.resolve_expr(&lhs);
        }
        if let Some(rhs) = b.rhs() {
            self.resolve_expr(&rhs);
        }
    }

    /// Subcase of [`resolve_binary`]: assignment whose LHS is a bare
    /// identifier. Catches reassigns of builtins/constants/classes/
    /// functions plus implicit `this.field` final-field writes.
    fn check_name_assignment(&mut self, b: &leek_parser::ast::BinaryExpr, ident: &SyntaxToken) {
        let name = ident.text().to_string();
        // Assignment to a known builtin constant is always an error
        // regardless of version — `PI = 12`, `INFINITY = 0`, etc.
        if builtins::is_builtin_constant(&name) {
            self.err(
                codes::CANT_ASSIGN_VALUE,
                self.span_of(ident),
                format!("cannot assign to constant `{name}`"),
            );
        }
        // Inside a class method (not constructor), a bare assignment
        // to a name that matches a final field is an implicit
        // `this.field = …`.
        if self.in_class
            && !self.in_constructor
            && let Some(class_name) = self.current_class.clone()
            && let Some(finals) = self.class_final_fields.get(&class_name)
            && finals.contains(&name)
        {
            self.err(
                codes::CANNOT_ASSIGN_FINAL_FIELD,
                self.span_of(ident),
                format!("cannot assign to final field `{name}`"),
            );
        }
        if let Some(kind) = self.lookup(&name) {
            // Compound assignment (`f += 1`, `abs *= 2`) requires
            // reading the prior value, which on a function name is
            // nonsense — error at all versions. Plain `f = 1` is
            // only banned at v4, where functions stop being
            // first-class values reassignable as variables.
            let compound = b.op().is_some_and(|o| o.kind() != SyntaxKind::Eq);
            match kind {
                SymbolKind::Function | SymbolKind::Builtin
                    if compound || self.version >= Version::V4 =>
                {
                    self.err(
                        codes::CANNOT_REDEFINE_FUNCTION,
                        self.span_of(ident),
                        format!("cannot reassign function `{name}`"),
                    );
                }
                SymbolKind::Class => {
                    self.err(
                        codes::CANT_ASSIGN_VALUE,
                        self.span_of(ident),
                        format!("cannot assign to class `{name}`"),
                    );
                }
                _ => {}
            }
        }
        // Whatever the disposition, a reassignment makes future
        // arity assumptions invalid — drop the metadata so
        // subsequent calls don't false-fire.
        self.fn_meta.remove(&name);
        // Record the reassignment so `resolve_name_call` skips the
        // BUILTIN_FN_META fallback. Upstream allows
        // `cos = function(x, y, z) {…}; cos(1, 2, 3)` (the
        // user-bound value overrides the builtin's arity).
        self.reassigned_names.insert(name);
    }

    fn resolve_new(&mut self, n: &leek_parser::ast::NewExpr) {
        // Class identifier is the first Ident token.
        if let Some(class_tok) = n
            .syntax()
            .children_with_tokens()
            .filter_map(rowan::NodeOrToken::into_token)
            .find(|t| t.kind() == SyntaxKind::Ident)
        {
            let class_name = class_tok.text().to_string();
            // Anchor a reference from the class name in `new Cat(...)`
            // to the class declaration for hover / go-to-def.
            self.record_class_ref(&class_tok, &class_name);
            if Some(&class_name) != self.current_class.as_ref() {
                if self.class_private_constructor.contains(&class_name) {
                    self.err(
                        codes::PRIVATE_CONSTRUCTOR,
                        self.span_of(&class_tok),
                        format!("constructor of class `{class_name}` is private",),
                    );
                } else if self.class_protected_constructor.contains(&class_name) {
                    self.err(
                        codes::PROTECTED_CONSTRUCTOR,
                        self.span_of(&class_tok),
                        format!("constructor of class `{class_name}` is protected",),
                    );
                }
            }
        }
        for child in n.syntax().children() {
            if let Some(e) = Expr::cast(child) {
                self.resolve_expr(&e);
            }
        }
    }

    fn resolve_lambda(&mut self, l: &leek_parser::ast::LambdaExpr) {
        // Lambdas introduce a new scope with their params. They're
        // closure boundaries — shadowing across them is allowed,
        // and the loop-depth context doesn't carry through.
        let saved_loop = std::mem::take(&mut self.loop_depth);
        let saved_breakable = std::mem::take(&mut self.breakable_depth);
        self.push_function_scope();
        if let Some(params) = l
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
        // Body can be a block or a single expression.
        for child in l.syntax().children() {
            if let Some(b) = Block::cast(child.clone()) {
                self.resolve_block_body(&b);
            } else if let Some(e) = Expr::cast(child) {
                self.resolve_expr(&e);
            }
        }
        self.pop_scope();
        self.loop_depth = saved_loop;
        self.breakable_depth = saved_breakable;
    }

    // ---- Calls ----

    pub(crate) fn err_invalid_arity(
        &mut self,
        tok: &SyntaxToken,
        class_name: &str,
        method: &str,
        min: u8,
        max: u8,
        arg_count: u8,
    ) {
        let expected = format_arity(min, max);
        self.err(
            codes::INVALID_PARAMETER_COUNT,
            self.span_of(tok),
            format!("`{class_name}.{method}` expects {expected} args; got {arg_count}",),
        );
    }

    pub(crate) fn resolve_call(&mut self, c: &CallExpr) {
        let arg_count =
            u8::try_from(c.arg_list().map_or(0, |al| al.args().count())).unwrap_or(u8::MAX);

        // `super(...)` inside a subclass constructor calls the parent
        // class constructor. If the parent's ctor is private, error.
        if let Some(callee) = c.callee()
            && let Expr::Name(n) = &callee
            && let Some(super_tok) = n
                .syntax()
                .children_with_tokens()
                .filter_map(rowan::NodeOrToken::into_token)
                .find(|t| t.kind() == SyntaxKind::KwSuper)
            && let Some(this_class) = self.current_class.clone()
            && let Some(parent) = self.class_parent.get(&this_class).cloned()
        {
            if self.class_private_constructor.contains(&parent) {
                self.err(
                    codes::PRIVATE_CONSTRUCTOR,
                    self.span_of(&super_tok),
                    format!("constructor of class `{parent}` is private"),
                );
            } else if self.class_protected_constructor.contains(&parent) {
                // Calling a protected ctor from a subclass is *allowed*.
                // No diagnostic here — the privacy check inside `new`
                // covers the from-outside case.
            }
        }

        // Bare-identifier callees aren't strictly *resolved* at this
        // stage (Leekscript dispatches unknown calls at runtime) but
        // if the name appears in our function-metadata table we
        // still check arity and version-availability.
        if let Some(callee) = c.callee() {
            match &callee {
                Expr::Name(n) => self.resolve_name_call(n, arg_count),
                Expr::Field(f) => self.resolve_field_call(f, arg_count, &callee),
                _ => self.resolve_expr(&callee),
            }
        }
        if let Some(args) = c.arg_list() {
            for a in args.args() {
                self.resolve_expr(&a);
            }
        }
    }

    fn resolve_name_call(&mut self, n: &leek_parser::ast::NameRef, arg_count: u8) {
        // `super(...)` / `this(...)` callees are keyword tokens — anchor
        // their class reference before the Ident-only fast path below.
        self.record_self_keyword_ref(n);
        let Some(ident) = n.ident() else { return };
        // Register the callee as a reference so LSP features
        // (hover, go-to-def, find-references) pick it up — same
        // record we'd make if the name appeared bare via
        // `resolve_name_ref`.
        self.record_ref(&ident);
        let name = ident.text().to_string();
        // Check the "removed builtins" list — these are functions
        // that existed in v1-v3 (or so) but were taken out at a
        // later version.
        if let Some(&(_, removed_at)) = REMOVED_BUILTINS.iter().find(|(n, _)| *n == name.as_str())
            && (self.version as u8) >= removed_at
        {
            self.err(
                codes::REMOVED_FUNCTION,
                self.span_of(&ident),
                format!("`{name}` was removed in v{removed_at}"),
            );
        }
        // User-defined functions (declared during pass 1) take
        // precedence; otherwise consult the static builtin metadata
        // — UNLESS the user has reassigned this name (`cos = f`)
        // OR we're inside a class method that has a same-named
        // overload accepting the current arity. In both cases the
        // builtin's arity no longer applies.
        let user_meta = self.fn_meta.get(&name).copied();
        let class_method_matches_arity = self
            .current_class
            .as_ref()
            .and_then(|c| self.class_method_arities.get(c))
            .and_then(|m| m.get(&name))
            .is_some_and(|&(min, max)| arg_count >= min && arg_count <= max);
        let meta = user_meta.or_else(|| {
            if self.reassigned_names.contains(&name) || class_method_matches_arity {
                None
            } else {
                crate::scope::BUILTIN_FN_META
                    .get(name.as_str())
                    .copied()
                    .or_else(|| {
                        builtins::builtin_fn_meta(&name).map(|(min_args, max_args, min_version)| {
                            FnMeta {
                                min_args,
                                max_args,
                                min_version,
                            }
                        })
                    })
            }
        });
        if let Some(meta) = meta {
            let is_builtin = matches!(self.lookup(&name), Some(SymbolKind::Builtin));
            self.check_call_meta(&ident, &name, meta, arg_count, is_builtin);
        }
        // Bare-name call inside a class method that resolves to
        // another method on the same class — `class A { a() { b() } b(x) {} }`.
        // Treat it like `this.b()` for arity checks.
        //
        // Class methods get declared as Function in the class scope,
        // so `lookup` returns Some(Function). We prefer the class-
        // method arity over any same-named user global, but builtins
        // (including ones shadowed by a same-named class method)
        // win — a bare `sqrt(x)` inside a class method targets the
        // global builtin, not the class's `sqrt()`.
        let shadows_builtin = builtins::is_builtin_name(&name);
        if !shadows_builtin && let Some(class_name) = self.current_class.clone() {
            // Bare-name call inside a subclass that resolves to a
            // private method on an ancestor is a privacy error even
            // before we check arity.
            if let Some(owner) = self.lookup_private_method_owner(&class_name, &name)
                && owner != class_name
            {
                self.err(
                    codes::PRIVATE_METHOD,
                    self.span_of(&ident),
                    format!("method `{name}` is private on class `{owner}`"),
                );
            } else if let Some(owner) = self.walk_class_chain(&class_name, |c| {
                self.class_private_static_methods
                    .get(c)
                    .is_some_and(|s| s.contains(&name))
            }) && owner != class_name
            {
                self.err(
                    codes::PRIVATE_STATIC_METHOD,
                    self.span_of(&ident),
                    format!("method `{name}` is private on class `{owner}`"),
                );
            } else if !self.class_has_unknown_parent.contains(&class_name)
                && let Some(arities) = self.class_method_arities.get(&class_name)
                && let Some(&(min, max)) = arities.get(&name)
                && (arg_count < min || arg_count > max)
            {
                self.err_invalid_arity(&ident, &class_name, &name, min, max, arg_count);
            }
        }
    }

    fn resolve_field_call(
        &mut self,
        f: &leek_parser::ast::FieldExpr,
        arg_count: u8,
        callee: &Expr,
    ) {
        // `super.m(...)` must resolve statically on an ancestor — the
        // upstream generator emits `super.u_m(...)`, which doesn't
        // compile in Java when no ancestor declares a matching method,
        // so it raises an analyze error instead (upstream 9627181,
        // issue #4010): UNKNOWN_METHOD when the name is absent from
        // the whole chain, INVALID_PARAMETER_COUNT when it exists with
        // another arity.
        if let Some(field_tok) = f.field()
            && field_base_is_super(f)
        {
            self.check_super_method_call(&field_tok, arg_count);
        }
        // Resolve base class for both private-method detection and
        // arity checks.
        let base_class = self.field_call_base_class(f);
        if let Some(field_tok) = f.field()
            && let Some((class_name, typed)) = base_class.clone()
        {
            let method = field_tok.text().to_string();
            let outside = Some(&class_name) != self.current_class.as_ref();
            // Whether the base is a class reference (`A.m()`)
            // versus an instance — drives the static vs instance
            // privacy check.
            let static_call = matches!(f.base(), Some(Expr::Name(ref n))
            if n.ident().is_some_and(|t| {
                matches!(self.lookup(t.text()), Some(SymbolKind::Class))
            }));
            if typed && outside {
                self.emit_method_privacy(&class_name, &method, static_call, &field_tok);
            }
            if let Some(arities) = self.class_method_arities.get(&class_name)
                && let Some(&(min, max)) = arities.get(&method)
                && (arg_count < min || arg_count > max)
            {
                let expected = format_arity(min, max);
                self.err(
                    codes::INVALID_PARAMETER_COUNT,
                    self.span_of(&field_tok),
                    format!("`{class_name}.{method}` expects {expected} args; got {arg_count}",),
                );
            }
        }
        self.resolve_expr(callee);
    }

    /// Statically resolve a `super.method(args…)` call against the
    /// enclosing class's ancestor chain, mirroring upstream's analyze
    /// pass: the loop that walks `getParent()` looking for a method
    /// group with a matching arity, then errors when nothing resolved.
    fn check_super_method_call(&mut self, field_tok: &SyntaxToken, arg_count: u8) {
        let Some(this_class) = self.current_class.clone() else {
            return;
        };
        let Some(parent) = self.class_parent.get(&this_class).cloned() else {
            return;
        };
        // Can't prove absence when an ancestor extends a class we
        // don't know about — same guard the bare-name arity check uses.
        if self
            .walk_class_chain(&parent, |c| self.class_has_unknown_parent.contains(c))
            .is_some()
        {
            return;
        }
        let method = field_tok.text().to_string();
        let resolved = self
            .walk_class_chain(&parent, |c| {
                self.class_method_arities
                    .get(c)
                    .and_then(|m| m.get(&method))
                    .is_some_and(|&(min, max)| arg_count >= min && arg_count <= max)
            })
            .is_some();
        if resolved {
            return;
        }
        let name_owner = self.walk_class_chain(&parent, |c| {
            self.class_method_arities
                .get(c)
                .is_some_and(|m| m.contains_key(&method))
        });
        if let Some(owner) = name_owner {
            // Method exists on an ancestor, but with another arity.
            let &(min, max) = self
                .class_method_arities
                .get(&owner)
                .and_then(|m| m.get(&method))
                .expect("owner found by walk_class_chain");
            self.err_invalid_arity(field_tok, &owner, &method, min, max, arg_count);
        } else {
            self.err(
                codes::UNKNOWN_METHOD,
                self.span_of(field_tok),
                format!("unknown method `{method}` on class `{parent}`"),
            );
        }
    }

    fn field_call_base_class(&self, f: &leek_parser::ast::FieldExpr) -> Option<(String, bool)> {
        let Some(Expr::Name(base_name)) = f.base() else {
            return None;
        };
        // `this.m(...)` / `class.m(...)` inside a class refers to
        // the enclosing class.
        let head_tok = base_name
            .syntax()
            .children_with_tokens()
            .filter_map(rowan::NodeOrToken::into_token)
            .find(|t| !t.kind().is_trivia());
        if let Some(t) = head_tok.as_ref()
            && matches!(t.kind(), SyntaxKind::KwThis | SyntaxKind::KwClass)
        {
            return self.current_class.clone().map(|c| (c, true));
        }
        base_name.ident().and_then(|t| {
            let txt = t.text().to_string();
            if matches!(self.lookup(&txt), Some(SymbolKind::Class)) {
                Some((txt, /*typed*/ true))
            } else {
                // Instance var. Privacy only when the var was typed;
                // arity always.
                self.var_class_of(t.text()).map(|c| {
                    let typed = self.var_class_typed_of(t.text()).is_some();
                    (c, typed)
                })
            }
        })
    }

    fn emit_method_privacy(
        &mut self,
        class_name: &str,
        method: &str,
        static_call: bool,
        field_tok: &SyntaxToken,
    ) {
        // Walk the inheritance chain — `private` and `protected`
        // methods on a parent class are still inaccessible from
        // outside even on a subclass instance.
        let priv_owner = if static_call {
            self.walk_class_chain(class_name, |c| {
                self.class_private_static_methods
                    .get(c)
                    .is_some_and(|s| s.contains(method))
                    || (self
                        .class_private_fields
                        .get(c)
                        .is_some_and(|s| s.contains(method))
                        && self
                            .class_static_members
                            .get(c)
                            .is_some_and(|s| s.contains(method)))
            })
        } else {
            self.lookup_private_method_owner(class_name, method)
        };
        let prot_owner = if static_call {
            self.walk_class_chain(class_name, |c| {
                self.class_protected_static_methods
                    .get(c)
                    .is_some_and(|s| s.contains(method))
            })
        } else {
            self.lookup_protected_method_owner(class_name, method)
        };
        if let Some(owner) = priv_owner {
            let owner_has_static_field = self
                .class_private_fields
                .get(&owner)
                .is_some_and(|s| s.contains(method))
                && self
                    .class_static_members
                    .get(&owner)
                    .is_some_and(|s| s.contains(method));
            let code = if static_call
                && owner_has_static_field
                && !self
                    .class_private_static_methods
                    .get(&owner)
                    .is_some_and(|s| s.contains(method))
            {
                codes::PRIVATE_STATIC_FIELD
            } else if static_call {
                codes::PRIVATE_STATIC_METHOD
            } else {
                codes::PRIVATE_METHOD
            };
            self.err(
                code,
                self.span_of(field_tok),
                format!("method `{method}` is private on class `{owner}`"),
            );
        } else if let Some(owner) = prot_owner {
            let code = if static_call {
                codes::PROTECTED_STATIC_METHOD
            } else {
                codes::PROTECTED_METHOD
            };
            self.err(
                code,
                self.span_of(field_tok),
                format!("method `{method}` is protected on class `{owner}`"),
            );
        }
    }

    pub(crate) fn check_call_meta(
        &mut self,
        ident: &SyntaxToken,
        name: &str,
        meta: FnMeta,
        arg_count: u8,
        is_builtin: bool,
    ) {
        // Version availability is always enforced — the failure mode
        // here is genuinely a v4-only function being called at v1-v3.
        if (self.version as u8) < meta.min_version {
            self.err(
                codes::FUNCTION_NOT_AVAILABLE,
                self.span_of(ident),
                format!(
                    "`{name}` is not available at @version:{}",
                    self.version as u8,
                ),
            );
            return;
        }
        // Arity for builtins is only enforced at v3+. v1/v2 accept
        // surplus or missing args silently (per
        // `testSystem_function_typing::15@v1`).
        if is_builtin && self.version < Version::V3 {
            return;
        }
        if arg_count < meta.min_args || arg_count > meta.max_args {
            let expected = format_arity(meta.min_args, meta.max_args);
            self.err(
                codes::INVALID_PARAMETER_COUNT,
                self.span_of(ident),
                format!("`{name}` expects {expected} args; got {arg_count}"),
            );
        }
    }

    /// Record references for class names that appear in type-annotation
    /// positions (`Cat c`, `-> Cat`, `Array<Cat>`, field / param / var
    /// types). These idents aren't visited by ordinary expression
    /// resolution, so without this pass hover and go-to-def can't reach
    /// a class from its type usages. Call once at the end of the walk,
    /// while the file scope (holding all class symbols) is still live.
    pub(crate) fn record_type_ref_classes(&mut self, root: &SyntaxNode) {
        for node in root.descendants() {
            match node.kind() {
                SyntaxKind::TypeRef => {
                    // This level's type name is its first Ident token;
                    // nested generic args live in child TypeRef nodes,
                    // each handled by their own iteration step.
                    if let Some(tok) = node
                        .children_with_tokens()
                        .filter_map(rowan::NodeOrToken::into_token)
                        .find(|t| t.kind() == SyntaxKind::Ident)
                    {
                        let name = tok.text().to_string();
                        self.record_class_ref(&tok, &name);
                    }
                }
                SyntaxKind::ClassDecl => {
                    // `class Cat extends Animal` — anchor the parent name.
                    let mut saw_extends = false;
                    for el in node.children_with_tokens() {
                        if let Some(t) = el.into_token() {
                            if t.kind() == SyntaxKind::KwExtends {
                                saw_extends = true;
                            } else if saw_extends && t.kind() == SyntaxKind::Ident {
                                let name = t.text().to_string();
                                self.record_class_ref(&t, &name);
                                break;
                            }
                        }
                    }
                }
                _ => {}
            }
        }
    }

    // ---- Name references ----

    /// `this` / `class` / `super` are keyword tokens, not Idents.
    /// Inside a class, anchor a reference from the keyword to the class
    /// it denotes so hover / go-to-def reach the declaration:
    ///   `this` / `class` → the enclosing class
    ///   `super`          → the parent class
    /// Called from both bare-name and call-callee positions
    /// (`super`, `super(...)`, `super.m()`).
    pub(crate) fn record_self_keyword_ref(&mut self, n: &NameRef) {
        if n.ident().is_some() {
            return;
        }
        let Some(tok) = n
            .syntax()
            .children_with_tokens()
            .filter_map(rowan::NodeOrToken::into_token)
            .find(|t| !t.kind().is_trivia())
        else {
            return;
        };
        let Some(current) = self.current_class.clone() else {
            return;
        };
        match tok.kind() {
            SyntaxKind::KwThis | SyntaxKind::KwClass => {
                self.record_class_ref(&tok, &current);
            }
            SyntaxKind::KwSuper => {
                if let Some(parent) = self.class_parent.get(&current).cloned() {
                    self.record_class_ref(&tok, &parent);
                }
            }
            _ => {}
        }
    }

    pub(crate) fn resolve_name_ref(&mut self, n: &NameRef) {
        self.record_self_keyword_ref(n);
        let Some(ident) = n.ident() else { return };
        // Feed the LSP-facing reference table — every NameRef that
        // resolves to a non-builtin in-scope binding becomes a
        // ResolvedRef anchored at this token.
        self.record_ref(&ident);
        let name = ident.text();
        if self.lookup(name).is_some() {
            // Builtin class names (`Array`, `Map`, `Set`, …) weren't
            // first-class values until v3 — `return Array` in v2 is
            // an unknown reference, not a class ref.
            if self.version < Version::V3
                && self.lookup(name) == Some(SymbolKind::Builtin)
                && matches!(
                    name,
                    "Array"
                        | "Map"
                        | "Set"
                        | "Object"
                        | "Class"
                        | "Function"
                        | "String"
                        | "Number"
                        | "Integer"
                        | "Real"
                        | "Boolean"
                        | "Null"
                        | "JSON"
                        | "Value",
                )
            {
                self.err(
                    codes::UNKNOWN_VARIABLE,
                    self.span_of(&ident),
                    format!("unknown variable or function `{name}`"),
                );
            }
            return;
        }
        // Identifiers that look like malformed numeric literals
        // (`_` followed by digits, e.g. `_1_000_000`) are upstream's
        // canonical UNKNOWN_VARIABLE_OR_FUNCTION case — flag them
        // even outside class scope.
        let looks_like_bad_number =
            name.starts_with('_') && name[1..].chars().next().is_some_and(|c| c.is_ascii_digit());
        // Three emission paths today:
        //   1. Case-typo'd keyword at v3+ (`True`, `FALSE`, …).
        //   2. Inside a class method, an identifier that doesn't
        //      resolve to a field/method/param/outer is wrong.
        //      `class A { m() { return name } }` expects the error.
        //   3. Malformed numeric-literal-shaped identifiers.
        if self.looks_like_case_typo(name) || self.in_class || looks_like_bad_number {
            let span = self.span_of(&ident);
            let mut diag = Diagnostic::error(
                codes::UNKNOWN_VARIABLE,
                span,
                format!("unknown variable or function `{name}`"),
            );
            // Offer a "did you mean…?" autofix when an in-scope name
            // is close enough by edit distance.
            if let Some(suggestion) = self.suggest_in_scope(name) {
                diag = diag.with_suggestion(leek_diagnostics::Suggestion::replace(
                    format!("did you mean `{suggestion}`?"),
                    span,
                    suggestion,
                ));
            }
            self.diagnostics.push(diag);
        }
    }

    /// Walk visible scopes (out to but not including the builtin
    /// scope) and return the closest non-builtin name to `needle`.
    fn suggest_in_scope(&self, needle: &str) -> Option<String> {
        // Skip the builtin scope (index 0).
        let names: Vec<&str> = self
            .scopes
            .iter()
            .skip(1)
            .flat_map(|s| s.keys().map(String::as_str))
            .collect();
        leek_diagnostics::best_match(needle, &names).map(String::from)
    }
}

/// Whether a field-call base is the `super` keyword (`super.m(...)`).
/// `super` is a keyword token inside a NameRef, not an Ident, so
/// [`Resolver::field_call_base_class`] never resolves it.
fn field_base_is_super(f: &leek_parser::ast::FieldExpr) -> bool {
    let Some(Expr::Name(base_name)) = f.base() else {
        return false;
    };
    base_name
        .syntax()
        .children_with_tokens()
        .filter_map(rowan::NodeOrToken::into_token)
        .find(|t| !t.kind().is_trivia())
        .is_some_and(|t| t.kind() == SyntaxKind::KwSuper)
}

/// Format an arity range for use in INVALID_PARAMETER_COUNT messages.
fn format_arity(min: u8, max: u8) -> String {
    if min == max {
        format!("{min}")
    } else if max == u8::MAX {
        format!("at least {min}")
    } else {
        format!("{min}..={max}")
    }
}
