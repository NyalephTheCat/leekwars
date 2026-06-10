//! Lower the parser AST into HIR — class and function lowering for the public API.

use leek_parser::ast::{self, AstNode, Expr as AstExpr};
use leek_syntax::{SyntaxKind, SyntaxNode};

use crate::ir::{
    Class, Def, Expr, ExprKind, Field, Function, Literal, Local, MethodDef, Param, Visibility,
};

use super::traits::LowerExpr;
use super::util::{
    collect_modifiers, field_name, first_ident_after, fn_return_type, method_name, parse_int_text,
};
use super::{ClassCtx, Lowerer, NameKind};

impl Lowerer {
    // ---- First-pass: register top-level items ----

    pub(crate) fn predeclare_function(&mut self, decl: &ast::FnDecl) {
        let Some(name_tok) = first_ident_after(decl.syntax(), SyntaxKind::KwFunction) else {
            return;
        };
        let name = name_tok.text().to_string();
        let span = self.span_of_node(decl.syntax());
        let backend_directives = self.fn_backend_directives(decl, span);
        // A bodiless signature's parameter/return types are captured here so
        // each overload's DefId carries them (the body pass only fills the
        // last same-named decl via `file_decls`, which would otherwise drop
        // earlier overloads' signatures). Functions with a body keep the
        // empty placeholder — the body pass populates them.
        let has_body = decl
            .syntax()
            .children()
            .any(|c| c.kind() == SyntaxKind::Block);
        let (params, return_type) = if has_body {
            (Vec::new(), None)
        } else {
            self.push_function_scope();
            let params = self.lower_params(decl.syntax());
            self.pop_scope();
            (params, fn_return_type(decl.syntax()))
        };
        let id = self.alloc_def(Def::Function(Function {
            name: name.clone(),
            span,
            params,
            return_type,
            body: None,
            backend_directives,
        }));
        self.out.items.push(id);
        self.file_decls.insert(name, NameKind::Function(id));
    }

    /// Read `@<backend>-backend:` directives from a function's doc
    /// comment. They're only honored on bodiless signatures in
    /// signature-file mode; anywhere else they're dropped with an
    /// `E0301` warning so they can't silently affect normal code.
    fn fn_backend_directives(
        &mut self,
        decl: &ast::FnDecl,
        span: leek_span::Span,
    ) -> Vec<(String, String)> {
        let offset = u32::from(decl.syntax().text_range().start());
        let Some((_, directives)) =
            leek_syntax::doc::doc_and_directives_before(&self.source_text, offset)
        else {
            return Vec::new();
        };
        if directives.is_empty() {
            return Vec::new();
        }
        let has_body = decl
            .syntax()
            .children()
            .any(|c| c.kind() == SyntaxKind::Block);
        let allowed =
            leek_syntax::doc::directives_enabled(&self.source_text, self.flags.function_signatures)
                && !has_body;
        if !allowed {
            self.diagnostics.push(leek_diagnostics::Diagnostic::warning(
                leek_diagnostics::codes::BACKEND_DIRECTIVE_NOT_ALLOWED,
                span,
                "backend directives are only allowed on bodiless function \
                 signatures in signature files; ignoring",
            ));
            return Vec::new();
        }
        directives.into_pairs()
    }

    pub(crate) fn predeclare_class(&mut self, decl: &ast::ClassDecl) {
        let Some(name_tok) = first_ident_after(decl.syntax(), SyntaxKind::KwClass) else {
            return;
        };
        let name = name_tok.text().to_string();
        let span = self.span_of_node(decl.syntax());
        let id = self.alloc_def(Def::Class(Class {
            name: name.clone(),
            span,
            parent: first_ident_after(decl.syntax(), SyntaxKind::KwExtends)
                .map(|t| t.text().to_string()),
            fields: Vec::new(),
            methods: Vec::new(),
            constructors: Vec::new(),
        }));
        self.out.items.push(id);
        self.file_decls.insert(name, NameKind::Class(id));
    }

    /// Lower an experimental `enum Name { A, B = 10, C }` declaration.
    /// Registered in the same first pass as classes (everything about
    /// it is literal, so there is no body pass): variants become
    /// static final integer fields on a synthesized class — exactly
    /// `class Name { static final integer A = 0 … }` — so backends
    /// need no enum-specific support and the construct stays
    /// expressible in official LeekScript for the converter. Values
    /// auto-increment from 0; an explicit `= (-)? INT` resets the
    /// counter (duplicate-name/value diagnostics live in the type
    /// checker, which sees the same CST).
    pub(crate) fn lower_enum_decl(&mut self, node: &SyntaxNode) {
        let Some(name_tok) = first_ident_after(node, SyntaxKind::KwEnum) else {
            return;
        };
        let name = name_tok.text().to_string();
        let span = self.span_of_node(node);
        let mut fields = Vec::new();
        let mut next_value: i64 = 0;
        for member in node
            .children()
            .filter(|n| n.kind() == SyntaxKind::EnumMember)
        {
            let mut variant = None;
            let mut int_tok = None;
            let mut negated = false;
            for tok in member
                .children_with_tokens()
                .filter_map(rowan::NodeOrToken::into_token)
            {
                match tok.kind() {
                    SyntaxKind::Ident if variant.is_none() => variant = Some(tok),
                    SyntaxKind::Minus => negated = true,
                    SyntaxKind::IntLiteral => int_tok = Some(tok),
                    _ => {}
                }
            }
            let Some(variant) = variant else { continue };
            if let Some(tok) = int_tok {
                let magnitude = parse_int_text(tok.text());
                next_value = if negated { -magnitude } else { magnitude };
            }
            let value = next_value;
            next_value = next_value.wrapping_add(1);
            let member_span = self.span_of_node(&member);
            let variant_name = variant.text().to_string();
            let field_def = self.alloc_def(Def::Local(Local {
                name: variant_name.clone(),
                ty: Some(leek_types::Type::Integer),
                span: member_span,
            }));
            fields.push(Field {
                def: field_def,
                name: variant_name,
                ty: Some(leek_types::Type::Integer),
                init: Some(Expr {
                    kind: ExprKind::Literal(Literal::Int(value)),
                    // HIR expression types are uniformly `Any` (the
                    // checker's TypeTable is the type source) — keep
                    // the synthesized literal consistent with that.
                    ty: leek_types::Type::Any,
                    span: member_span,
                }),
                is_static: true,
                is_final: true,
                visibility: Visibility::Public,
                span: member_span,
            });
        }
        let id = self.alloc_def(Def::Class(Class {
            name: name.clone(),
            span,
            parent: None,
            fields,
            methods: Vec::new(),
            constructors: Vec::new(),
        }));
        self.out.items.push(id);
        self.file_decls.insert(name, NameKind::Class(id));
    }

    // ---- Second-pass: lower bodies ----

    pub(crate) fn lower_function_body(&mut self, decl: &ast::FnDecl) {
        let Some(name_tok) = first_ident_after(decl.syntax(), SyntaxKind::KwFunction) else {
            return;
        };
        let Some(NameKind::Function(id)) = self.file_decls.get(name_tok.text()).copied() else {
            return;
        };
        self.push_function_scope();
        let params = self.lower_params(decl.syntax());
        let body = decl
            .syntax()
            .children()
            .find_map(ast::Block::cast)
            .map(|b| self.lower_block(&b));
        self.pop_scope();
        let return_type = fn_return_type(decl.syntax());
        if let Some(Def::Function(f)) = self.out.defs.get_mut(id.0 as usize) {
            f.params = params;
            f.body = body;
            f.return_type = return_type;
        }
    }

    // The parent-chain walk moves `cursor` via `while let Some(..) = cursor`,
    // so the reassignment needs a fresh clone — `clone_from` can't apply.
    #[allow(clippy::assigning_clones)]
    pub(crate) fn lower_class_body(&mut self, decl: &ast::ClassDecl) {
        let Some(name_tok) = first_ident_after(decl.syntax(), SyntaxKind::KwClass) else {
            return;
        };
        let Some(NameKind::Class(id)) = self.file_decls.get(name_tok.text()).copied() else {
            return;
        };
        let Some(body) = decl
            .syntax()
            .children()
            .find(|n| n.kind() == SyntaxKind::ClassBody)
        else {
            return;
        };
        let mut fields = Vec::new();
        let mut methods = Vec::new();
        let mut constructors = Vec::new();
        // First sweep: collect field and method names so method
        // bodies can resolve bare references back to `this.field`
        // or static calls on `class`.
        let mut ctx = ClassCtx::default();
        // Seed with inherited members from the parent chain so a
        // bare `m()` inside a subclass method resolves through the
        // chain (e.g. `class B extends A { r() { return m() } }`
        // where `m` lives on `A`).
        if let Some(Def::Class(self_class)) = self.out.defs.get(id.0 as usize) {
            let mut cursor = self_class.parent.clone();
            let mut seen = std::collections::HashSet::new();
            while let Some(pname) = cursor {
                if !seen.insert(pname.clone()) {
                    break;
                }
                let Some(NameKind::Class(pid)) = self.file_decls.get(&pname).copied() else {
                    break;
                };
                let Some(Def::Class(pclass)) = self.out.defs.get(pid.0 as usize) else {
                    break;
                };
                for f in &pclass.fields {
                    if f.is_static {
                        ctx.static_field_names.insert(f.name.clone());
                    } else {
                        ctx.field_names.insert(f.name.clone());
                    }
                }
                for m in &pclass.methods {
                    if m.is_static {
                        ctx.static_method_names.insert(m.name.clone());
                        ctx.static_method_arities
                            .entry(m.name.clone())
                            .or_default()
                            .insert(m.params.len());
                    } else {
                        ctx.method_names.insert(m.name.clone());
                        ctx.method_arities
                            .entry(m.name.clone())
                            .or_default()
                            .insert(m.params.len());
                    }
                }
                cursor = pclass.parent.clone();
            }
        }
        for member in body.children() {
            let modifiers = collect_modifiers(&member);
            let is_static = modifiers.contains(&"static");
            match member.kind() {
                SyntaxKind::ClassField => {
                    if let Some(f) = ast::ClassField::cast(member.clone())
                        && let Some(t) = field_name(&f)
                    {
                        let name = t.text().to_string();
                        if is_static {
                            ctx.static_field_names.insert(name);
                        } else {
                            ctx.field_names.insert(name);
                        }
                    }
                }
                SyntaxKind::ClassMethod => {
                    if let Some(m) = ast::ClassMethod::cast(member.clone())
                        && let Some(t) = method_name(&m)
                    {
                        let name = t.text().to_string();
                        // Count user params by scanning the
                        // method's ParamList (no name resolution
                        // happens in this first sweep).
                        let arity = m
                            .syntax()
                            .children()
                            .find(|n| n.kind() == SyntaxKind::ParamList)
                            .map_or(0, |pl| {
                                pl.children()
                                    .filter(|c| c.kind() == SyntaxKind::Param)
                                    .count()
                            });
                        if is_static {
                            ctx.static_method_names.insert(name.clone());
                            ctx.static_method_arities
                                .entry(name)
                                .or_default()
                                .insert(arity);
                        } else {
                            ctx.method_names.insert(name.clone());
                            ctx.method_arities.entry(name).or_default().insert(arity);
                        }
                    }
                }
                _ => {}
            }
        }
        self.class_ctx.push(ctx);
        for member in body.children() {
            let modifiers = collect_modifiers(&member);
            let visibility = if modifiers.contains(&"private") {
                Visibility::Private
            } else if modifiers.contains(&"protected") {
                Visibility::Protected
            } else {
                Visibility::Public
            };
            let is_static = modifiers.contains(&"static");
            let is_final = modifiers.contains(&"final");
            match member.kind() {
                SyntaxKind::ClassField => {
                    if let Some(f) = ast::ClassField::cast(member.clone()) {
                        let span = self.span_of_node(f.syntax());
                        let name = field_name(&f)
                            .map(|t| t.text().to_string())
                            .unwrap_or_default();
                        let init = f
                            .syntax()
                            .children()
                            .find_map(AstExpr::cast)
                            .map(|e| self.lower_expr(&e));
                        // Optional type annotation on the field, e.g.
                        // `static real reel` or `real? a = 12`.
                        let field_ty = f
                            .syntax()
                            .children()
                            .find(|n| n.kind() == SyntaxKind::TypeRef)
                            .map(|n| leek_types::type_from_node(&n));
                        let field_def = self.alloc_def(Def::Local(Local {
                            name: name.clone(),
                            ty: field_ty.clone(),
                            span,
                        }));
                        fields.push(Field {
                            def: field_def,
                            name,
                            ty: field_ty,
                            init,
                            is_static,
                            is_final,
                            visibility,
                            span,
                        });
                    }
                }
                SyntaxKind::ClassMethod => {
                    if let Some(m) = ast::ClassMethod::cast(member.clone()) {
                        let span = self.span_of_node(m.syntax());
                        let name = method_name(&m)
                            .map(|t| t.text().to_string())
                            .unwrap_or_default();
                        self.push_function_scope();
                        let params = self.lower_params(m.syntax());
                        let body = m
                            .syntax()
                            .children()
                            .find_map(ast::Block::cast)
                            .map(|b| self.lower_block(&b));
                        self.pop_scope();
                        let method_def = self.alloc_def(Def::Function(Function {
                            name: name.clone(),
                            span,
                            params: Vec::new(),
                            return_type: None,
                            body: None,
                            backend_directives: Vec::new(),
                        }));
                        methods.push(MethodDef {
                            def: method_def,
                            name,
                            params,
                            return_type: fn_return_type(m.syntax()),
                            body,
                            is_static,
                            visibility,
                            span,
                        });
                    }
                }
                SyntaxKind::ClassConstructor => {
                    let span = self.span_of_node(&member);
                    self.push_function_scope();
                    let params = self.lower_params(&member);
                    let body = member
                        .children()
                        .find_map(ast::Block::cast)
                        .map(|b| self.lower_block(&b));
                    self.pop_scope();
                    let ctor_def = self.alloc_def(Def::Function(Function {
                        name: "constructor".into(),
                        span,
                        params: Vec::new(),
                        return_type: None,
                        body: None,
                        backend_directives: Vec::new(),
                    }));
                    constructors.push(MethodDef {
                        def: ctor_def,
                        name: "constructor".into(),
                        params,
                        return_type: None,
                        body,
                        is_static: false,
                        visibility,
                        span,
                    });
                }
                _ => {}
            }
        }
        self.class_ctx.pop();
        if let Some(Def::Class(c)) = self.out.defs.get_mut(id.0 as usize) {
            c.fields = fields;
            c.methods = methods;
            c.constructors = constructors;
        }
    }

    pub(crate) fn lower_params(&mut self, fn_node: &SyntaxNode) -> Vec<Param> {
        let Some(params) = fn_node
            .children()
            .find(|n| n.kind() == SyntaxKind::ParamList)
        else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for p in params.children() {
            if p.kind() != SyntaxKind::Param {
                continue;
            }
            let Some(ident) = p
                .children_with_tokens()
                .filter_map(rowan::NodeOrToken::into_token)
                .find(|t| t.kind() == SyntaxKind::Ident)
            else {
                continue;
            };
            // `@x` reference marker — the lexer leaves `@` as a
            // sibling token of the ident inside the `Param` node.
            let is_by_ref = p
                .children_with_tokens()
                .filter_map(rowan::NodeOrToken::into_token)
                .any(|t| t.kind() == SyntaxKind::At);
            let span = self.span_of_token(&ident);
            let name = ident.text().to_string();
            let id = self.declare_local(&name, span, None);
            // Default initializer if present.
            let default = p
                .children()
                .find_map(AstExpr::cast)
                .map(|e| self.lower_expr(&e));
            // Optional `real`-style annotation in `function f(real r)`.
            let param_ty = p
                .children()
                .find(|n| n.kind() == SyntaxKind::TypeRef)
                .map(|n| leek_types::type_from_node(&n));
            out.push(Param {
                def: id,
                name,
                ty: param_ty,
                default,
                is_by_ref,
                span,
            });
        }
        out
    }
}
