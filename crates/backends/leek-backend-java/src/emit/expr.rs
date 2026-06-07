use leek_hir::{
    BinaryOp, Expr, ExprKind, IntervalExpr, Literal, NameRef, NewExpr, PostfixOp, SliceExpr,
    UnaryOp,
};
use leek_types::Type;
use std::fmt::Write as _;

use super::traits::EmitExpr;
use super::{
    Emitter, builtin_fn_wrapper, escape_string, expr_op_cost, is_div_expr, is_primitive_number,
    is_primitive_number_expr, is_string_expr, java_class_name, sanitize_ident, user_fn_wrapper,
};
use crate::mangle;

impl Emitter<'_> {
    pub(crate) fn expr_to_string(&self, e: &Expr) -> String {
        let mut buf = String::new();
        self.write_expr(&mut buf, e, /* parens_if_needed */ false);
        buf
    }

    /// Render an expression as a Java boolean. For literal booleans
    /// this is identity; for arbitrary Leek expressions we go through
    /// `LeekOperations.bool(this, expr)`.
    pub(crate) fn expr_to_bool(&self, e: &Expr) -> String {
        match &e.kind {
            ExprKind::Literal(Literal::Bool(b)) => b.to_string(),
            ExprKind::Binary(
                BinaryOp::Eq
                | BinaryOp::Ne
                | BinaryOp::IdentityEq
                | BinaryOp::IdentityNe
                | BinaryOp::Lt
                | BinaryOp::Le
                | BinaryOp::Gt
                | BinaryOp::Ge
                | BinaryOp::And
                | BinaryOp::Or
                | BinaryOp::In
                | BinaryOp::NotIn
                | BinaryOp::Is
                | BinaryOp::Instanceof,
                _,
                _,
            ) => self.expr_to_string(e),
            ExprKind::Unary(UnaryOp::Not, _) => self.expr_to_string(e),
            _ => format!("bool({})", self.expr_to_string(e)),
        }
    }
}

impl EmitExpr for Emitter<'_> {
    fn write_expr(&self, buf: &mut String, e: &Expr, parens_if_negative: bool) {
        match &e.kind {
            ExprKind::Literal(lit) => self.write_literal(buf, lit, parens_if_negative),
            ExprKind::Name(n) => self.write_name(buf, n),
            ExprKind::Binary(op, l, r) => self.write_binary(buf, *op, l, r, &e.ty),
            ExprKind::Unary(op, x) => self.write_unary(buf, *op, x, parens_if_negative),
            ExprKind::Postfix(op, x) => self.write_postfix(buf, *op, x),
            ExprKind::Call(c) => self.write_call(buf, c),
            ExprKind::Field(b, name) => {
                // Reference shape: `getField(base, "name", null)` —
                // an AI instance method. Direct `base.getField(...)`
                // would only work when `base` is statically a
                // `LeekValue` subtype; we'd hit `cannot find symbol`
                // on bare `Object` references otherwise.
                buf.push_str("getField(");
                self.write_expr(buf, b, false);
                buf.push_str(", \"");
                buf.push_str(name);
                buf.push_str("\", null)");
            }
            ExprKind::Index(b, i) => {
                // Reference shape: `get(base, index, null)` — a 3-arg
                // instance method on AI (third arg is the calling-
                // class for visibility checks; null is correct for
                // top-level array/map indexing).
                buf.push_str("get(");
                self.write_expr(buf, b, false);
                buf.push_str(", ");
                self.write_expr(buf, i, false);
                buf.push_str(", null)");
            }
            ExprKind::Slice(s) => self.write_slice(buf, s),
            ExprKind::Array(items) => self.write_array(buf, items),
            ExprKind::Map(pairs) => self.write_map(buf, pairs),
            ExprKind::Set(items) => self.write_set(buf, items),
            ExprKind::Object(fields) => self.write_object(buf, fields),
            ExprKind::Ternary(c, t, e) => self.write_ternary(buf, c, t, e),
            ExprKind::Interval(iv) => self.write_interval(buf, iv),
            ExprKind::Cast(x, ty) => {
                write!(buf, "(({}) ", java_class_name(ty)).unwrap();
                self.write_expr(buf, x, false);
                buf.push(')');
            }
            ExprKind::New(n) => self.write_new(buf, n),
            ExprKind::Lambda(l) => self.write_lambda(buf, l),
        }
    }
}

impl Emitter<'_> {
    pub(crate) fn write_name(&self, buf: &mut String, n: &NameRef) {
        match n {
            NameRef::Local(id) => {
                // Inside a self-recursive lambda body, references
                // to the in-construction var go through the
                // box-array so we don't read uninitialized memory.
                // See the `Supplier`-wrap in `write_lambda`.
                if Some(*id) == self.self_rec_def.get() {
                    buf.push_str("_self_box[0]");
                    return;
                }
                let name = self.def_name(*id).to_string();
                let mangled = mangle::local(self.opts, &name);
                if self.boxed_locals.borrow().contains(id) {
                    // Captured-and-written by a nested lambda → shared via a
                    // one-element `Object[]`; every read/write goes through `[0]`.
                    buf.push_str(&mangled);
                    buf.push_str("[0]");
                } else {
                    buf.push_str(&mangled);
                }
            }
            NameRef::Global(id) => {
                let name = self.def_name(*id).to_string();
                buf.push_str(&mangle::global(self.opts, &name));
            }
            NameRef::Function(id) => {
                // In call position (`f(args)`) `write_call` produces
                // the `f_name(args)` form directly — we never reach
                // here. So a NameRef::Function arriving in `write_name`
                // is a first-class reference (`[f, g]`, `arr[0]`'s
                // value of `f`, `var x = f`, etc.) and needs to be
                // wrapped in a `FunctionLeekValue`.
                let name = self.def_name(*id).to_string();
                let mangled = mangle::function(self.opts, &name);
                let arity = self.user_fn_arity(*id);
                buf.push_str(&user_fn_wrapper(&mangled, arity));
            }
            NameRef::Class(id) => {
                let name = self.def_name(*id).to_string();
                buf.push_str(&mangle::class_name(self.opts, &name));
            }
            NameRef::Builtin(name) => {
                // If the source ever reassigns this builtin name
                // (`push = 1` etc.), check the `__shadows` map
                // first and fall through to the original builtin
                // ref only when the user hasn't set it. The
                // fallback is the original emission below, wrapped
                // in a ternary.
                if self.shadowed_builtins.borrow().contains(name) {
                    buf.push_str("(__shadows.containsKey(\"");
                    buf.push_str(name);
                    buf.push_str("\") ? __shadows.get(\"");
                    buf.push_str(name);
                    buf.push_str("\") : (");
                    // Recurse with a temporary "no-shadow" so we
                    // don't infinitely re-enter this branch.
                    let mut tmp_shadow = self.shadowed_builtins.borrow_mut();
                    let prev: Vec<String> = tmp_shadow.iter().cloned().collect();
                    tmp_shadow.clear();
                    drop(tmp_shadow);
                    self.write_name(buf, n);
                    let mut tmp_shadow = self.shadowed_builtins.borrow_mut();
                    tmp_shadow.extend(prev);
                    drop(tmp_shadow);
                    buf.push_str("))");
                    return;
                }
                // System constants render to their Java counterparts.
                // Built-in class names (`Real`, `Number`, `String`, …)
                // map to the reference's lowercase-singleton form
                // (`realClass`, `numberClass`, etc.) — those are
                // pre-instantiated `ClassLeekValue` references on
                // the AI runtime.
                match name.as_str() {
                    "PI" => buf.push_str("Math.PI"),
                    "E" => buf.push_str("Math.E"),
                    "Infinity" => buf.push_str("Double.POSITIVE_INFINITY"),
                    "NaN" => buf.push_str("Double.NaN"),
                    "Real" | "Number" | "Integer" | "String" | "Boolean" | "Array" | "Map"
                    | "Set" | "Object" | "Null" | "Function" | "Interval" | "Value" | "Class"
                    | "JSON" | "System" | "Color" => {
                        buf.push_str(&name.to_lowercase());
                        buf.push_str("Class");
                    }
                    other => {
                        // First-class reference to a builtin function
                        // (`var f = cos` / `arrayMap([1,2,3], cos)`).
                        // Mirrors upstream's `writeAnonymousSystemFunctions`,
                        // which synthesizes a per-AI `FunctionLeekValue` for
                        // every builtin that escapes its call shape — except
                        // we emit it inline at the use site so we don't
                        // need to thread "used builtins" state through the
                        // emitter.
                        if let Some(snippet) = builtin_fn_wrapper(other, self.opts.version) {
                            buf.push_str(&snippet);
                        } else {
                            buf.push_str(other);
                        }
                    }
                }
            }
            NameRef::This => buf.push_str("this"),
            NameRef::Super => buf.push_str("super"),
            NameRef::Class_ => buf.push_str("this.getClass()"),
            NameRef::Unresolved(name) => {
                // Fall back to the mangled local form so the surrounding
                // code still parses. The interpreter would surface the
                // unresolved diagnostic separately.
                buf.push_str(&mangle::local(self.opts, name));
            }
        }
    }

    // ---- binary ops --------------------------------------------------------

    pub(crate) fn write_binary(
        &self,
        buf: &mut String,
        op: BinaryOp,
        l: &Expr,
        r: &Expr,
        result_ty: &Type,
    ) {
        if op.is_assignment() {
            self.write_assignment(buf, op, l, r);
            return;
        }
        match op {
            BinaryOp::Add => self.write_arith_or_concat(buf, l, r, "+", "add", result_ty),
            BinaryOp::Sub => self.write_arith(buf, l, r, "-", "sub", result_ty),
            BinaryOp::Mul => self.write_arith(buf, l, r, "*", "mul", result_ty),
            BinaryOp::Div => {
                // Reference: `div(a, b)` — instance method on AI,
                // not `LeekOperations.div(this, ...)`. Returns `double`.
                // v1 has a `div_v1` that returns `null` on
                // divide-by-zero (corpus pin: `8 / 0 → null`).
                // Route v1 through it; later versions keep the
                // double-returning `div`.
                let _ = result_ty;
                let helper = if matches!(self.opts.version, leek_syntax::Version::V1) {
                    "div_v1"
                } else {
                    "div"
                };
                buf.push_str(helper);
                buf.push('(');
                if Self::either_null(l, r) {
                    self.write_arith_operand_object(buf, l);
                    buf.push_str(", ");
                    self.write_arith_operand_object(buf, r);
                } else {
                    self.write_arith_operand(buf, l);
                    buf.push_str(", ");
                    self.write_arith_operand(buf, r);
                }
                buf.push(')');
            }
            BinaryOp::Mod => self.write_arith(buf, l, r, "%", "mod", result_ty),
            BinaryOp::IntDiv => {
                buf.push('(');
                self.write_as_long(buf, l);
                buf.push_str(" / ");
                self.write_as_long(buf, r);
                buf.push(')');
            }
            BinaryOp::Pow => {
                buf.push_str("pow(");
                if Self::either_null(l, r) {
                    self.write_arith_operand_object(buf, l);
                    buf.push_str(", ");
                    self.write_arith_operand_object(buf, r);
                } else {
                    self.write_arith_operand(buf, l);
                    buf.push_str(", ");
                    self.write_arith_operand(buf, r);
                }
                buf.push(')');
            }
            BinaryOp::Eq => {
                buf.push_str("equals_equals(");
                self.write_expr(buf, l, false);
                buf.push_str(", ");
                self.write_expr(buf, r, false);
                buf.push(')');
            }
            BinaryOp::Ne => {
                buf.push_str("notequals_equals(");
                self.write_expr(buf, l, false);
                buf.push_str(", ");
                self.write_expr(buf, r, false);
                buf.push(')');
            }
            BinaryOp::IdentityEq => {
                // Java's `==` only compiles for compatible types.
                // At v1 the `div_v1` helper returns Object (null
                // on divide-by-zero); comparing it against
                // `Double.NaN` (primitive double) is a javac
                // error. Wrap both sides via an Object cast — the
                // identity semantics are preserved (reference
                // equality on the box) and javac is satisfied.
                let needs_object_box = matches!(self.opts.version, leek_syntax::Version::V1)
                    && (is_div_expr(l) || is_div_expr(r));
                if needs_object_box {
                    buf.push_str("java.util.Objects.equals((Object)(");
                    self.write_expr(buf, l, true);
                    buf.push_str("), (Object)(");
                    self.write_expr(buf, r, true);
                    buf.push_str("))");
                } else {
                    self.write_expr(buf, l, true);
                    buf.push_str(" == ");
                    self.write_expr(buf, r, true);
                }
            }
            BinaryOp::IdentityNe => {
                let needs_object_box = matches!(self.opts.version, leek_syntax::Version::V1)
                    && (is_div_expr(l) || is_div_expr(r));
                if needs_object_box {
                    buf.push_str("!java.util.Objects.equals((Object)(");
                    self.write_expr(buf, l, true);
                    buf.push_str("), (Object)(");
                    self.write_expr(buf, r, true);
                    buf.push_str("))");
                } else {
                    self.write_expr(buf, l, true);
                    buf.push_str(" != ");
                    self.write_expr(buf, r, true);
                }
            }
            BinaryOp::Lt => self.write_compare(buf, l, r, "<", "less"),
            BinaryOp::Le => self.write_compare(buf, l, r, "<=", "lessequals"),
            BinaryOp::Gt => self.write_compare(buf, l, r, ">", "more"),
            BinaryOp::Ge => self.write_compare(buf, l, r, ">=", "moreequals"),
            BinaryOp::And => {
                buf.push('(');
                buf.push_str(&self.expr_to_bool(l));
                buf.push_str(" && ");
                buf.push_str(&self.expr_to_bool(r));
                buf.push(')');
            }
            BinaryOp::Or => {
                buf.push('(');
                buf.push_str(&self.expr_to_bool(l));
                buf.push_str(" || ");
                buf.push_str(&self.expr_to_bool(r));
                buf.push(')');
            }
            BinaryOp::Xor => {
                buf.push('(');
                buf.push_str(&self.expr_to_bool(l));
                buf.push_str(" ^ ");
                buf.push_str(&self.expr_to_bool(r));
                buf.push(')');
            }
            BinaryOp::BitAnd => self.write_bit(buf, l, r, "&"),
            BinaryOp::BitOr => self.write_bit(buf, l, r, "|"),
            BinaryOp::BitXor => self.write_bit(buf, l, r, "^"),
            BinaryOp::ShiftL => self.write_bit(buf, l, r, "<<"),
            BinaryOp::ShiftR => self.write_bit(buf, l, r, ">>"),
            BinaryOp::UShiftR => self.write_bit(buf, l, r, ">>>"),
            BinaryOp::NullCoalesce => {
                // `a ?? b`: reference shape is `load(a) != null ? a : b`.
                // `load(...)` is an instance method on `AI` that handles
                // any box/unbox the LHS might be wrapped in.
                buf.push_str("(load(");
                self.write_expr(buf, l, false);
                buf.push_str(") != null ? ");
                self.write_expr(buf, l, false);
                buf.push_str(" : ");
                self.write_expr(buf, r, false);
                buf.push(')');
            }
            BinaryOp::In => {
                // `x in c`: AI has `contains(haystack, needle)` as an
                // instance method.
                buf.push_str("contains(");
                self.write_expr(buf, r, false);
                buf.push_str(", ");
                self.write_expr(buf, l, false);
                buf.push(')');
            }
            BinaryOp::NotIn => {
                buf.push_str("!contains(");
                self.write_expr(buf, r, false);
                buf.push_str(", ");
                self.write_expr(buf, l, false);
                buf.push(')');
            }
            BinaryOp::Is | BinaryOp::Instanceof => {
                self.write_expr(buf, l, false);
                buf.push_str(" instanceof ");
                buf.push_str(&self.expr_to_string(r));
            }
            _ => unreachable!("assignment ops handled above"),
        }
    }

    pub(crate) fn write_arith_or_concat(
        &self,
        buf: &mut String,
        l: &Expr,
        r: &Expr,
        java_op: &str,
        runtime_fn: &str,
        result_ty: &Type,
    ) {
        // String concatenation uses the same `add(...)` helper as
        // numeric `+` but with a `(String)` cast prefix instead of
        // `(Object)`. Detected via either HIR type info or the
        // syntactic literal form.
        if is_string_expr(l) || is_string_expr(r) {
            buf.push_str("(String) add(");
            self.write_expr(buf, l, false);
            buf.push_str(", ");
            self.write_expr(buf, r, false);
            buf.push(')');
            return;
        }
        self.write_arith(buf, l, r, java_op, runtime_fn, result_ty);
    }

    pub(crate) fn write_arith(
        &self,
        buf: &mut String,
        l: &Expr,
        r: &Expr,
        java_op: &str,
        runtime_fn: &str,
        _result_ty: &Type,
    ) {
        if is_primitive_number_expr(l) && is_primitive_number_expr(r) {
            // Inline Java arithmetic. Bool literals can't appear as
            // operands to `*`/`-`/etc. in Java, so coerce them to
            // `1l`/`0l` first — matches Leek's promote-bool-to-int
            // semantics.
            self.write_primitive_arith_operand(buf, l);
            buf.push(' ');
            buf.push_str(java_op);
            buf.push(' ');
            self.write_primitive_arith_operand(buf, r);
        } else {
            // `(Object) add(a, b)` — cast-prefixed runtime helper.
            // Object is the conservative result type; narrower
            // casts are emitted by the surrounding context.
            buf.push_str("(Object) ");
            buf.push_str(runtime_fn);
            buf.push('(');
            if Self::either_null(l, r) {
                self.write_arith_operand_object(buf, l);
                buf.push_str(", ");
                self.write_arith_operand_object(buf, r);
            } else {
                self.write_arith_operand(buf, l);
                buf.push_str(", ");
                self.write_arith_operand(buf, r);
            }
            buf.push(')');
        }
    }

    /// Write a binary-helper argument. When either side of the call
    /// is a null literal, callers force both operands through
    /// [`write_arith_operand_object`] so the `(Object, Object)`
    /// runtime overload is picked unambiguously (and the helper's
    /// own null-handling kicks in instead of an NPE at unbox time).
    pub(crate) fn write_arith_operand(&self, buf: &mut String, e: &Expr) {
        if matches!(&e.kind, ExprKind::Literal(Literal::Null)) {
            // Force a single null literal to the Long overload —
            // happens when only one side is null and we still want
            // primitive dispatch. The two-null case is handled by
            // [`Self::either_null`] in the caller.
            buf.push_str("(Long) null");
        } else {
            self.write_expr(buf, e, false);
        }
    }

    /// Variant of [`write_arith_operand`] that always emits an
    /// `(Object)` cast. Used when one operand is null and the other
    /// is a primitive — the runtime helpers (`add`/`sub`/`mul`/`pow`/
    /// `mod`) have an `(Object, Object)` overload that returns null
    /// cleanly on null inputs, but Java's overload resolution can't
    /// pick between `pow(long, long)` and `pow(Long, long)` when one
    /// side is `Long` and the other primitive. Casting both to Object
    /// resolves the ambiguity.
    pub(crate) fn write_arith_operand_object(&self, buf: &mut String, e: &Expr) {
        buf.push_str("(Object) ");
        if matches!(&e.kind, ExprKind::Literal(Literal::Null)) {
            buf.push_str("null");
        } else {
            // Primitive operands need to be boxed first. `(Object) 14L`
            // is a valid Java cast (autoboxes `14L` to `Long` then
            // upcasts), but `(Object) (long) ...` isn't. Letting
            // `write_expr` handle the literal keeps the form
            // `Long`-friendly.
            self.write_expr(buf, e, true);
        }
    }

    /// True when either operand is a null literal — caller's signal
    /// to use the `(Object, Object)` runtime overload.
    pub(crate) fn either_null(l: &Expr, r: &Expr) -> bool {
        matches!(&l.kind, ExprKind::Literal(Literal::Null))
            || matches!(&r.kind, ExprKind::Literal(Literal::Null))
    }

    /// Write a primitive-arithmetic operand, promoting bool literals
    /// to long since Java's `*`/`-`/etc. don't accept `boolean`.
    pub(crate) fn write_primitive_arith_operand(&self, buf: &mut String, e: &Expr) {
        if let ExprKind::Literal(Literal::Bool(b)) = &e.kind {
            buf.push_str(if *b { "1l" } else { "0l" });
        } else {
            self.write_expr(buf, e, true);
        }
    }

    pub(crate) fn write_compare(
        &self,
        buf: &mut String,
        l: &Expr,
        r: &Expr,
        java_op: &str,
        runtime_fn: &str,
    ) {
        if is_primitive_number_expr(l) && is_primitive_number_expr(r) {
            self.write_expr(buf, l, true);
            buf.push(' ');
            buf.push_str(java_op);
            buf.push(' ');
            self.write_expr(buf, r, true);
        } else {
            // Dedicated runtime helpers: `less(a, b)`, `more(a, b)`,
            // `less_equals`, `more_equals` — match the reference's
            // method names directly. No `LeekOperations.compare`
            // detour.
            buf.push_str(runtime_fn);
            buf.push('(');
            self.write_expr(buf, l, false);
            buf.push_str(", ");
            self.write_expr(buf, r, false);
            buf.push(')');
        }
    }

    pub(crate) fn write_bit(&self, buf: &mut String, l: &Expr, r: &Expr, op: &str) {
        buf.push('(');
        self.write_as_long(buf, l);
        buf.push(' ');
        buf.push_str(op);
        buf.push(' ');
        self.write_as_long(buf, r);
        buf.push(')');
    }

    pub(crate) fn write_as_long(&self, buf: &mut String, e: &Expr) {
        if is_primitive_number(&e.ty) {
            self.write_expr(buf, e, true);
        } else {
            buf.push_str("((Number) (");
            self.write_expr(buf, e, false);
            buf.push_str(")).longValue()");
        }
    }

    pub(crate) fn write_assignment(&self, buf: &mut String, op: BinaryOp, l: &Expr, r: &Expr) {
        // Index l-value? Route through the reference's `put*` helpers
        // — those are the only way to assign back through an
        // `Object`-typed array/map binding in Java.
        if let ExprKind::Index(base, idx) = &l.kind {
            self.write_index_assignment(buf, op, base, idx, r);
            return;
        }
        // `count = function(...)`, `push = 1` — assignment whose
        // l-value is a builtin name. There's no Java variable named
        // `count`/`push` to assign into; without an HIR-level shadow
        // rewrite we can't faithfully track the new binding. Emit
        // just the r-value so the program at least compiles — the
        // RHS's side effects still happen and subsequent reads of
        // the builtin name read the original builtin (value
        // mismatch, not a compile error).
        // `this.field = value` — emit as a direct Java field write
        // (`<base>.<field> = value`) rather than going through the
        // read-side `getField(...)` helper, which isn't assignable.
        if let ExprKind::Field(base, fname) = &l.kind {
            self.write_expr(buf, base, true);
            buf.push('.');
            buf.push_str(&sanitize_ident(fname));
            buf.push_str(" = ");
            self.write_expr(buf, r, false);
            return;
        }
        if let ExprKind::Name(NameRef::Builtin(name)) = &l.kind {
            // Builtin reassign — route through the `__shadows`
            // map field on the AI class. v1 allows shadowing
            // builtin names; subsequent reads via `write_name`
            // see the user's value.
            if self.shadowed_builtins.borrow().contains(name) {
                buf.push_str("__shadows.put(\"");
                buf.push_str(name);
                buf.push_str("\", ");
                self.write_expr(buf, r, false);
                buf.push(')');
                return;
            }
        }
        if matches!(
            &l.kind,
            ExprKind::Name(NameRef::Builtin(_) | NameRef::Function(_) | NameRef::Class(_))
        ) {
            // Assignment to a function/class name (no shadow
            // tracking yet for these — the resolver already
            // errors at v4). Emit only the RHS so the program
            // compiles.
            self.write_expr(buf, r, false);
            return;
        }
        // Compound forms decompose to `l = (l <op> r)` because the
        // type-narrowing on the result might be different from the
        // l-value's declared type. Plain `=` is a straight assign.
        if let Some(base) = op.compound_base() {
            // Synthesize a non-compound binary so write_binary handles
            // promotion / concat / runtime-fn routing for us.
            let expanded = Expr {
                kind: ExprKind::Binary(base, Box::new(l.clone()), Box::new(r.clone())),
                ty: l.ty.clone(),
                span: l.span,
            };
            self.write_expr(buf, l, false);
            buf.push_str(" = ");
            self.write_expr(buf, &expanded, false);
        } else {
            // Treat `<local> = lambda` the same way as `var <local> =
            // lambda`: prime `initializing_def` so the lambda emit
            // detects self-recursion and wraps with the Supplier-box
            // pattern. Without this the `var aux; aux = function(...)
            // { aux(...) }` shape goes through the outlined-factory
            // path, which captures `u_aux` at the (null) initial
            // value and recursive calls dispatch to null.
            let prev = self.initializing_def.get();
            if let ExprKind::Name(NameRef::Local(id)) = &l.kind {
                self.initializing_def.set(Some(*id));
            }
            self.write_expr(buf, l, false);
            buf.push_str(" = ");
            self.write_expr(buf, r, false);
            self.initializing_def.set(prev);
        }
    }

    /// Emit `put*(base, idx, value, null)` — the reference's idiom
    /// for writing through an indexed l-value. The fourth `null` is
    /// the calling-class for visibility checks; top-level array/map
    /// writes pass null.
    pub(crate) fn write_index_assignment(
        &self,
        buf: &mut String,
        op: BinaryOp,
        base: &Expr,
        idx: &Expr,
        value: &Expr,
    ) {
        let helper = match op {
            BinaryOp::Assign => "putv4",
            BinaryOp::AddAssign => "put_add_eq",
            BinaryOp::SubAssign => "put_sub_eq",
            BinaryOp::MulAssign => "put_mul_eq",
            BinaryOp::DivAssign => "put_div_eq",
            BinaryOp::IntDivAssign => "put_div_eq",
            BinaryOp::ModAssign => "put_mod_eq",
            BinaryOp::PowAssign => "put_pow_eq",
            BinaryOp::BitAndAssign => "put_band_eq",
            BinaryOp::BitOrAssign => "put_bor_eq",
            BinaryOp::BitXorAssign => "put_bxor_eq",
            BinaryOp::ShiftLAssign => "put_shl_eq",
            BinaryOp::ShiftRAssign => "put_shr_eq",
            BinaryOp::UShiftRAssign => "put_shr_eq",
            BinaryOp::NullCoalesceAssign => "putv4",
            _ => "putv4",
        };
        buf.push_str(helper);
        buf.push('(');
        self.write_expr(buf, base, false);
        buf.push_str(", ");
        self.write_expr(buf, idx, false);
        buf.push_str(", ");
        self.write_expr(buf, value, false);
        buf.push_str(", null)");
    }

    // ---- unary / postfix ---------------------------------------------------

    pub(crate) fn write_unary(
        &self,
        buf: &mut String,
        op: UnaryOp,
        x: &Expr,
        parens_if_negative: bool,
    ) {
        match op {
            UnaryOp::Neg => {
                if is_primitive_number_expr(x) {
                    if parens_if_negative {
                        buf.push('(');
                    }
                    buf.push('-');
                    self.write_expr(buf, x, true);
                    if parens_if_negative {
                        buf.push(')');
                    }
                } else {
                    // Reference's name is bare `minus(...)`, lifted
                    // from `AI`'s instance methods.
                    buf.push_str("minus(");
                    self.write_expr(buf, x, false);
                    buf.push(')');
                }
            }
            UnaryOp::Pos => self.write_expr(buf, x, parens_if_negative),
            UnaryOp::Not => {
                buf.push('!');
                buf.push_str(&self.expr_to_bool(x));
            }
            UnaryOp::BitNot => {
                buf.push_str("(~");
                self.write_as_long(buf, x);
                buf.push(')');
            }
            UnaryOp::PreInc => {
                // Reference shape: `u_x = add(u_x, 1l)`. The `add`/
                // `sub` methods are instance methods on `AI` so bare
                // names resolve correctly inside an AI subclass.
                self.write_expr(buf, x, false);
                buf.push_str(" = add(");
                self.write_expr(buf, x, false);
                buf.push_str(", 1l)");
            }
            UnaryOp::PreDec => {
                self.write_expr(buf, x, false);
                buf.push_str(" = sub(");
                self.write_expr(buf, x, false);
                buf.push_str(", 1l)");
            }
            UnaryOp::Ref => self.write_expr(buf, x, parens_if_negative),
        }
    }

    pub(crate) fn write_postfix(&self, buf: &mut String, op: PostfixOp, x: &Expr) {
        match op {
            PostfixOp::PostInc => {
                // `a++` = pre-value of `a` after assignment. Trick from
                // the Java reference: compute `add(a, 1)`, assign it to
                // `a`, then `sub(...,1)` to recover the original.
                buf.push_str("sub(");
                self.write_expr(buf, x, false);
                buf.push_str(" = add(");
                self.write_expr(buf, x, false);
                buf.push_str(", 1l), 1l)");
            }
            PostfixOp::PostDec => {
                buf.push_str("add(");
                self.write_expr(buf, x, false);
                buf.push_str(" = sub(");
                self.write_expr(buf, x, false);
                buf.push_str(", 1l), 1l)");
            }
            PostfixOp::NonNull => {
                // `x!` — assert non-null. The reference just emits the
                // expression bare and relies on runtime null checks
                // at the next use site.
                self.write_expr(buf, x, false);
            }
        }
    }

    // ---- collection literals ----------------------------------------------

    pub(crate) fn write_array(&self, buf: &mut String, items: &[Expr]) {
        // Version split mirrors `LegacyLeekArray.writeJavaCode` /
        // `LeekExpression.writeJavaCode` upstream: v4 instantiates the
        // strict `ArrayLeekValue`, v1–v3 the looser `LegacyArrayLeekValue`
        // which also stands in for `Map` pre-v4.
        let class = if matches!(self.opts.version, leek_syntax::Version::V4) {
            "ArrayLeekValue"
        } else {
            "LegacyArrayLeekValue"
        };
        if items.is_empty() {
            buf.push_str("new ");
            buf.push_str(class);
            buf.push('(');
            buf.push_str(self.ai_this());
            buf.push(')');
            return;
        }
        buf.push_str("new ");
        buf.push_str(class);
        buf.push('(');
        buf.push_str(self.ai_this());
        buf.push_str(", new Object[] { ");
        for (i, it) in items.iter().enumerate() {
            if i > 0 {
                buf.push_str(", ");
            }
            self.write_expr(buf, it, false);
        }
        buf.push_str(" })");
    }

    pub(crate) fn write_map(&self, buf: &mut String, pairs: &[(Expr, Expr)]) {
        // v1–v3: maps reuse `LegacyArrayLeekValue` (single backing
        // store for both array and map shapes). v4: dedicated
        // `MapLeekValue` with ordered keys.
        let class = if matches!(self.opts.version, leek_syntax::Version::V4) {
            "MapLeekValue"
        } else {
            "LegacyArrayLeekValue"
        };
        if pairs.is_empty() {
            buf.push_str("new ");
            buf.push_str(class);
            buf.push_str("(this)");
            return;
        }
        buf.push_str("new ");
        buf.push_str(class);
        buf.push_str("(this, new Object[] { ");
        for (i, (k, v)) in pairs.iter().enumerate() {
            if i > 0 {
                buf.push_str(", ");
            }
            self.write_expr(buf, k, false);
            buf.push_str(", ");
            self.write_expr(buf, v, false);
        }
        buf.push_str(" })");
    }

    pub(crate) fn write_set(&self, buf: &mut String, items: &[Expr]) {
        buf.push_str("new SetLeekValue(");
        buf.push_str(self.ai_this());
        buf.push_str(", new Object[] { ");
        for (i, it) in items.iter().enumerate() {
            if i > 0 {
                buf.push_str(", ");
            }
            self.write_expr(buf, it, false);
        }
        buf.push_str(" })");
    }

    pub(crate) fn write_object(&self, buf: &mut String, fields: &[(String, Expr)]) {
        // Reference shape: `new ObjectLeekValue(this, new String[]{...keys...}, new Object[]{...values...})`.
        // Keys are statically known so they go through `String[]`, not
        // a flat alternating `Object[]`.
        buf.push_str("new ObjectLeekValue(");
        buf.push_str(self.ai_this());
        buf.push_str(", new String[] { ");
        for (i, (k, _)) in fields.iter().enumerate() {
            if i > 0 {
                buf.push_str(", ");
            }
            buf.push('"');
            buf.push_str(&escape_string(k, true));
            buf.push('"');
        }
        buf.push_str(" }, new Object[] { ");
        for (i, (_, v)) in fields.iter().enumerate() {
            if i > 0 {
                buf.push_str(", ");
            }
            self.write_expr(buf, v, false);
        }
        buf.push_str(" })");
    }

    pub(crate) fn write_ternary(&self, buf: &mut String, c: &Expr, t: &Expr, e: &Expr) {
        // Reference shape (from `LeekTernaire.writeJavaCode`):
        // `cond ? then : else` with each branch optionally wrapped
        // in `ops(BRANCH, branch_cost)` when the two branches have
        // different costs — keeps the runtime accounting honest
        // depending on which arm is taken. No outer parens.
        buf.push_str(&self.expr_to_bool(c));
        buf.push_str(" ? ");
        let then_cost = expr_op_cost(t);
        let else_cost = expr_op_cost(e);
        let branch_ops = self.opts.emit_ops && then_cost != else_cost;
        if branch_ops && then_cost > 0 {
            buf.push_str("ops(");
            self.write_expr(buf, t, false);
            write!(buf, ", {then_cost})").unwrap();
        } else {
            self.write_expr(buf, t, false);
        }
        buf.push_str(" : ");
        if branch_ops && else_cost > 0 {
            buf.push_str("ops(");
            self.write_expr(buf, e, false);
            write!(buf, ", {else_cost})").unwrap();
        } else {
            self.write_expr(buf, e, false);
        }
    }

    pub(crate) fn write_interval(&self, buf: &mut String, iv: &IntervalExpr) {
        // Reference shape:
        //   `new IntegerIntervalLeekValue(this, minClosed, from, maxClosed, to)`
        // `IntervalLeekValue` itself is abstract — pick Integer- or
        // Real- variant based on whether any endpoint is a real
        // literal. Step is part of runtime semantics, not the ctor.
        let any_real = [iv.start.as_deref(), iv.end.as_deref(), iv.step.as_deref()]
            .into_iter()
            .flatten()
            .any(|e| matches!(&e.kind, ExprKind::Literal(Literal::Real(_))));
        let cls = if any_real {
            "RealIntervalLeekValue"
        } else {
            "IntegerIntervalLeekValue"
        };
        let default_endpoint = if any_real { "0.0" } else { "0l" };
        buf.push_str("new ");
        buf.push_str(cls);
        buf.push('(');
        buf.push_str(self.ai_this());
        buf.push_str(", ");
        buf.push_str(if iv.start_inclusive { "true" } else { "false" });
        buf.push_str(", ");
        match &iv.start {
            Some(e) => self.write_expr(buf, e, false),
            None => buf.push_str(default_endpoint),
        }
        buf.push_str(", ");
        buf.push_str(if iv.end_inclusive { "true" } else { "false" });
        buf.push_str(", ");
        match &iv.end {
            Some(e) => self.write_expr(buf, e, false),
            None => buf.push_str(default_endpoint),
        }
        buf.push(')');
    }

    pub(crate) fn write_slice(&self, buf: &mut String, s: &SliceExpr) {
        buf.push_str("LeekOperations.slice(");
        buf.push_str(self.ai_this());
        buf.push_str(", ");
        self.write_expr(buf, &s.base, false);
        buf.push_str(", ");
        match &s.start {
            Some(e) => self.write_expr(buf, e, false),
            None => buf.push_str("null"),
        }
        buf.push_str(", ");
        match &s.end {
            Some(e) => self.write_expr(buf, e, false),
            None => buf.push_str("null"),
        }
        buf.push_str(", ");
        match &s.step {
            Some(e) => self.write_expr(buf, e, false),
            None => buf.push_str("null"),
        }
        buf.push(')');
    }

    pub(crate) fn write_new(&self, buf: &mut String, n: &NewExpr) {
        // Built-in primitive classes (`Integer`, `Real`, `Number`,
        // `Boolean`) have no Java constructor — the upstream emitter
        // collapses `new Integer()` to the type's default literal
        // (`0l` / `0.0` / `false`). Mirror that to avoid emitting
        // `new u_Integer()` for a class that doesn't exist.
        // See `LeekFunctionCall.compileL`'s `ClassValueType` arm.
        match n.class.as_str() {
            "Integer" => {
                buf.push_str("0l");
                return;
            }
            "Real" | "Number" => {
                buf.push_str("0.0");
                return;
            }
            "Boolean" => {
                buf.push_str("false");
                return;
            }
            _ => {}
        }
        buf.push_str("new ");
        buf.push_str(&mangle::class_name(self.opts, &n.class));
        buf.push('(');
        for (i, a) in n.args.iter().enumerate() {
            if i > 0 {
                buf.push_str(", ");
            }
            self.write_expr(buf, a, false);
        }
        buf.push(')');
    }
}
