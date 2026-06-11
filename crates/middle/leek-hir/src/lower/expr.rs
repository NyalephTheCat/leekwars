//! Lower the parser AST into HIR — expression lowering for the public API.

use leek_parser::ast::{self, AstNode, Expr as AstExpr};
use leek_syntax::SyntaxKind;
use leek_types::Type;

use crate::ir::{
    BinaryOp, Call, Callee, Expr, ExprKind, IntervalExpr, LambdaBody, LambdaExpr, Literal, NameRef,
    NewExpr, PostfixOp, SetItem, SliceExpr, UnaryOp,
};

use super::traits::LowerExpr;
use super::util::{
    binary_op_from_token, first_ident, interval_brackets, parse_hex_float, postfix_op_from_token,
    strip_string_quotes_and_unescape_versioned, unary_op_from_token,
};
use super::{Lowerer, NameKind};

impl LowerExpr for Lowerer {
    fn lower_expr(&mut self, e: &AstExpr) -> Expr {
        let span = self.span_of_node(e.syntax());
        let kind = match e {
            AstExpr::Literal(lit) => ExprKind::Literal(self.lower_literal(lit)),
            AstExpr::Name(n) => {
                let nr = self.lower_name_ref(n);
                // Inside a method/ctor body, an unresolved bare
                // identifier that names a class field rewrites to
                // `this.field`. Method names stay as Builtin so a
                // bare `m()` call still goes through the normal
                // callee resolution path.
                if let NameRef::Builtin(name) = &nr {
                    let in_class = self.class_ctx.last();
                    if let Some(c) = in_class {
                        if c.field_names.contains(name) {
                            let this_expr = Expr {
                                kind: ExprKind::Name(NameRef::This),
                                ty: Type::Any,
                                span,
                            };
                            ExprKind::Field(Box::new(this_expr), name.clone(), false)
                        } else if c.static_field_names.contains(name)
                            || c.static_method_names.contains(name)
                        {
                            // Static field or static method — treat
                            // a bare reference as `ClassName.x`. The
                            // interp returns a `Value::Function` for
                            // static methods so `x == item` works.
                            let class_expr = Expr {
                                kind: ExprKind::Name(NameRef::Class_),
                                ty: Type::Any,
                                span,
                            };
                            ExprKind::Field(Box::new(class_expr), name.clone(), false)
                        } else if c.method_names.contains(name) {
                            // Bare reference to an instance method
                            // value — rewrite to `this.method`. The
                            // interp returns a `BoundMethod` so
                            // `var f = m; f(args)` works.
                            let this_expr = Expr {
                                kind: ExprKind::Name(NameRef::This),
                                ty: Type::Any,
                                span,
                            };
                            ExprKind::Field(Box::new(this_expr), name.clone(), false)
                        } else {
                            ExprKind::Name(nr)
                        }
                    } else {
                        ExprKind::Name(nr)
                    }
                } else {
                    ExprKind::Name(nr)
                }
            }
            AstExpr::Binary(b) => self.lower_binary(b),
            AstExpr::Unary(u) => self.lower_unary(u),
            AstExpr::Postfix(p) => self.lower_postfix(p),
            AstExpr::Paren(p) => {
                return self.lower_expr_or_null(p.inner(), span);
            }
            AstExpr::Call(c) => ExprKind::Call(Box::new(self.lower_call(c))),
            AstExpr::Array(a) => {
                ExprKind::Array(a.elements().map(|el| self.lower_expr(&el)).collect())
            }
            AstExpr::Index(idx) => {
                let base = self.lower_expr_or_null(idx.base(), span);
                let i = self.lower_expr_or_null(idx.index(), span);
                ExprKind::Index(Box::new(base), Box::new(i))
            }
            AstExpr::Field(f) => {
                let base = self.lower_expr_or_null(f.base(), span);
                let field = f.field().map(|t| t.text().to_string()).unwrap_or_default();
                ExprKind::Field(Box::new(base), field, f.is_optional())
            }
            AstExpr::Map(m) => {
                let mut pairs = Vec::new();
                let exprs: Vec<_> = m.syntax().children().filter_map(AstExpr::cast).collect();
                let mut it = exprs.into_iter();
                while let (Some(k), Some(v)) = (it.next(), it.next()) {
                    pairs.push((self.lower_expr(&k), self.lower_expr(&v)));
                }
                ExprKind::Map(pairs)
            }
            AstExpr::Set(s) => ExprKind::Set(
                s.syntax()
                    .children()
                    .filter_map(|child| {
                        if child.kind() == SyntaxKind::SetRangeElement {
                            // `a..b` range element — both bounds are
                            // expression children of the wrapper node.
                            let mut bounds = child.children().filter_map(AstExpr::cast);
                            let start = bounds.next()?;
                            let end = bounds.next();
                            Some(SetItem {
                                start: self.lower_expr(&start),
                                end: end.map(|e| self.lower_expr(&e)),
                            })
                        } else {
                            AstExpr::cast(child).map(|e| SetItem {
                                start: self.lower_expr(&e),
                                end: None,
                            })
                        }
                    })
                    .collect(),
            ),
            AstExpr::Object(o) => {
                // Object: alternating key, value pairs at the AST level
                // (same shape as Map). Keys are typically identifiers
                // — we keep the textual form.
                let mut pairs = Vec::new();
                let exprs: Vec<_> = o.syntax().children().filter_map(AstExpr::cast).collect();
                let mut it = exprs.into_iter();
                while let (Some(k), Some(v)) = (it.next(), it.next()) {
                    let key = match &k {
                        AstExpr::Name(n) => {
                            n.ident().map(|t| t.text().to_string()).unwrap_or_default()
                        }
                        _ => String::new(),
                    };
                    pairs.push((key, self.lower_expr(&v)));
                }
                ExprKind::Object(pairs)
            }
            AstExpr::Ternary(t) => {
                let exprs: Vec<_> = t.syntax().children().filter_map(AstExpr::cast).collect();
                let mut it = exprs.into_iter();
                let cond = self.lower_expr_or_null(it.next(), span);
                let then_b = self.lower_expr_or_null(it.next(), span);
                let else_b = self.lower_expr_or_null(it.next(), span);
                ExprKind::Ternary(Box::new(cond), Box::new(then_b), Box::new(else_b))
            }
            AstExpr::Interval(i) => {
                // Source shape: `[start? .. end? (: step)?]`. Walk
                // children in source order, attributing exprs to the
                // start/end/step slot based on where they appear
                // relative to the `..` and `:` markers.
                let mut start: Option<Box<Expr>> = None;
                let mut end: Option<Box<Expr>> = None;
                let mut step: Option<Box<Expr>> = None;
                let mut saw_dotdot = false;
                let mut saw_colon = false;
                for child in i.syntax().children_with_tokens() {
                    match &child {
                        rowan::NodeOrToken::Token(t) => match t.kind() {
                            SyntaxKind::DotDot => saw_dotdot = true,
                            SyntaxKind::Colon => saw_colon = true,
                            _ => {}
                        },
                        rowan::NodeOrToken::Node(n) => {
                            if let Some(e) = AstExpr::cast(n.clone()) {
                                let lowered = Box::new(self.lower_expr(&e));
                                if saw_colon {
                                    step = Some(lowered);
                                } else if saw_dotdot {
                                    end = Some(lowered);
                                } else {
                                    start = Some(lowered);
                                }
                            }
                        }
                    }
                }
                let (start_inclusive, end_inclusive) = interval_brackets(i.syntax());
                ExprKind::Interval(IntervalExpr {
                    start,
                    end,
                    step,
                    start_inclusive,
                    end_inclusive,
                })
            }
            AstExpr::Slice(s) => {
                // Source shape: `base[start? : end? (: step)?]`. The
                // `:` markers split the bracketed portion into up to
                // three slots; the AST flattens all of them as
                // sibling expressions, so we walk in source order and
                // attribute each child to start/end/step based on
                // how many `:` we've passed.
                let mut base: Option<Box<Expr>> = None;
                let mut start: Option<Box<Expr>> = None;
                let mut end: Option<Box<Expr>> = None;
                let mut step: Option<Box<Expr>> = None;
                let mut saw_bracket = false;
                let mut colon_count = 0;
                for child in s.syntax().children_with_tokens() {
                    match &child {
                        rowan::NodeOrToken::Token(t) => match t.kind() {
                            SyntaxKind::LBracket => saw_bracket = true,
                            SyntaxKind::Colon if saw_bracket => colon_count += 1,
                            _ => {}
                        },
                        rowan::NodeOrToken::Node(n) => {
                            if let Some(e) = AstExpr::cast(n.clone()) {
                                let lowered = Box::new(self.lower_expr(&e));
                                if saw_bracket {
                                    match colon_count {
                                        0 => start = Some(lowered),
                                        1 => end = Some(lowered),
                                        _ => step = Some(lowered),
                                    }
                                } else {
                                    base = Some(lowered);
                                }
                            }
                        }
                    }
                }
                let base = base.unwrap_or_else(|| Box::new(self.null_expr(span)));
                ExprKind::Slice(SliceExpr {
                    base,
                    start,
                    end,
                    step,
                })
            }
            AstExpr::Cast(c) => {
                let inner =
                    self.lower_expr_or_null(c.syntax().children().find_map(AstExpr::cast), span);
                // We don't bind the cast's destination type yet; keep
                // it as Any for now.
                ExprKind::Cast(Box::new(inner), Type::Any)
            }
            AstExpr::New(n) => {
                let class = first_ident(n.syntax())
                    .map(|t| t.text().to_string())
                    .unwrap_or_default();
                let mut args = Vec::new();
                for child in n.syntax().children() {
                    if child.kind() == SyntaxKind::ArgList {
                        for sub in child.children() {
                            if let Some(e) = AstExpr::cast(sub) {
                                args.push(self.lower_expr(&e));
                            }
                        }
                    }
                }
                ExprKind::New(NewExpr { class, args })
            }
            AstExpr::Lambda(l) => self.lower_lambda(l),
        };
        Expr {
            kind,
            ty: Type::Any,
            span,
        }
    }
}

impl Lowerer {
    // ---- Expression helpers ----

    pub(crate) fn lower_literal(&self, lit: &ast::LiteralExpr) -> Literal {
        let Some(tok) = lit.token() else {
            return Literal::Null;
        };
        let text = tok.text();
        match tok.kind() {
            SyntaxKind::IntLiteral => {
                // An `L` suffix marks a big_integer literal (`2L`,
                // `0xFFL`) — kept as canonical decimal digits.
                if text.ends_with('L') {
                    Literal::BigInt(super::util::parse_bigint_text(text))
                } else {
                    // Strip underscore separators and parse — falls back
                    // to 0 if a bad-suffix lexer diagnostic already
                    // covered the case.
                    Literal::Int(super::util::parse_int_text(text))
                }
            }
            SyntaxKind::RealLiteral => {
                let cleaned: String = text.chars().filter(|c| *c != '_').collect();
                // Hex floats (`0x1.p53`, `0xa.bcdp-42`) — Rust's
                // `f64::from_str` doesn't accept the C99 hex-float
                // syntax, so parse manually.
                if let Some(rest) = cleaned
                    .strip_prefix("0x")
                    .or_else(|| cleaned.strip_prefix("0X"))
                {
                    Literal::Real(parse_hex_float(rest))
                } else {
                    cleaned
                        .parse::<f64>()
                        .map(Literal::Real)
                        .unwrap_or(Literal::Real(0.0))
                }
            }
            SyntaxKind::StringLiteral => Literal::String(
                strip_string_quotes_and_unescape_versioned(text, self.version),
            ),
            SyntaxKind::KwTrue => Literal::Bool(true),
            SyntaxKind::KwFalse => Literal::Bool(false),
            SyntaxKind::KwNull => Literal::Null,
            SyntaxKind::Pi => Literal::Real(std::f64::consts::PI),
            SyntaxKind::Lemniscate => Literal::Real(f64::INFINITY),
            _ => Literal::Null,
        }
    }

    /// Resolve a class-or-builtin name (used by `instanceof`).
    /// Prefers user-defined classes, falls back to a builtin tag.
    pub(crate) fn resolve_class_or_builtin(&self, name: &str) -> NameRef {
        if let Some(NameKind::Class(id)) = self.file_decls.get(name) {
            return NameRef::Class(*id);
        }
        NameRef::Builtin(name.to_string())
    }

    pub(crate) fn lower_name_ref(&mut self, n: &ast::NameRef) -> NameRef {
        // Keyword-shaped name (`this`, `super`, `class`).
        if let Some(t) = n
            .syntax()
            .children_with_tokens()
            .filter_map(rowan::NodeOrToken::into_token)
            .find(|t| !t.kind().is_trivia())
        {
            match t.kind() {
                SyntaxKind::KwThis => return NameRef::This,
                SyntaxKind::KwSuper => return NameRef::Super,
                SyntaxKind::KwClass => return NameRef::Class_,
                _ => {}
            }
        }
        let Some(ident) = n.ident() else {
            return NameRef::Unresolved(String::new());
        };
        let name = ident.text();
        if let Some(id) = self.lookup_local(name) {
            return NameRef::Local(id);
        }
        if let Some(kind) = self.file_decls.get(name) {
            return match kind {
                NameKind::Function(id) => NameRef::Function(*id),
                NameKind::Class(id) => NameRef::Class(*id),
                NameKind::Global(id) => NameRef::Global(*id),
            };
        }
        // Anything else — could be a builtin or unresolved. The
        // interpreter knows the builtin set; we tag everything as
        // `Builtin` here and let the interpreter sort it out.
        NameRef::Builtin(name.to_string())
    }

    pub(crate) fn lower_binary(&mut self, b: &ast::BinaryExpr) -> ExprKind {
        let span = self.span_of_node(b.syntax());
        let lhs = self.lower_expr_or_null(b.lhs(), span);
        let mut rhs = self.lower_expr_or_null(b.rhs(), span);
        // `x instanceof Foo` parses with a `TypeRef` RHS, not an
        // expression. Materialize the type as a name reference so the
        // interpreter resolves it to a class value.
        if b.op().map(|t| t.kind()) == Some(SyntaxKind::KwInstanceof)
            && let Some(tr) = b
                .syntax()
                .children()
                .find(|n| n.kind() == SyntaxKind::TypeRef)
            && let Some(name) = tr
                .children_with_tokens()
                .filter_map(rowan::NodeOrToken::into_token)
                .find(|t| t.kind() == SyntaxKind::Ident)
        {
            let span = self.span_of_token(&name);
            let nr = self.resolve_class_or_builtin(name.text());
            rhs = Expr {
                kind: ExprKind::Name(nr),
                ty: Type::Any,
                span,
            };
        }
        // `not in` is a two-token operator — scan the node's tokens
        // and recognise the pair before falling back to single-token
        // lookup.
        let tokens: Vec<SyntaxKind> = b
            .syntax()
            .children_with_tokens()
            .filter_map(rowan::NodeOrToken::into_token)
            .filter(|t| !t.kind().is_trivia())
            .map(|t| t.kind())
            .collect();
        // Two-token operators: `not in` and `is not`. Both have
        // dedicated HIR variants because their truthiness is the
        // inverse of their single-token counterparts.
        let has_pair = |first, second| {
            tokens
                .iter()
                .zip(tokens.iter().skip(1))
                .any(|(a, b)| *a == first && *b == second)
        };
        // `is not` reaches us as `BinaryExpr(lhs, Is, UnaryExpr(Not, x))`
        // because the precedence parser binds `not` as a unary prefix.
        // Unwrap the rhs and rewrite as inequality on the inner value.
        let raw_op = b.op().map(|t| t.kind());
        let is_not_chain = matches!(raw_op, Some(SyntaxKind::KwIs))
            && matches!(&rhs.kind, ExprKind::Unary(UnaryOp::Not, _));
        if is_not_chain {
            let inner_rhs = match rhs.kind {
                ExprKind::Unary(UnaryOp::Not, inner) => *inner,
                _ => unreachable!(),
            };
            return ExprKind::Binary(BinaryOp::Ne, Box::new(lhs), Box::new(inner_rhs));
        }
        let op = if has_pair(SyntaxKind::KwNot, SyntaxKind::KwIn) {
            BinaryOp::NotIn
        } else if has_pair(SyntaxKind::KwIs, SyntaxKind::KwNot) {
            BinaryOp::Ne
        } else {
            b.op()
                .and_then(|t| binary_op_from_token(t.kind()))
                .unwrap_or(BinaryOp::Assign)
        };
        ExprKind::Binary(op, Box::new(lhs), Box::new(rhs))
    }

    pub(crate) fn lower_unary(&mut self, u: &ast::UnaryExpr) -> ExprKind {
        let span = self.span_of_node(u.syntax());
        let operand = self.lower_expr_or_null(u.operand(), span);
        // Default the op to `Ref` (no-op) when the token isn't a
        // recognised unary operator — better than silently
        // negating, which masks parser bugs and changes values.
        let op = u
            .op()
            .and_then(|t| unary_op_from_token(t.kind()))
            .unwrap_or(UnaryOp::Ref);
        ExprKind::Unary(op, Box::new(operand))
    }

    pub(crate) fn lower_postfix(&mut self, p: &ast::PostfixExpr) -> ExprKind {
        let span = self.span_of_node(p.syntax());
        let operand = self.lower_expr_or_null(p.syntax().children().find_map(AstExpr::cast), span);
        let op_kind = p
            .syntax()
            .children_with_tokens()
            .filter_map(rowan::NodeOrToken::into_token)
            .find_map(|t| postfix_op_from_token(t.kind()));
        let op = op_kind.unwrap_or(PostfixOp::NonNull);
        ExprKind::Postfix(op, Box::new(operand))
    }

    pub(crate) fn lower_call(&mut self, c: &ast::CallExpr) -> Call {
        let span = self.span_of_node(c.syntax());
        let args = c
            .arg_list()
            .map(|al| al.args().map(|a| self.lower_expr(&a)).collect::<Vec<_>>())
            .unwrap_or_default();
        let callee = match c.callee() {
            Some(AstExpr::Name(n)) => {
                let nr = self.lower_name_ref(&n);
                // Inside a class body, a bare `foo()` call may
                // resolve to an instance method (`this.foo`) or a
                // static method (`class.foo`). Rewrite to a method
                // call so the dispatcher does the right thing.
                if let NameRef::Builtin(name) = &nr {
                    let argc = args.len();
                    if let Some(ctx) = self.class_ctx.last() {
                        // Arity-aware rewrite: a bare `foo(...)`
                        // resolves to `this.foo`/`Class_.foo` only
                        // when one of the in-scope method overloads
                        // matches the call's argument count. Misses
                        // fall through to the builtin (so
                        // `class A { sqrt() {} sqrt(x,y) {} }`'s
                        // `sqrt(25)` reaches the math builtin).
                        let matches_instance = ctx
                            .method_arities
                            .get(name)
                            .is_some_and(|s| s.contains(&argc));
                        let matches_static = ctx
                            .static_method_arities
                            .get(name)
                            .is_some_and(|s| s.contains(&argc));
                        if matches_instance {
                            let receiver = Expr {
                                kind: ExprKind::Name(NameRef::This),
                                ty: Type::Any,
                                span,
                            };
                            Callee::Method {
                                receiver,
                                method: name.clone(),
                                optional: false,
                            }
                        } else if matches_static {
                            let receiver = Expr {
                                kind: ExprKind::Name(NameRef::Class_),
                                ty: Type::Any,
                                span,
                            };
                            Callee::Method {
                                receiver,
                                method: name.clone(),
                                optional: false,
                            }
                        } else {
                            Callee::Function(nr)
                        }
                    } else {
                        Callee::Function(nr)
                    }
                } else {
                    Callee::Function(nr)
                }
            }
            Some(AstExpr::Field(f)) => {
                let receiver = self.lower_expr_or_null(f.base(), span);
                let method = f.field().map(|t| t.text().to_string()).unwrap_or_default();
                Callee::Method {
                    receiver,
                    method,
                    optional: f.is_optional(),
                }
            }
            Some(other) => Callee::Expr(self.lower_expr(&other)),
            None => Callee::Expr(self.null_expr(span)),
        };
        Call { callee, args, span }
    }

    pub(crate) fn lower_lambda(&mut self, l: &ast::LambdaExpr) -> ExprKind {
        let span = self.span_of_node(l.syntax());
        // Lambdas use a *transparent* boundary so closures see
        // outer locals as captures (handled downstream in MIR's
        // `lower_lambda` via `collect_lambda_captures`). Methods
        // and top-level functions use the opaque
        // `push_function_scope` to keep their fields/this resolution
        // from being shadowed by enclosing locals.
        self.push_scope();
        let params = self.lower_params(l.syntax());
        // Body: either a Block or a single expression.
        let body = if let Some(b) = l.syntax().children().find_map(ast::Block::cast) {
            LambdaBody::Block(self.lower_block(&b))
        } else if let Some(e) = l.syntax().children().find_map(AstExpr::cast) {
            LambdaBody::Expr(Box::new(self.lower_expr(&e)))
        } else {
            LambdaBody::Expr(Box::new(self.null_expr(span)))
        };
        self.pop_scope();
        ExprKind::Lambda(LambdaExpr { params, body })
    }
}
