//! Standalone assignment / l-value / final-field / member-access
//! checks that fire from the expression walker. Pulled into their
//! own file because each is independent and tends to evolve as we
//! tighten parity with upstream.

use leek_diagnostics::{Diagnostic, Severity};
use leek_parser::ast::{self, AstNode, Expr};
use leek_syntax::{SyntaxKind, SyntaxNode};

use crate::Resolver;
use crate::builtins;
use crate::codes;
use crate::scope::SymbolKind;
use crate::util::{INTRINSIC_FINAL_CLASS_FIELDS, is_potential_lvalue, strip_string_quotes};

impl Resolver {
    /// Emit DEFAULT_ARGUMENT_NOT_END if any non-default parameter
    /// follows a parameter with a default value.
    pub(crate) fn check_param_defaults(&mut self, fn_node: &SyntaxNode) {
        let Some(params) = fn_node
            .children()
            .find(|n| n.kind() == SyntaxKind::ParamList)
        else {
            return;
        };
        let mut seen_default = false;
        for p in params.children() {
            if p.kind() != SyntaxKind::Param {
                continue;
            }
            let has_default = p
                .children_with_tokens()
                .filter_map(rowan::NodeOrToken::into_token)
                .any(|t| t.kind() == SyntaxKind::Eq);
            if has_default {
                seen_default = true;
            } else if seen_default && let Some(ident) = crate::util::first_ident(&p) {
                self.err(
                    codes::DEFAULT_ARGUMENT_NOT_END,
                    self.span_of(&ident),
                    format!(
                        "parameter `{}` without a default follows a parameter with one",
                        ident.text(),
                    ),
                );
            }
        }
    }

    /// Check a `FieldExpr` read for visibility / existence.
    ///
    /// - `obj.private_field` from outside the class → `PRIVATE_FIELD`.
    /// - `this.unknown_field` inside the class → `CLASS_MEMBER_DOES_NOT_EXIST`.
    ///
    /// Method-call form (`obj.private_method(...)`) is handled in
    /// [`Resolver::resolve_call`](crate::Resolver::resolve_call).
    pub(crate) fn check_field_access(&mut self, f: &ast::FieldExpr) {
        let Some(field_tok) = f.field() else { return };
        let Some(base) = f.base() else { return };
        let field_text = field_tok.text();

        // `null.member` — upstream emits this as an *error* under
        // `// @strict` and a *warning* otherwise. The warning path
        // doesn't break the program's `equals` expectation.
        if let Expr::Literal(lit) = &base
            && lit.token().is_some_and(|t| t.kind() == SyntaxKind::KwNull)
        {
            let severity = if self.opts.strict {
                Severity::Error
            } else {
                Severity::Warning
            };
            self.diagnostics.push(Diagnostic::new(
                codes::CLASS_MEMBER_DOES_NOT_EXIST,
                severity,
                self.span_of(&field_tok),
                format!("`null` has no member `{field_text}`"),
            ));
            return;
        }

        // Strict-mode existence check: in non-strict mode upstream
        // tolerates missing-field access at runtime (returns null),
        // so we only emit when the program opted in via `// @strict`.
        if self.opts.strict
            && let Expr::Name(base_name) = &base
        {
            // `this.x` inside a class: x must be a declared member
            // of the current class. We skip this when the class has
            // an unknown parent — inherited members may be in play.
            let base_is_this = base_name
                .syntax()
                .children_with_tokens()
                .filter_map(rowan::NodeOrToken::into_token)
                .any(|t| t.kind() == SyntaxKind::KwThis);
            if base_is_this
                && let Some(class_name) = self.current_class.clone()
                && !self.class_has_unknown_parent.contains(&class_name)
                && self
                    .class_fields_all
                    .get(&class_name)
                    .is_some_and(|s| !s.contains(field_text))
            {
                self.err(
                    codes::CLASS_MEMBER_DOES_NOT_EXIST,
                    self.span_of(&field_tok),
                    format!("class `{class_name}` has no member `{field_text}`",),
                );
                return;
            }
            // `instance.x` where instance has a known class.
            if let Some(ident) = base_name.ident()
                && let Some(class_name) = self.var_class_of(ident.text())
                && !self.class_has_unknown_parent.contains(&class_name)
                && self
                    .class_fields_all
                    .get(&class_name)
                    .is_some_and(|s| !s.contains(field_text))
            {
                self.err(
                    codes::CLASS_MEMBER_DOES_NOT_EXIST,
                    self.span_of(&field_tok),
                    format!("class `{class_name}` has no member `{field_text}`",),
                );
                return;
            }
        }

        let Expr::Name(base_name) = base else { return };
        // `this.x` from inside a subclass where `x` is a private
        // field of an ancestor (but not the current class itself) is
        // a privacy violation. Same for the non-strict path so we
        // always emit the diagnostic.
        let base_is_this = base_name
            .syntax()
            .children_with_tokens()
            .filter_map(rowan::NodeOrToken::into_token)
            .any(|t| t.kind() == SyntaxKind::KwThis);
        if base_is_this
            && let Some(class_name) = self.current_class.clone()
            && let Some(owner) = self.lookup_private_owner(&class_name, field_text)
            && owner != class_name
        {
            self.err(
                codes::PRIVATE_FIELD,
                self.span_of(&field_tok),
                format!("field `{field_text}` is private on class `{owner}`",),
            );
            return;
        }
        // `class.member` — the base refers to the enclosing class.
        // Run the same static-existence check we run for
        // `ClassName.member`.
        let base_is_class_kw = base_name
            .syntax()
            .children_with_tokens()
            .filter_map(rowan::NodeOrToken::into_token)
            .any(|t| t.kind() == SyntaxKind::KwClass);
        if base_is_class_kw
            && let Some(class_name) = self.current_class.clone()
            && !self.class_has_unknown_parent.contains(&class_name)
        {
            let exists = INTRINSIC_FINAL_CLASS_FIELDS.contains(&field_text)
                || self
                    .class_static_members
                    .get(&class_name)
                    .is_some_and(|s| s.contains(field_text))
                || self
                    .class_fields_all
                    .get(&class_name)
                    .is_some_and(|s| s.contains(field_text));
            if !exists {
                self.err(
                    codes::CLASS_STATIC_MEMBER_DOES_NOT_EXIST,
                    self.span_of(&field_tok),
                    format!("`{class_name}` has no static member `{field_text}`",),
                );
            }
            return;
        }
        let Some(base_tok) = base_name.ident() else {
            return;
        };
        let base_text = base_tok.text();

        // `ClassName.member` — the right-hand side must exist as a
        // static field/method, instance member (referencing the
        // unbound method is allowed), or one of the intrinsic
        // metadata fields. Anything else is a static-member miss.
        // We skip the check for classes that inherit from an
        // unanalyzed parent — they may have inherited members we
        // can't see.
        if self.lookup(base_text) == Some(SymbolKind::Class)
            && !self.class_has_unknown_parent.contains(base_text)
        {
            let exists = INTRINSIC_FINAL_CLASS_FIELDS.contains(&field_text)
                || self
                    .class_static_members
                    .get(base_text)
                    .is_some_and(|s| s.contains(field_text))
                || self
                    .class_fields_all
                    .get(base_text)
                    .is_some_and(|s| s.contains(field_text));
            if !exists {
                self.err(
                    codes::CLASS_STATIC_MEMBER_DOES_NOT_EXIST,
                    self.span_of(&field_tok),
                    format!("`{base_text}` has no static member `{field_text}`",),
                );
                return;
            }
            // Static-field privacy is enforced only at call sites
            // (`A.field()`), not raw reads. See `resolve_field_call`.
            return;
        }

        // `C x = new C(...)` (explicit type) followed by `x.private_field`.
        if let Some(class_name) = self.var_class_typed_of(base_text)
            && Some(&class_name) != self.current_class.as_ref()
            && let Some(owner) = self.lookup_private_owner(&class_name, field_text)
        {
            self.err(
                codes::PRIVATE_FIELD,
                self.span_of(&field_tok),
                format!("field `{field_text}` is private on class `{owner}`",),
            );
            return;
        }

        // `var x = new C(...)` — privacy/protection apply even for
        // the inferred-class case. The same-class exemption stays.
        if let Some(class_name) = self.var_class_of(base_text)
            && Some(&class_name) != self.current_class.as_ref()
        {
            if let Some(owner) = self.lookup_private_owner(&class_name, field_text) {
                self.err(
                    codes::PRIVATE_FIELD,
                    self.span_of(&field_tok),
                    format!("field `{field_text}` is private on class `{owner}`",),
                );
            } else if let Some(owner) = self.lookup_protected_owner(&class_name, field_text) {
                self.err(
                    codes::PROTECTED_FIELD,
                    self.span_of(&field_tok),
                    format!("field `{field_text}` is protected on class `{owner}`",),
                );
            }
        }
    }

    /// Emit `CANT_ASSIGN_VALUE` when the assignment target isn't a
    /// valid l-value: literals (`5 = x`), arithmetic (`(a % 3) \= 0`),
    /// parens around non-lvalues, etc.
    pub(crate) fn check_non_lvalue_assignment(&mut self, lhs: &Expr) {
        if !is_potential_lvalue(lhs) {
            let span = self.node_span(lhs.syntax());
            self.err(
                codes::CANT_ASSIGN_VALUE,
                span,
                "left-hand side of assignment is not assignable".to_string(),
            );
        }
    }

    /// Emit `CANNOT_REDEFINE_FUNCTION` when an arithmetic mutation
    /// (`abs++`, `--push`, etc.) targets a name bound to a function
    /// or builtin. Also catches `obj.final_field++` by re-using the
    /// final-field assignment check.
    pub(crate) fn check_fn_increment(&mut self, target: &Expr) {
        match target {
            Expr::Name(n) => {
                if let Some(ident) = n.ident() {
                    let name = ident.text().to_string();
                    if matches!(
                        self.lookup(&name),
                        Some(SymbolKind::Function | SymbolKind::Builtin)
                    ) {
                        self.err(
                            codes::CANNOT_REDEFINE_FUNCTION,
                            self.span_of(&ident),
                            format!("cannot increment/decrement function `{name}`"),
                        );
                    }
                }
            }
            Expr::Field(_) => {
                // `obj.x++` is an assignment to `obj.x` — same checks.
                self.check_final_field_assignment(target);
            }
            _ => {}
        }
    }

    /// Emit `CANNOT_ASSIGN_FINAL_FIELD` if `lhs` is a `FieldExpr`
    /// targeting a known immutable builtin constant
    /// (`Integer.MIN_VALUE = …`) or a `final` field on a user class
    /// reachable via `var x = new Cls(...); x.field = …`.
    pub(crate) fn check_final_field_assignment(&mut self, lhs: &Expr) {
        // `ClassName['field'] = …` — index assignment with a string
        // literal key on a class name resolves to the same final
        // field set.
        if let Expr::Index(idx) = lhs
            && let Some(Expr::Name(base_name)) = idx.base()
            && let Some(base_tok) = base_name.ident()
            && self.lookup(base_tok.text()) == Some(SymbolKind::Class)
            && let Some(Expr::Literal(lit)) = idx.index()
            && let Some(lit_tok) = lit.token()
            && lit_tok.kind() == SyntaxKind::StringLiteral
        {
            let key = strip_string_quotes(lit_tok.text());
            if self.is_final_class_member(base_tok.text(), &key) {
                self.err(
                    codes::CANNOT_ASSIGN_FINAL_FIELD,
                    self.span_of(&lit_tok),
                    format!("cannot assign to final field `{}.{key}`", base_tok.text()),
                );
            }
            return;
        }
        let Expr::Field(field) = lhs else { return };
        let Some(field_tok) = field.field() else {
            return;
        };
        let Some(base) = field.base() else { return };
        let field_text = field_tok.text();

        // `this.x = …` inside a class method (but not constructor)
        // — look up our own class's final-field set. NameRef holds
        // the `this` token as a keyword, not an Ident, so we scan
        // child tokens. Constructors are permitted to initialize
        // final fields.
        if !self.in_constructor
            && let Expr::Name(base_name) = &base
            && base_name
                .syntax()
                .children_with_tokens()
                .filter_map(rowan::NodeOrToken::into_token)
                .any(|t| t.kind() == SyntaxKind::KwThis)
            && let Some(class_name) = self.current_class.clone()
            && let Some(finals) = self.class_final_fields.get(&class_name)
            && finals.contains(field_text)
        {
            self.err(
                codes::CANNOT_ASSIGN_FINAL_FIELD,
                self.span_of(&field_tok),
                format!("cannot assign to final field `this.{field_text}`"),
            );
            return;
        }

        let Expr::Name(base_name) = base else { return };
        let Some(base_tok) = base_name.ident() else {
            return;
        };
        let base_text = base_tok.text();

        // Builtin path like `Integer.MIN_VALUE`.
        let path = format!("{base_text}.{field_text}");
        if builtins::FINAL_BUILTIN_FIELDS.contains(&path.as_str()) {
            self.err(
                codes::CANNOT_ASSIGN_FINAL_FIELD,
                self.span_of(&field_tok),
                format!("cannot assign to final field `{path}`"),
            );
            return;
        }

        // `ClassName.field = …` — static final field, one of the
        // intrinsic class metadata fields (`name`, `fields`,
        // `methods`, `static_fields`, etc.), or a method (which
        // can't be reassigned at all).
        if self.lookup(base_text) == Some(SymbolKind::Class) {
            if self.is_final_class_member(base_text, field_text) {
                self.err(
                    codes::CANNOT_ASSIGN_FINAL_FIELD,
                    self.span_of(&field_tok),
                    format!("cannot assign to final field `{base_text}.{field_text}`"),
                );
            } else if self
                .class_method_arities
                .get(base_text)
                .is_some_and(|m| m.contains_key(field_text))
            {
                self.err(
                    codes::CANT_ASSIGN_VALUE,
                    self.span_of(&field_tok),
                    format!("cannot assign to method `{base_text}.{field_text}`"),
                );
            }
            return;
        }

        // User class via `var x = new Cls(...)` tracked binding.
        if let Some(class_name) = self.var_class_of(base_text)
            && let Some(finals) = self.class_final_fields.get(&class_name)
            && finals.contains(field_text)
        {
            self.err(
                codes::CANNOT_ASSIGN_FINAL_FIELD,
                self.span_of(&field_tok),
                format!("cannot assign to final field `{class_name}.{field_text}`"),
            );
        }
    }
}
