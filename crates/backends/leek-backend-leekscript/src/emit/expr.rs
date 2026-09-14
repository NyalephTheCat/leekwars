//! Expression emission with precedence-driven re-parenthesization.

use leek_hir::{Call, Callee, Expr, ExprKind, LambdaBody, Literal, NameRef, UnaryOp};
use leek_span::Span;
use leek_syntax::Version;
use leek_types::Type;

use crate::prec::{
    self, PREC_ATOM, PREC_POSTFIX, PREC_PREFIX, PREC_TERNARY, binary_str, left_min, right_min,
};

use super::Emitter;

/// Precedence of the call/postfix tier — receivers below it parenthesize.
const PREC_CALL: u8 = 16;
/// Lambdas parenthesize whenever used as an operand (any `min > 0`).
const PREC_LAMBDA: u8 = 0;

impl Emitter<'_> {
    /// Emit `e`, wrapping it in parentheses when its precedence is looser
    /// than the surrounding context requires (`min`).
    pub(crate) fn emit_expr(&mut self, e: &Expr, min: u8) {
        let prec = expr_prec(&e.kind);
        let paren = prec < min;
        if paren {
            self.w.token("(");
        }
        self.emit_kind(&e.kind, e.span);
        if paren {
            self.w.token(")");
        }
    }

    fn emit_kind(&mut self, kind: &ExprKind, span: Span) {
        match kind {
            ExprKind::Literal(l) => {
                if let Literal::String(v) = l
                    && string_lit_is_approximate(v, self.opts.version)
                {
                    self.semantic_loss(
                        span,
                        "this string literal",
                        "at v1 a value holding both `\"` and `'` has no single-literal \
                         form; the emitted literal decodes to a different string",
                    );
                }
                let s = literal_str(l, self.opts.version);
                self.w.token(&s);
            }
            ExprKind::Name(n) => {
                let s = self.name_ref(n);
                self.w.token(&s);
            }
            ExprKind::Binary(op, l, r) => {
                let (p, assoc) = prec::binary(*op);
                self.emit_expr(l, left_min(p, assoc));
                self.w.space();
                self.w.token(binary_str(*op));
                self.w.space();
                self.emit_expr(r, right_min(p, assoc));
            }
            ExprKind::Unary(op, x) => {
                self.w.token(unary_str(*op));
                self.emit_expr(x, PREC_PREFIX);
            }
            ExprKind::Postfix(op, x) => {
                self.emit_expr(x, PREC_POSTFIX);
                self.w.token(postfix_str(*op));
            }
            ExprKind::Call(c) => self.emit_call(c),
            ExprKind::Field(b, name, optional) => {
                self.emit_expr(b, PREC_CALL);
                self.w.token(if *optional { "?." } else { "." });
                self.w.token(name);
            }
            ExprKind::Index(b, i) => {
                self.emit_expr(b, PREC_CALL);
                self.w.token("[");
                self.emit_expr(i, 0);
                self.w.token("]");
            }
            ExprKind::Slice(s) => {
                self.emit_expr(&s.base, PREC_CALL);
                self.w.token("[");
                if let Some(st) = &s.start {
                    self.emit_expr(st, 0);
                }
                self.w.token(":");
                if let Some(en) = &s.end {
                    self.emit_expr(en, 0);
                }
                if let Some(step) = &s.step {
                    self.w.token(":");
                    self.emit_expr(step, 0);
                }
                self.w.token("]");
            }
            ExprKind::Array(items) => {
                self.w.token("[");
                for (i, e) in items.iter().enumerate() {
                    if i > 0 {
                        self.w.token(",");
                        self.w.space();
                    }
                    self.emit_expr(e, 0);
                }
                self.w.token("]");
            }
            ExprKind::Map(pairs) => {
                if pairs.is_empty() {
                    self.w.token("[:]");
                } else {
                    self.w.token("[");
                    for (i, (k, v)) in pairs.iter().enumerate() {
                        if i > 0 {
                            self.w.token(",");
                            self.w.space();
                        }
                        self.emit_expr(k, 0);
                        self.w.space();
                        self.w.token(":");
                        self.w.space();
                        self.emit_expr(v, 0);
                    }
                    self.w.token("]");
                }
            }
            ExprKind::Set(items) if items.is_empty() => {
                // `{}` re-parses as an empty *object*, not an empty set —
                // the grammar resolves that ambiguity in the object's favour
                // (docs/grammar.md §8.5). `<>` is the unambiguous spelling
                // and is what `<>` in the source lowered from anyway.
                self.w.token("<>");
            }
            ExprKind::Set(items) => {
                self.w.token("{");
                for (i, item) in items.iter().enumerate() {
                    if i > 0 {
                        self.w.token(",");
                        self.w.space();
                    }
                    self.emit_expr(&item.start, 0);
                    if let Some(end) = &item.end {
                        self.w.token("..");
                        self.emit_expr(end, 0);
                    }
                }
                self.w.token("}");
            }
            ExprKind::Object(fields) => {
                self.w.token("{");
                for (i, (name, v)) in fields.iter().enumerate() {
                    if i > 0 {
                        self.w.token(",");
                        self.w.space();
                    }
                    self.w.token(name);
                    self.w.token(":");
                    self.w.space();
                    self.emit_expr(v, 0);
                }
                self.w.token("}");
            }
            ExprKind::Ternary(c, t, e) => {
                self.emit_expr(c, PREC_TERNARY + 1);
                self.w.space();
                self.w.token("?");
                self.w.space();
                self.emit_expr(t, 0);
                self.w.space();
                self.w.token(":");
                self.w.space();
                self.emit_expr(e, PREC_TERNARY);
            }
            ExprKind::Interval(iv) => {
                self.w.token(if iv.start_inclusive { "[" } else { "]" });
                if let Some(s) = &iv.start {
                    self.emit_expr(s, 0);
                }
                self.w.token("..");
                if let Some(e) = &iv.end {
                    self.emit_expr(e, 0);
                }
                if let Some(step) = &iv.step {
                    self.w.token(":");
                    self.emit_expr(step, 0);
                }
                self.w.token(if iv.end_inclusive { "]" } else { "[" });
            }
            ExprKind::Cast(e, ty) => {
                self.emit_expr(e, PREC_POSTFIX);
                self.w.space();
                self.w.token("as");
                self.w.space();
                self.w.token(&type_str(ty));
            }
            ExprKind::New(n) => {
                self.w.token("new");
                self.w.space();
                self.w.token(&n.class);
                self.emit_arg_list(&n.args);
            }
            ExprKind::Lambda(l) => {
                self.emit_params(&l.params);
                self.w.space();
                self.w.token("->");
                self.w.space();
                match &l.body {
                    LambdaBody::Block(b) => self.emit_block(b),
                    LambdaBody::Expr(e) => self.emit_expr(e, 0),
                }
            }
        }
    }

    fn emit_call(&mut self, c: &Call) {
        match &c.callee {
            Callee::Function(n) => {
                let s = self.name_ref(n);
                self.w.token(&s);
            }
            Callee::Method {
                receiver,
                method,
                optional,
            } => {
                self.emit_expr(receiver, PREC_CALL);
                self.w.token(if *optional { "?." } else { "." });
                self.w.token(method);
            }
            Callee::Expr(e) => self.emit_expr(e, PREC_CALL),
        }
        self.emit_arg_list(&c.args);
    }

    fn emit_arg_list(&mut self, args: &[Expr]) {
        self.w.token("(");
        for (i, a) in args.iter().enumerate() {
            if i > 0 {
                self.w.token(",");
                self.w.space();
            }
            self.emit_expr(a, 0);
        }
        self.w.token(")");
    }

    fn name_ref(&self, n: &NameRef) -> String {
        match n {
            NameRef::Local(id) | NameRef::Global(id) | NameRef::Class(id) => self.def_name(*id),
            NameRef::Function(id) => self
                .names
                .get(id)
                .cloned()
                .unwrap_or_else(|| self.def_name(*id)),
            NameRef::Builtin(s) | NameRef::Unresolved(s) => s.clone(),
            NameRef::This => "this".to_string(),
            NameRef::Super => "super".to_string(),
            NameRef::Class_ => "class".to_string(),
        }
    }

    fn def_name(&self, id: leek_hir::DefId) -> String {
        self.hir
            .defs
            .get(id.0 as usize)
            .map_or_else(|| "null".to_string(), |d| d.name().to_string())
    }
}

fn literal_str(l: &Literal, version: Version) -> String {
    match l {
        Literal::Int(i) => i.to_string(),
        Literal::Real(r) => real_lit(*r),
        Literal::BigInt(digits) => format!("{digits}L"),
        Literal::String(s) => string_lit(s, version),
        Literal::Bool(b) => if *b { "true" } else { "false" }.to_string(),
        Literal::Null => "null".to_string(),
    }
}

fn expr_prec(kind: &ExprKind) -> u8 {
    match kind {
        ExprKind::Binary(op, _, _) => prec::binary(*op).0,
        ExprKind::Unary(..) => PREC_PREFIX,
        ExprKind::Postfix(..) | ExprKind::Cast(..) => PREC_POSTFIX,
        ExprKind::Ternary(..) => PREC_TERNARY,
        ExprKind::Lambda(_) => PREC_LAMBDA,
        // Atoms and postfix-chains (call/field/index/slice) bind tightly
        // enough to never need wrapping as a child.
        _ => PREC_ATOM,
    }
}

fn unary_str(op: UnaryOp) -> &'static str {
    match op {
        UnaryOp::Neg => "-",
        UnaryOp::Pos => "+",
        UnaryOp::Not => "!",
        UnaryOp::BitNot => "~",
        UnaryOp::PreInc => "++",
        UnaryOp::PreDec => "--",
        UnaryOp::Ref => "@",
    }
}

fn postfix_str(op: leek_hir::PostfixOp) -> &'static str {
    match op {
        leek_hir::PostfixOp::PostInc => "++",
        leek_hir::PostfixOp::PostDec => "--",
        leek_hir::PostfixOp::NonNull => "!",
    }
}

/// Render a real literal so it always re-lexes as a real (with a decimal
/// point or exponent).
///
/// Non-finite values go out as the names the language already has for them:
/// `∞` (U+221E, official syntax — it lowers straight back to
/// `Literal::Real(INFINITY)`) and the `NaN` builtin constant. They used to be
/// emitted as the arithmetic that produces them, `(1.0 / 0.0)` and
/// `(0.0 / 0.0)`, which is simply a different program at v1: division by zero
/// returns `null` there, so `return ∞;` round-tripped to `null` (#154).
fn real_lit(r: f64) -> String {
    if r.is_nan() {
        return "NaN".to_string();
    }
    if r.is_infinite() {
        return if r > 0.0 {
            "\u{221E}".to_string()
        } else {
            "-\u{221E}".to_string()
        };
    }
    // `{:?}` for f64 always includes a `.0` for integer-valued reals and
    // uses `e` notation only for very large/small magnitudes.
    format!("{r:?}")
}

/// Render a string literal with the escapes `version` will decode back to
/// the same value.
///
/// v1 keeps the backslash before a quote matching the delimiter
/// (`leek_text::EscapeMode::V1`): `"\""` reads back as the two characters
/// `\"`, so at v1 a `"` inside the value cannot be written with a
/// backslash at all — the literal switches to `'` delimiters instead. A v1
/// value holding *both* quote characters has no single-literal form; it
/// keeps the v2+ shape, which is the closest approximation available.
pub(crate) fn string_lit(s: &str, version: Version) -> String {
    // At v1 the delimiter is the only lever for embedding a quote, so pick
    // the one the value does not contain.
    let quote = if version == Version::V1 && s.contains('"') && !s.contains('\'') {
        '\''
    } else {
        '"'
    };
    let mut out = String::with_capacity(s.len() + 2);
    out.push(quote);
    for c in s.chars() {
        match c {
            '"' | '\'' if c == quote => {
                out.push('\\');
                out.push(c);
            }
            // The other quote needs no escape, and at v1 escaping it would
            // be wrong in the opposite direction (`\'` inside `"…"` decodes
            // to a bare `'`, but so does a plain `'`).
            '"' | '\'' => out.push(c),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '\r' => out.push_str("\\r"),
            c => out.push(c),
        }
    }
    out.push(quote);
    out
}

/// True when `version` has no faithful single-literal spelling for `s`.
///
/// Only v1, and only for a value holding *both* quote characters: there the
/// delimiter is the sole lever for embedding a quote (a backslash before the
/// delimiter reads back as the two characters `\"`), so one of the two is
/// necessarily wrong. [`string_lit`] emits the closest approximation and the
/// caller reports the loss.
pub(crate) fn string_lit_is_approximate(s: &str, version: Version) -> bool {
    version == Version::V1 && s.contains('"') && s.contains('\'')
}

/// The annotation to write back for a *declared* type, or `None` when this
/// backend has no faithful official spelling for it.
///
/// A whitelist, deliberately. HIR `Type`s cover shapes that official
/// LeekScript cannot spell — `Tuple` is experimental, a multi-parameter
/// `FunctionWithReturn` has no syntax in the type grammar at all, and a
/// `ClassInstance` carrying bound generic arguments needs a `class Box<T>`
/// declaration, which is feature-gated. Writing the nearest name for those
/// (what [`type_str`] does, because an `as` cast must produce *something*)
/// would silently change what the declaration means, which is the bug this
/// exists to fix — so they get `None` and the caller raises a diagnostic.
pub(crate) fn decl_type_str(ty: &Type) -> Option<String> {
    Some(match ty {
        Type::Any => "any".to_string(),
        Type::Void => "void".to_string(),
        Type::Boolean => "boolean".to_string(),
        Type::Integer => "integer".to_string(),
        Type::Real => "real".to_string(),
        Type::BigInteger => "big_integer".to_string(),
        Type::String => "string".to_string(),
        Type::Object => "Object".to_string(),
        Type::Interval => "Interval".to_string(),
        Type::Function => "Function".to_string(),
        // Generic *arguments* are official (`TypeArgs` carries no feature
        // gate); only the `<T>` on a declaration is experimental. An `Any`
        // element is what a bare `Array` parses to, so it round-trips either
        // way — write the shorter form.
        Type::Array(el) if **el == Type::Any => "Array".to_string(),
        Type::Array(el) => format!("Array<{}>", decl_type_str(el)?),
        Type::Set(el) if **el == Type::Any => "Set".to_string(),
        Type::Set(el) => format!("Set<{}>", decl_type_str(el)?),
        Type::Map(k, v) if **k == Type::Any && **v == Type::Any => "Map".to_string(),
        Type::Map(k, v) => format!("Map<{}, {}>", decl_type_str(k)?, decl_type_str(v)?),
        Type::ClassInstance(name, args) if args.is_empty() => name.clone(),
        Type::Nullable(inner) => format!("{}?", decl_type_str(inner)?),
        Type::Union(members) => {
            let mut out = String::new();
            for (i, m) in members.iter().enumerate() {
                if i > 0 {
                    out.push_str(" | ");
                }
                out.push_str(&decl_type_str(m)?);
            }
            out
        }
        Type::Null | Type::Tuple(_) | Type::FunctionWithReturn { .. } | Type::ClassInstance(..) => {
            return None;
        }
    })
}

/// Best-effort rendering of a type for `as` casts. Scalars are exact;
/// containers erase their element types (always valid, and casts of
/// containers are rare).
pub(crate) fn type_str(ty: &Type) -> String {
    match ty {
        Type::Any => "any".to_string(),
        Type::Null => "null".to_string(),
        Type::Void => "void".to_string(),
        Type::Boolean => "boolean".to_string(),
        Type::Integer => "integer".to_string(),
        Type::Real => "real".to_string(),
        Type::BigInteger => "big_integer".to_string(),
        Type::String => "string".to_string(),
        Type::Array(_) | Type::Tuple(_) => "Array".to_string(),
        Type::Map(_, _) => "Map".to_string(),
        Type::Set(_) => "Set".to_string(),
        Type::Object => "Object".to_string(),
        Type::ClassInstance(name, _) => name.clone(),
        Type::Function | Type::FunctionWithReturn { .. } => "Function".to_string(),
        Type::Interval => "Interval".to_string(),
        Type::Nullable(inner) => format!("{}?", type_str(inner)),
        Type::Union(_) => "any".to_string(),
    }
}
