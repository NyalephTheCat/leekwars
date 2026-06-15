//! Expression emission with precedence-driven re-parenthesization.

use leek_hir::{Call, Callee, Expr, ExprKind, LambdaBody, Literal, NameRef, UnaryOp};
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
        self.emit_kind(&e.kind);
        if paren {
            self.w.token(")");
        }
    }

    fn emit_kind(&mut self, kind: &ExprKind) {
        match kind {
            ExprKind::Literal(l) => {
                let s = literal_str(l);
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

fn literal_str(l: &Literal) -> String {
    match l {
        Literal::Int(i) => i.to_string(),
        Literal::Real(r) => real_lit(*r),
        Literal::BigInt(digits) => format!("{digits}L"),
        Literal::String(s) => string_lit(s),
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
/// point or exponent). Non-finite values are emitted as equivalent
/// arithmetic so the output stays valid source.
fn real_lit(r: f64) -> String {
    if r.is_nan() {
        return "(0.0 / 0.0)".to_string();
    }
    if r.is_infinite() {
        return if r > 0.0 {
            "(1.0 / 0.0)".to_string()
        } else {
            "(-1.0 / 0.0)".to_string()
        };
    }
    // `{:?}` for f64 always includes a `.0` for integer-valued reals and
    // uses `e` notation only for very large/small magnitudes.
    format!("{r:?}")
}

/// Render a string literal with the necessary escapes.
pub(crate) fn string_lit(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '\r' => out.push_str("\\r"),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Best-effort rendering of a type for `as` casts. Scalars are exact;
/// containers erase their element types (always valid, and casts of
/// containers are rare).
fn type_str(ty: &Type) -> String {
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
