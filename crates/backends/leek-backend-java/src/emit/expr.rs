use leek_hir::{
    BinaryOp, Callee, Def, Expr, ExprKind, IntervalExpr, Literal, NameRef, NewExpr, PostfixOp,
    SliceExpr, UnaryOp,
};
use leek_types::Type;
use std::fmt::Write as _;

use super::traits::EmitExpr;
use super::{
    Emitter, builtin_fn_wrapper, escape_string, is_div_expr, is_primitive_number,
    is_primitive_number_expr, is_string_expr, java_class_name, user_fn_wrapper,
};
use crate::mangle;

/// The math builtins whose result is statically numeric (`real`/`integer`) —
/// the same set as `leek_runtime::math_sig`. Used to decide whether a value
/// assigned into a typed numeric array can be coerced (see
/// [`Emitter::expr_is_numeric`]).
fn is_numeric_math_builtin(name: &str) -> bool {
    matches!(
        name,
        "sqrt"
            | "cbrt"
            | "sin"
            | "cos"
            | "tan"
            | "asin"
            | "acos"
            | "atan"
            | "sinh"
            | "cosh"
            | "tanh"
            | "exp"
            | "log"
            | "log10"
            | "log2"
            | "floor"
            | "ceil"
            | "round"
            | "pow"
            | "atan2"
            | "hypot"
    )
}

/// The reflective `field_<op>_eq` helper for a compound assignment to an
/// external object field (`a.f += r`). `None` for a plain `=`.
fn field_compound_helper(op: BinaryOp) -> Option<&'static str> {
    Some(match op {
        BinaryOp::AddAssign => "field_add_eq",
        BinaryOp::SubAssign => "field_sub_eq",
        BinaryOp::MulAssign => "field_mul_eq",
        BinaryOp::DivAssign => "field_div_eq",
        BinaryOp::IntDivAssign => "field_intdiv_eq",
        BinaryOp::ModAssign => "field_mod_eq",
        BinaryOp::PowAssign => "field_pow_eq",
        BinaryOp::BitAndAssign => "field_band_eq",
        BinaryOp::BitOrAssign => "field_bor_eq",
        BinaryOp::BitXorAssign => "field_bxor_eq",
        BinaryOp::ShiftLAssign => "field_shl_eq",
        BinaryOp::ShiftRAssign => "field_shr_eq",
        BinaryOp::UShiftRAssign => "field_ushr_eq",
        BinaryOp::NullCoalesceAssign => "field_coalesce_eq",
        _ => return None,
    })
}

/// Whether an interval bound makes the interval a `Real` one: a real literal
/// (`1.0`, `∞`) or the `Infinity`/`INFINITY` keyword (possibly negated). A
/// variable or arithmetic bound does NOT — upstream keys class selection on the
/// literal token, treating `[a..5]` as an integer interval regardless of `a`'s
/// inferred type.
/// Whether `e` can be an operand of a raw Java `<`/`>`/`<=`/`>=` — an int/real
/// (NOT bool: Java rejects `long > boolean`). Recurses through unary `-`/`+`.
fn is_java_comparable_number(e: &Expr) -> bool {
    match &e.kind {
        ExprKind::Literal(Literal::Int(_) | Literal::Real(_)) => true,
        ExprKind::Unary(UnaryOp::Neg | UnaryOp::Pos, inner) => is_java_comparable_number(inner),
        _ => matches!(e.ty, Type::Integer | Type::Real),
    }
}

/// Render a builtin constant's runtime [`Value`] as a Java literal.
fn constant_literal(v: &leek_runtime::Value) -> String {
    use leek_runtime::Value;
    match v {
        Value::Int(n) => format!("{n}l"),
        Value::Bool(b) => b.to_string(),
        Value::Null => "null".to_string(),
        Value::Real(f) if f.is_nan() => "Double.NaN".to_string(),
        Value::Real(f) if f.is_infinite() => {
            if *f < 0.0 {
                "Double.NEGATIVE_INFINITY".to_string()
            } else {
                "Double.POSITIVE_INFINITY".to_string()
            }
        }
        // A finite real constant — ensure a decimal point so it's a Java double.
        Value::Real(f) => {
            let s = format!("{f}");
            if s.contains('.') || s.contains('e') || s.contains('E') {
                s
            } else {
                format!("{s}.0")
            }
        }
        Value::String(s) => format!("{:?}", s.as_str()),
        // Composite constants don't occur in the builtin constant table.
        _ => "null".to_string(),
    }
}

/// Whether an interval literal is a `Real` interval (vs `Integer`): true when
/// any *present* bound forces real, or when both bounds are absent (`[..]` /
/// `]..[`). Used both to pick the construction class and to cast an interval
/// literal to its concrete runtime class for a receiver-method call (the
/// interval methods live on the concrete subclasses, not the abstract base).
fn interval_is_real(iv: &IntervalExpr) -> bool {
    let present = [iv.start.as_deref(), iv.end.as_deref()];
    let has_present = present.iter().any(Option::is_some);
    let any_real = present.into_iter().flatten().any(bound_forces_real);
    any_real || !has_present
}

fn bound_forces_real(e: &Expr) -> bool {
    match &e.kind {
        // A *finite* real literal forces a real interval. The `∞` / `-∞` symbol
        // lowers to an infinite real literal but does NOT — upstream emits
        // `]-∞..5]` as an `IntegerInterval` (the infinite bound is just the
        // sentinel). Only the `Infinity` *keyword* (a builtin name) forces real.
        ExprKind::Literal(Literal::Real(r)) => r.is_finite(),
        ExprKind::Name(NameRef::Builtin(n)) => n == "Infinity" || n == "INFINITY",
        ExprKind::Unary(_, inner) => bound_forces_real(inner),
        _ => false,
    }
}

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
                // `this.field` inside a class method reads the real Java field
                // directly (the class is a `NativeObjectLeekValue` subclass with
                // public fields). Any other base is `Object`-typed, so go
                // through the reflective `getField(base, "name", null)`.
                if self.is_own_instance_field(b, name) {
                    buf.push_str(&self.own_instance_field_ref(name));
                } else {
                    buf.push_str("getField(");
                    self.write_expr(buf, b, false);
                    buf.push_str(", \"");
                    buf.push_str(name);
                    buf.push_str("\", ");
                    buf.push_str(&self.calling_class());
                    buf.push(')');
                }
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
                if self.ref_boxes.borrow().contains(id) {
                    // `@`-ref param bound to a `Box` — read its current value.
                    buf.push_str(&mangled);
                    buf.push_str(".get()");
                } else if self.boxed_locals.borrow().contains(id) {
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
                // A hoisted singleton so repeated refs are the same instance
                // (`test == test` → true).
                let field = format!("ufunction_{mangled}");
                buf.push_str(&self.fn_singleton(field, || user_fn_wrapper(&mangled, arity)));
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
                        // A builtin *constant* (`SORT_DESC`, `TYPE_ARRAY`, …)
                        // folds to its literal value (upstream does the same —
                        // there's no Java symbol for it). `lookup_constant` is
                        // constants-only, so a builtin *function* name still
                        // falls through to the `FunctionLeekValue` wrapper below.
                        if let Some(lit) =
                            leek_runtime::lookup_constant(other).map(|v| constant_literal(&v))
                        {
                            buf.push_str(&lit);
                        } else if builtin_fn_wrapper(other, self.opts.version).is_some() {
                            // First-class reference to a builtin function
                            // (`var f = cos` / `arrayMap([1,2,3], cos)`).
                            // Mirrors upstream's `writeAnonymousSystemFunctions`,
                            // which synthesizes a per-AI `FunctionLeekValue` for
                            // every builtin that escapes its call shape. Hoisted
                            // to a singleton so `endsWith == endsWith` → true.
                            let field = format!("anonymous_{}", super::sanitize_ident(other));
                            let v = self.opts.version;
                            buf.push_str(
                                &self.fn_singleton(field, || builtin_fn_wrapper(other, v).unwrap()),
                            );
                        } else {
                            buf.push_str(other);
                        }
                    }
                }
            }
            NameRef::This => {
                // Inside a class, `this` is the instance — emit the qualified
                // `<u_Class>.this` so it stays the enclosing instance even
                // inside an INLINE lambda (where bare `this` is the
                // `FunctionLeekValue`). In an OUTLINED lambda (an AI-level
                // method) `<u_Class>.this` is out of scope, so fall back to bare
                // `this`; at top level it's the AI (`this`).
                match self.current_class.get() {
                    Some(c) if !self.in_outlined.get() => {
                        buf.push_str(&mangle::class_name(self.opts, &c.name));
                        buf.push_str(".this");
                    }
                    _ => buf.push_str("this"),
                }
            }
            NameRef::Super => buf.push_str("super"),
            NameRef::Class_ => {
                // `class` (the current class) is its `ClassLeekValue` handle
                // inside a method (`class.name` → the class name); fall back to
                // `this.getClass()` outside a class context.
                if let Some(c) = self.current_class.get() {
                    buf.push_str(&mangle::class_name(self.opts, &c.name));
                } else {
                    buf.push_str("this.getClass()");
                }
            }
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
                // v1–v3 `==` is the *loose* equality `eq` (type-coercing:
                // `true == 1`, `1 == "1"`); v4 promoted `==` to the stricter
                // `equals_equals` and moved loose compare out. (`===` is the
                // strict/identity form — handled by `IdentityEq`.)
                let f = if matches!(self.opts.version, leek_syntax::Version::V4) {
                    "equals_equals("
                } else {
                    "eq("
                };
                buf.push_str(f);
                self.write_expr(buf, l, false);
                buf.push_str(", ");
                self.write_expr(buf, r, false);
                buf.push(')');
            }
            BinaryOp::Ne => {
                let f = if matches!(self.opts.version, leek_syntax::Version::V4) {
                    "notequals_equals("
                } else {
                    "neq("
                };
                buf.push_str(f);
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
            BinaryOp::And => self.write_short_circuit(buf, l, r, "&&", op),
            BinaryOp::Or => self.write_short_circuit(buf, l, r, "||", op),
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
                // `x in c`: AI's `operatorIn(container, value)` dispatches over
                // intervals / arrays / maps / sets (there is no `contains`
                // instance method — that was a wrong guess).
                buf.push_str("operatorIn(");
                self.write_expr(buf, r, false);
                buf.push_str(", ");
                self.write_expr(buf, l, false);
                buf.push(')');
            }
            BinaryOp::NotIn => {
                buf.push_str("!operatorIn(");
                self.write_expr(buf, r, false);
                buf.push_str(", ");
                self.write_expr(buf, l, false);
                buf.push(')');
            }
            BinaryOp::Is | BinaryOp::Instanceof => {
                // AI's `instanceOf(value, classValue)` — the class operand is a
                // `ClassLeekValue` (the builtin `arrayClass`/`mapClass`/… fields,
                // or a user class's `u_<C>` handle), NOT a Java type, so the
                // native `instanceof` operator can't be used.
                buf.push_str("instanceOf(");
                self.write_expr(buf, l, false);
                buf.push_str(", ");
                self.write_expr(buf, r, false);
                buf.push(')');
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
        // Raw Java `<`/`>` needs both operands to be actual numbers — a `bool`
        // is "primitive" but `10l > false` is a javac error, so a bool operand
        // (or anything non-numeric) routes through the runtime helper.
        if is_java_comparable_number(l) && is_java_comparable_number(r) {
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
        // `@`-ref param (a runtime `Box`) — route writes through `Box` methods
        // so they propagate to the caller's variable / array element.
        if self.is_ref_box(l) {
            let ExprKind::Name(NameRef::Local(id)) = &l.kind else {
                unreachable!("is_ref_box implies Name(Local)")
            };
            let name = mangle::local(self.opts, self.def_name(*id));
            let method = match op {
                BinaryOp::Assign => "set",
                BinaryOp::AddAssign => "add_eq",
                BinaryOp::SubAssign => "sub_eq",
                BinaryOp::MulAssign => "mul_eq",
                BinaryOp::DivAssign => "div_eq",
                _ => "",
            };
            if method.is_empty() {
                // No dedicated `Box` mutator — `set` the recomputed value.
                let base = op.compound_base().unwrap_or(BinaryOp::Assign);
                buf.push_str(&name);
                buf.push_str(".set(");
                if matches!(op, BinaryOp::Assign) {
                    self.write_expr(buf, r, false);
                } else {
                    let expanded = Expr {
                        kind: ExprKind::Binary(base, Box::new(l.clone()), Box::new(r.clone())),
                        ty: l.ty.clone(),
                        span: l.span,
                    };
                    self.write_expr(buf, &expanded, false);
                }
                buf.push(')');
            } else {
                buf.push_str(&name);
                buf.push('.');
                buf.push_str(method);
                buf.push('(');
                self.write_expr(buf, r, false);
                buf.push(')');
            }
            return;
        }
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
        // Field write. `this.field = v` inside a class method writes the real
        // Java field directly (coerced to the field's Java type). Any other
        // base is `Object`-typed, so go through reflective
        // `setField(base, "field", v, null)` (assignable; the read-side
        // `getField` isn't).
        if let ExprKind::Field(base, fname) = &l.kind {
            if self.is_own_instance_field(base, fname) {
                // Direct Java field, qualified as `<Class>.this.<field>` (see
                // `own_instance_field_ref`): dodges a same-named param shadow
                // (`constructor(name) { this.name = name }` in clean mode) AND
                // works inside a lambda (where bare `this` is the lambda). A
                // compound `this.x <op>= r` becomes `…this.x = coerce(…this.x
                // <op> r)` — the synthesized binary's read qualifies the same way.
                buf.push_str(&self.own_instance_field_ref(fname));
                buf.push_str(" = ");
                let v = match self.compound_base_op(op) {
                    Some(bop) => {
                        let expanded = Expr {
                            kind: ExprKind::Binary(bop, Box::new(l.clone()), Box::new(r.clone())),
                            ty: l.ty.clone(),
                            span: l.span,
                        };
                        self.expr_to_string(&expanded)
                    }
                    None => self.expr_to_string(r),
                };
                buf.push_str(&Self::coerce_decl(self.own_field_ty(fname).as_ref(), v));
            } else if let Some(helper) = field_compound_helper(op) {
                // External field compound assign: `a.f <op>= r` →
                // `field_<op>_eq(a, "f", r, null)`, which mutates the field and
                // returns the new value (a plain `setField` would store only the
                // RHS and drop the `<op>`).
                let helper = if self.is_v1_pow_assign(op) {
                    "field_pow_eq"
                } else {
                    helper
                };
                buf.push_str(helper);
                buf.push('(');
                self.write_expr(buf, base, false);
                buf.push_str(", \"");
                buf.push_str(fname);
                buf.push_str("\", ");
                self.write_expr(buf, r, false);
                buf.push_str(", ");
                buf.push_str(&self.calling_class());
                buf.push(')');
            } else {
                buf.push_str("setField(");
                self.write_expr(buf, base, false);
                buf.push_str(", \"");
                buf.push_str(fname);
                buf.push_str("\", ");
                // A typed static field coerces a numeric-literal write to its
                // declared type (`static real reel; titi.reel = 10` stores
                // `10.0`, so `.class` reads `Real`). Only a statically-numeric
                // value is coerced (a bare var could be null).
                match self.static_field_scalar_ty(base, fname) {
                    Some(ty) if Self::expr_is_numeric(r) => {
                        let v = self.expr_to_string(r);
                        buf.push_str(&Self::coerce_decl(Some(&ty), v));
                    }
                    _ => self.write_expr(buf, r, false),
                }
                buf.push_str(", ");
                buf.push_str(&self.calling_class());
                buf.push(')');
            }
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
        if let Some(base) = self.compound_base_op(op) {
            // Synthesize a non-compound binary so write_binary handles
            // promotion / concat / runtime-fn routing for us.
            let expanded = Expr {
                kind: ExprKind::Binary(base, Box::new(l.clone()), Box::new(r.clone())),
                ty: l.ty.clone(),
                span: l.span,
            };
            self.write_expr(buf, l, false);
            buf.push_str(" = ");
            // A typed scalar target (`integer g_x; g_x %= 5`) holds a primitive
            // `long`/`double`, so the `Object`-typed binary result must coerce
            // back — same as the plain-`=` path.
            let v = self.expr_to_string(&expanded);
            buf.push_str(&Self::coerce_decl(self.assign_target_scalar_ty(l), v));
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
            // v1 value semantics: a plain `b = a` deep-copies a composite
            // load so `b` doesn't alias `a` (see `v1_clone`).
            // A statically-typed scalar target coerces the RHS to its declared
            // type (`integer b; b = a[1]` stores an int) — same `compileConvert`
            // rule as the var-decl initializer.
            buf.push_str(&Self::coerce_decl(
                self.assign_target_scalar_ty(l),
                self.v1_clone(r),
            ));
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
            BinaryOp::IntDivAssign => "put_intdiv_eq",
            BinaryOp::ModAssign => "put_mod_eq",
            BinaryOp::PowAssign => "put_pow_eq",
            BinaryOp::BitAndAssign => "put_band_eq",
            BinaryOp::BitOrAssign => "put_bor_eq",
            BinaryOp::BitXorAssign => "put_bxor_eq",
            BinaryOp::ShiftLAssign => "put_shl_eq",
            BinaryOp::ShiftRAssign => "put_shr_eq",
            BinaryOp::UShiftRAssign => "put_ushr_eq",
            BinaryOp::NullCoalesceAssign => "put_coalesce_eq",
            _ => "putv4",
        };
        // v1 `^=` is power-assign, not xor.
        let helper = if self.is_v1_pow_assign(op) {
            "put_pow_eq"
        } else {
            helper
        };
        buf.push_str(helper);
        buf.push('(');
        self.write_expr(buf, base, false);
        buf.push_str(", ");
        self.write_expr(buf, idx, false);
        buf.push_str(", ");
        self.write_index_value(buf, op, base, value);
        buf.push_str(", null)");
    }

    /// Emit the value of an indexed assignment, coercing it to the array's
    /// declared element type for a plain `=` into a typed numeric array — the
    /// runtime `ArrayLeekValue` is type-erased, so `Array<real> a; a[0] = 5`
    /// must store `5.0` and `Array<integer> a; a[0] = 5.7` must store `5`.
    /// Mirrors upstream `JavaWriter.compileConvert`'s numeric arms
    /// (`((Number) v).doubleValue()` / `.longValue()`). The Java backend gets
    /// untyped HIR (`value.ty` is `Any`), so the element type is recovered from
    /// the base local's *declaration* and the value is coerced only when it is
    /// statically, unambiguously numeric (a literal or a math builtin — never a
    /// bare `+`, which could be string/array concat) — matching upstream's
    /// "only an int-or-real static value is converted" rule and avoiding a cast
    /// of a non-`Number` value.
    fn write_index_value(&self, buf: &mut String, op: BinaryOp, base: &Expr, value: &Expr) {
        if op == BinaryOp::Assign && Self::expr_is_numeric(value) {
            let suffix = match self.local_array_elem_ty(base) {
                Some(Type::Real) => Some(").doubleValue()"),
                Some(Type::Integer) => Some(").longValue()"),
                _ => None,
            };
            if let Some(suffix) = suffix {
                buf.push_str("((Number) ");
                self.write_expr(buf, value, true);
                buf.push_str(suffix);
                return;
            }
        }
        self.write_expr(buf, value, false);
    }

    /// The declared scalar type of an assignment l-value, when it's a local or
    /// global declared `integer`/`real`/`boolean`. Used to coerce the RHS of a
    /// plain `=` the same way the var-decl initializer is coerced.
    fn assign_target_scalar_ty(&self, l: &Expr) -> Option<&Type> {
        let ty = match &l.kind {
            ExprKind::Name(NameRef::Local(id)) => match self.hir.defs.get(id.0 as usize) {
                Some(Def::Local(d)) => d.ty.as_ref(),
                _ => None,
            },
            ExprKind::Name(NameRef::Global(id)) => match self.hir.defs.get(id.0 as usize) {
                Some(Def::Global(d)) => d.ty.as_ref(),
                _ => None,
            },
            _ => None,
        };
        match ty {
            Some(t @ (Type::Integer | Type::Real | Type::Boolean)) => Some(t),
            _ => None,
        }
    }

    /// The declared element type stored *through* an index l-value on `base`,
    /// when `base` is a local with a statically typed container — `Array<real>
    /// a` → `Real` (write `a[i] = …`), `Map<k, real> m` → `Real` (write `m[k] =
    /// …`). Recovered from the def table since the HIR `Expr.ty` is `Any` here.
    fn local_array_elem_ty(&self, base: &Expr) -> Option<Type> {
        if let ExprKind::Name(NameRef::Local(id)) = &base.kind
            && let Some(Def::Local(l)) = self.hir.defs.get(id.0 as usize)
        {
            return match &l.ty {
                Some(Type::Array(elem)) => Some((**elem).clone()),
                Some(Type::Map(_, val)) => Some((**val).clone()),
                _ => None,
            };
        }
        None
    }

    /// True when `base.name` is `this.<field>` inside a class method and
    /// `<field>` is one of that class's own instance fields — so it's emitted
    /// as a direct Java field access instead of reflective `getField`/`setField`
    /// (the class is a `NativeObjectLeekValue` subclass with real Java fields).
    /// A reference to the current class's own instance field, qualified as
    /// `<u_Class>.this.<field>`. The enclosing-instance form (not bare `this.`)
    /// is correct both directly in a method and inside a lambda created in that
    /// method (where bare `this` is the `FunctionLeekValue`, not the instance),
    /// and it can't be shadowed by a same-named param (clean mode doesn't prefix
    /// params). Falls back to bare `this.` if no class context (shouldn't happen
    /// — `is_own_instance_field` already gates on `current_class`).
    fn own_instance_field_ref(&self, field: &str) -> String {
        match self.current_class.get() {
            Some(c) => format!("{}.this.{}", mangle::class_name(self.opts, &c.name), field),
            None => format!("this.{field}"),
        }
    }

    pub(crate) fn is_own_instance_field(&self, base: &Expr, name: &str) -> bool {
        matches!(&base.kind, ExprKind::Name(NameRef::This))
            && self
                .current_class
                .get()
                .is_some_and(|c| c.fields.iter().any(|f| !f.is_static && f.name == name))
    }

    /// Whether `p` is a `@`-by-ref parameter at v1 — the only version where a
    /// write through it propagates to the caller (via a runtime `Box`). At v2+
    /// `@` params are plain.
    pub(crate) fn is_v1_ref_param(&self, p: &leek_hir::Param) -> bool {
        p.is_by_ref && matches!(self.opts.version, leek_syntax::Version::V1)
    }

    /// Static op cost of an expression *as emitted* — [`expr_op_cost`] minus the
    /// builtin per-call cost of any **shadowed** builtin calls inside it. A
    /// reassigned builtin (`count = function(…){…}; count(…)`) is dispatched
    /// through the `__shadows` map (a user function), so it never pays the
    /// builtin's tabulated cost; counting it would over-charge.
    pub(crate) fn emit_cost(&self, e: &Expr) -> u32 {
        super::expr_op_cost(e).saturating_sub(self.shadowed_overcharge(e))
    }

    /// Emit a short-circuiting `&&` / `||`, distributing op cost into per-operand
    /// `ops(...)` wrappers so the right side is charged only when evaluated. The
    /// operator's own cost rides on the always-evaluated left operand — matching
    /// the reference's `ops(l, lc + opCost) && ops(r, rc)` shape.
    fn write_short_circuit(
        &self,
        buf: &mut String,
        l: &Expr,
        r: &Expr,
        java_op: &str,
        op: BinaryOp,
    ) {
        buf.push('(');
        let lb = self.expr_to_bool(l);
        let rb = self.expr_to_bool(r);
        if self.opts.emit_ops {
            let lc = self.emit_cost(l) + super::binary_op_cost(op);
            let rc = self.emit_cost(r);
            let _ = write!(buf, "ops({lb}, {lc}) {java_op} ops({rb}, {rc})");
        } else {
            let _ = write!(buf, "{lb} {java_op} {rb}");
        }
        buf.push(')');
    }

    fn shadowed_overcharge(&self, e: &Expr) -> u32 {
        let mut total = 0;
        if let ExprKind::Call(c) = &e.kind
            && let Callee::Function(NameRef::Builtin(name)) = &c.callee
            && self.shadowed_builtins.borrow().contains(name)
        {
            total += super::builtin_call_cost(name);
        }
        leek_hir::visit::walk_expr_children(e, &mut |c| total += self.shadowed_overcharge(c));
        total
    }

    /// At v1 every by-value parameter is bound through a `new Box(ai, …)` whose
    /// 2-arg ctor charges 1 op — so a callable's body pays 1 op per by-value
    /// param on entry (per call). Returns the `ops(n);` tick to emit at the body
    /// start, or empty at v2+ (plain params) / when there are none. `@`-ref
    /// params alias a passed box, so they're excluded.
    pub(crate) fn v1_param_box_ops(&self, params: &[leek_hir::Param]) -> String {
        if !matches!(self.opts.version, leek_syntax::Version::V1) {
            return String::new();
        }
        let n = params.iter().filter(|p| !p.is_by_ref).count();
        if n > 0 {
            format!("ops({n});")
        } else {
            String::new()
        }
    }

    /// Whether the l-value/operand is a `@`-ref-param bound to a runtime `Box`
    /// (so reads use `.get()` and writes route through `Box` methods).
    pub(crate) fn is_ref_box(&self, e: &Expr) -> bool {
        matches!(&e.kind, ExprKind::Name(NameRef::Local(id)) if self.ref_boxes.borrow().contains(id))
    }

    /// The Java name of the `Box` variable backing a `@`-ref param.
    pub(crate) fn ref_box_name(&self, e: &Expr) -> String {
        match &e.kind {
            ExprKind::Name(NameRef::Local(id)) => mangle::local(self.opts, self.def_name(*id)),
            _ => String::new(),
        }
    }

    /// Register (once) a hoisted `FunctionLeekValue` singleton field named
    /// `field`, initialized to `make()`'s wrapper, and return the field name to
    /// reference at the use site. Repeated references to the same function thus
    /// reuse one instance, so `f == f` compares equal.
    pub(crate) fn fn_singleton(&self, field: String, make: impl FnOnce() -> String) -> String {
        if !self.fn_singletons.borrow().contains_key(&field) {
            let decl = format!("private FunctionLeekValue {field} = {};", make());
            self.fn_singletons.borrow_mut().insert(field.clone(), decl);
        }
        field
    }

    /// Register (once, dedup'd by `key`) a hoisted class member — any full
    /// declaration (e.g. a runtime-dispatch helper method) emitted at class-body
    /// end alongside the function singletons.
    pub(crate) fn hoist_member(&self, key: &str, make: impl FnOnce() -> String) {
        if !self.fn_singletons.borrow().contains_key(key) {
            let decl = make();
            self.fn_singletons
                .borrow_mut()
                .insert(key.to_string(), decl);
        }
    }

    /// The base binary op a compound assignment decomposes to, accounting for
    /// the v1 quirk where `^=` is exponent-assign (power), not bitwise-xor —
    /// even though the binary `^` is xor at every version.
    fn compound_base_op(&self, op: BinaryOp) -> Option<BinaryOp> {
        if matches!(op, BinaryOp::BitXorAssign)
            && matches!(self.opts.version, leek_syntax::Version::V1)
        {
            return Some(BinaryOp::Pow);
        }
        op.compound_base()
    }

    /// True when `op` is `^=` and we're at v1 — where it means power-assign, so
    /// the index/field put-helper must be the `pow` variant, not `bxor`.
    fn is_v1_pow_assign(&self, op: BinaryOp) -> bool {
        matches!(op, BinaryOp::BitXorAssign)
            && matches!(self.opts.version, leek_syntax::Version::V1)
    }

    /// The calling-class argument for a visibility-checked member access
    /// (`getField`/`setField`/`callObjectAccess`/`field_*_eq`). Inside a class
    /// (a method body or the static initializer) this is that class's
    /// `ClassLeekValue` handle, granting the access rights the class legitimately
    /// has (private/protected to itself and its hierarchy); at top level it's
    /// `null`. Passing the real context — as upstream does — stops a class's own
    /// private member from being wrongly denied (which read back as `null`).
    pub(crate) fn calling_class(&self) -> String {
        match self.current_class.get() {
            Some(c) => mangle::class_name(self.opts, &c.name),
            None => "null".to_string(),
        }
    }

    /// The declared scalar type of a static field `<Class>.<name>` (for a write
    /// coercion). `None` unless `base` is a class reference and the field is a
    /// static `integer`/`real`/`boolean`/nullable-scalar.
    fn static_field_scalar_ty(&self, base: &Expr, name: &str) -> Option<Type> {
        let ExprKind::Name(NameRef::Class(id)) = &base.kind else {
            return None;
        };
        let Some(Def::Class(c)) = self.hir.defs.get(id.0 as usize) else {
            return None;
        };
        let ty = c
            .fields
            .iter()
            .find(|f| f.is_static && f.name == name)?
            .ty
            .clone()?;
        matches!(
            ty,
            Type::Integer | Type::Real | Type::Boolean | Type::Nullable(_)
        )
        .then_some(ty)
    }

    pub(crate) fn own_field_ty(&self, name: &str) -> Option<Type> {
        self.current_class.get().and_then(|c| {
            c.fields
                .iter()
                .find(|f| !f.is_static && f.name == name)
                .and_then(|f| f.ty.clone())
        })
    }

    /// Whether `e` is statically, unambiguously a number — an int/real literal,
    /// a negation of one, or a numeric math builtin (`round`/`floor`/`sqrt`/…).
    /// Deliberately conservative: a bare `+`/`*` etc. is excluded because
    /// without types it could be string or array concatenation, not arithmetic.
    fn expr_is_numeric(e: &Expr) -> bool {
        match &e.kind {
            ExprKind::Literal(Literal::Int(_) | Literal::Real(_)) => true,
            ExprKind::Unary(UnaryOp::Neg, x) => Self::expr_is_numeric(x),
            ExprKind::Call(c) => matches!(
                &c.callee,
                Callee::Function(NameRef::Builtin(n)) if is_numeric_math_builtin(n)
            ),
            _ => false,
        }
    }

    /// Emit `++`/`--` on an index l-value via the compound-assign put-helper
    /// (`put_add_eq(base, idx, 1l, null)` / `put_sub_eq(...)`), which returns
    /// the new value. `a[i]` lowers to `get(...)`, not assignable in Java, so
    /// the plain `<x> = add(<x>, 1l)` form can't be used for an indexed target.
    /// Emit `++`/`--` on an *external* field l-value (`A.a++`, `obj.f++`,
    /// `a[0].f++`) via `field_add_eq`/`field_sub_eq` (returns the new value).
    /// `obj.f` reads as `getField(...)`, not a Java l-value, so the plain
    /// `<x> = add(<x>, 1l)` form can't be used. Own `this.f` is a real Java field
    /// and stays on the direct path.
    fn write_field_incdec(&self, buf: &mut String, helper: &str, base: &Expr, fname: &str) {
        buf.push_str(helper);
        buf.push('(');
        self.write_expr(buf, base, false);
        buf.push_str(", \"");
        buf.push_str(fname);
        buf.push_str("\", 1l, ");
        buf.push_str(&self.calling_class());
        buf.push(')');
    }

    /// Whether `x` is an external (non-own-`this`) field l-value, needing the
    /// `field_*_eq` inc/dec path rather than a direct Java field assignment.
    fn is_external_field<'b>(&self, x: &'b Expr) -> Option<(&'b Expr, &'b str)> {
        if let ExprKind::Field(base, fname) = &x.kind
            && !self.is_own_instance_field(base, fname)
        {
            return Some((base, fname));
        }
        None
    }

    fn write_index_incdec(&self, buf: &mut String, helper: &str, base: &Expr, idx: &Expr) {
        buf.push_str(helper);
        buf.push('(');
        self.write_expr(buf, base, false);
        buf.push_str(", ");
        self.write_expr(buf, idx, false);
        buf.push_str(", 1l, null)");
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
                // An index l-value (`a[i]`) emits as `get(...)`, which isn't a
                // valid Java l-value — route through the `put_add_eq` helper
                // (returns the new value, matching pre-increment), like a
                // compound `a[i] += 1`. Reference shape for a plain l-value:
                // `u_x = add(u_x, 1l)` (`add`/`sub` are bare AI instance
                // methods).
                if self.is_ref_box(x) {
                    buf.push_str(&self.ref_box_name(x));
                    buf.push_str(".increment()");
                } else if let ExprKind::Index(base, idx) = &x.kind {
                    self.write_index_incdec(buf, "put_add_eq", base, idx);
                } else if let Some((base, fname)) = self.is_external_field(x) {
                    self.write_field_incdec(buf, "field_add_eq", base, fname);
                } else {
                    self.write_expr(buf, x, false);
                    buf.push_str(" = add(");
                    self.write_expr(buf, x, false);
                    buf.push_str(", 1l)");
                }
            }
            UnaryOp::PreDec => {
                if self.is_ref_box(x) {
                    buf.push_str(&self.ref_box_name(x));
                    buf.push_str(".decrement()");
                } else if let ExprKind::Index(base, idx) = &x.kind {
                    self.write_index_incdec(buf, "put_sub_eq", base, idx);
                } else if let Some((base, fname)) = self.is_external_field(x) {
                    self.write_field_incdec(buf, "field_sub_eq", base, fname);
                } else {
                    self.write_expr(buf, x, false);
                    buf.push_str(" = sub(");
                    self.write_expr(buf, x, false);
                    buf.push_str(", 1l)");
                }
            }
            UnaryOp::Ref => self.write_expr(buf, x, parens_if_negative),
        }
    }

    pub(crate) fn write_postfix(&self, buf: &mut String, op: PostfixOp, x: &Expr) {
        match op {
            PostfixOp::PostInc => {
                // `a++` = pre-value of `a` after assignment. Trick from
                // the Java reference: compute `add(a, 1)`, assign it to
                // `a`, then `sub(...,1)` to recover the original. An index
                // l-value goes through `put_add_eq` (returns the new value),
                // so the old value is `sub(put_add_eq(...), 1)`.
                buf.push_str("sub(");
                if self.is_ref_box(x) {
                    buf.push_str(&self.ref_box_name(x));
                    buf.push_str(".increment()");
                } else if let ExprKind::Index(base, idx) = &x.kind {
                    self.write_index_incdec(buf, "put_add_eq", base, idx);
                } else if let Some((base, fname)) = self.is_external_field(x) {
                    self.write_field_incdec(buf, "field_add_eq", base, fname);
                } else {
                    self.write_expr(buf, x, false);
                    buf.push_str(" = add(");
                    self.write_expr(buf, x, false);
                    buf.push_str(", 1l)");
                }
                buf.push_str(", 1l)");
            }
            PostfixOp::PostDec => {
                buf.push_str("add(");
                if self.is_ref_box(x) {
                    buf.push_str(&self.ref_box_name(x));
                    buf.push_str(".decrement()");
                } else if let ExprKind::Index(base, idx) = &x.kind {
                    self.write_index_incdec(buf, "put_sub_eq", base, idx);
                } else if let Some((base, fname)) = self.is_external_field(x) {
                    self.write_field_incdec(buf, "field_sub_eq", base, fname);
                } else {
                    self.write_expr(buf, x, false);
                    buf.push_str(" = sub(");
                    self.write_expr(buf, x, false);
                    buf.push_str(", 1l)");
                }
                buf.push_str(", 1l)");
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
            buf.push_str(&self.ai_this());
            buf.push(')');
            return;
        }
        buf.push_str("new ");
        buf.push_str(class);
        buf.push('(');
        buf.push_str(&self.ai_this());
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
        let legacy = !matches!(self.opts.version, leek_syntax::Version::V4);
        let class = if legacy {
            "LegacyArrayLeekValue"
        } else {
            "MapLeekValue"
        };
        if pairs.is_empty() {
            buf.push_str("new ");
            buf.push_str(class);
            buf.push('(');
            buf.push_str(&self.ai_this());
            buf.push(')');
            return;
        }
        buf.push_str("new ");
        buf.push_str(class);
        buf.push('(');
        buf.push_str(&self.ai_this());
        buf.push_str(", new Object[] { ");
        for (i, (k, v)) in pairs.iter().enumerate() {
            if i > 0 {
                buf.push_str(", ");
            }
            self.write_expr(buf, k, false);
            buf.push_str(", ");
            self.write_expr(buf, v, false);
        }
        buf.push_str(" }");
        // v1–v3 `LegacyArrayLeekValue` is one unified collection; the trailing
        // `isKeyValue` flag tells its `Object[]` constructor to read pairs
        // (`true`) rather than push sequentially (`false`). Without it a map
        // literal builds a 0,1,2,… auto-keyed array. `MapLeekValue` (v4) has no
        // such flag — its constructor always reads pairs.
        if legacy {
            buf.push_str(", true");
        }
        buf.push(')');
    }

    pub(crate) fn write_set(&self, buf: &mut String, items: &[Expr]) {
        buf.push_str("new SetLeekValue(");
        buf.push_str(&self.ai_this());
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
        buf.push_str(&self.ai_this());
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
        let then_cost = self.emit_cost(t);
        let else_cost = self.emit_cost(e);
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

    /// Refine a receiver-builtin's cast class to the concrete runtime type when
    /// the table names an abstract base we can't call methods on. Today: an
    /// interval literal receiver (`intervalMax([1..2])`) is cast to
    /// `Integer`/`RealIntervalLeekValue` (where the methods live) instead of the
    /// abstract `IntervalLeekValue`. A non-literal interval still falls back to
    /// the abstract class (would need a runtime-dispatch wrapper like upstream).
    pub(crate) fn concrete_receiver_class(
        class: &'static str,
        method: &str,
        receiver: &Expr,
    ) -> &'static str {
        // `intervalContains` lives on the abstract base (it has a `Number`
        // overload that dispatches) — keep the abstract cast; the concrete
        // subclasses only expose `(long)`/`(double)`, which an `Object` arg
        // can't match. All other interval methods are concrete-only.
        if class == "IntervalLeekValue"
            && method != "intervalContains"
            && let ExprKind::Interval(iv) = &receiver.kind
        {
            return if interval_is_real(iv) {
                "RealIntervalLeekValue"
            } else {
                "IntegerIntervalLeekValue"
            };
        }
        class
    }

    pub(crate) fn write_interval(&self, buf: &mut String, iv: &IntervalExpr) {
        // Reference shape:
        //   `new IntegerIntervalLeekValue(this, minClosed, from, maxClosed, to)`
        // `IntervalLeekValue` itself is abstract — pick Integer- or
        // Real- variant based on whether any endpoint is a real
        // literal. Step is part of runtime semantics, not the ctor.
        // A `Real` interval when any *present* endpoint is a real literal or the
        // `Infinity` keyword, or when BOTH bounds are absent (`[..]` / `]..[`).
        // Variable / expression bounds count as integer (upstream keys on the
        // literal token, not the inferred type: `var a = 1.5; [a..5]` is still
        // an `IntegerInterval`).
        let real = interval_is_real(iv);
        let cls = if real {
            "RealIntervalLeekValue"
        } else {
            "IntegerIntervalLeekValue"
        };
        // Sentinels for an *absent* bound. A closed bracket on the missing side
        // (`[…` / `…]`) collapses to the zero sentinel (the empty `[..]`); an
        // open bracket (`]…` / `…[`) means unbounded → ±∞ (`Long.MIN/MAX_VALUE`
        // for integer intervals, `Double.(NEGATIVE|POSITIVE)_INFINITY` for real).
        let (zero, neg_inf, pos_inf) = if real {
            (
                "0.0",
                "Double.NEGATIVE_INFINITY",
                "Double.POSITIVE_INFINITY",
            )
        } else {
            ("0l", "Long.MIN_VALUE", "Long.MAX_VALUE")
        };
        buf.push_str("new ");
        buf.push_str(cls);
        buf.push('(');
        buf.push_str(&self.ai_this());
        // Start. An absent bound always emits `inclusive = false`.
        buf.push_str(", ");
        if let Some(e) = &iv.start {
            buf.push_str(if iv.start_inclusive { "true" } else { "false" });
            buf.push_str(", ");
            self.write_expr(buf, e, false);
        } else {
            buf.push_str("false, ");
            buf.push_str(if iv.start_inclusive { zero } else { neg_inf });
        }
        // End.
        buf.push_str(", ");
        if let Some(e) = &iv.end {
            buf.push_str(if iv.end_inclusive { "true" } else { "false" });
            buf.push_str(", ");
            self.write_expr(buf, e, false);
        } else {
            buf.push_str("false, ");
            buf.push_str(if iv.end_inclusive { zero } else { pos_inf });
        }
        buf.push(')');
    }

    pub(crate) fn write_slice(&self, buf: &mut String, s: &SliceExpr) {
        // Mirror the upstream `LeekArrayAccess.writeJavaCode` range forms — AI
        // instance methods called bare (like `get(...)`), NOT a non-existent
        // `LeekOperations.slice`. The method name selects on which bounds are
        // present; then the base, the present bound(s), and the stride:
        //   a[i:j]  → rangeDynamic(base, i, j)   a[i:] → rangeDynamic_start(base, i)
        //   a[:j]   → rangeDynamic_end(base, j)  a[:]  → rangeDynamic_all(base)
        // with the stride appended last when present (`a[i:j:k]` → +`, k`).
        // We emit the `rangeDynamic*` family (upstream's ANY-typed choice, the
        // string-indexing PR #3138): for a String base it routes to
        // `stringSlice`, for everything else it is a pure delegate to the
        // matching `range*` — same values, same ops. Our HIR is untyped, so
        // the dynamic dispatch is always the right call.
        let name = match (s.start.is_some(), s.end.is_some()) {
            (true, true) => "rangeDynamic",
            (true, false) => "rangeDynamic_start",
            (false, true) => "rangeDynamic_end",
            (false, false) => "rangeDynamic_all",
        };
        buf.push_str(name);
        buf.push('(');
        self.write_expr(buf, &s.base, false);
        if let Some(e) = &s.start {
            buf.push_str(", ");
            self.write_expr(buf, e, false);
        }
        if let Some(e) = &s.end {
            buf.push_str(", ");
            self.write_expr(buf, e, false);
        }
        if let Some(e) = &s.step {
            buf.push_str(", ");
            self.write_expr(buf, e, false);
        }
        buf.push(')');
    }

    pub(crate) fn write_new(&self, buf: &mut String, n: &NewExpr) {
        // A built-in class used as a constructor (`new Array()`, `new Map`,
        // `new Integer()`, …) has no user-class Java constructor — route it to
        // the same construction the upstream emitter uses (collection
        // `*LeekValue`, or a primitive's default literal). See
        // `LeekFunctionCall.compileL`'s `ClassValueType` arm.
        if self.write_builtin_class_construct(buf, &n.class, &n.args) {
            return;
        }
        // A user class: construct via the AI's `new_<class>(args)` helper, which
        // runs the field-default constructor then the user `init(...)`. (`new
        // u_X(...)` directly would skip `init` / RAM accounting.) Each arg is
        // cast to `(Object)` so a lone `null` doesn't bind as a null
        // `Object... args` array (Java varargs ambiguity).
        buf.push_str("new_");
        buf.push_str(&mangle::class_name(self.opts, &n.class));
        buf.push('(');
        for (i, a) in n.args.iter().enumerate() {
            if i > 0 {
                buf.push_str(", ");
            }
            buf.push_str("(Object) (");
            self.write_expr(buf, a, false);
            buf.push(')');
        }
        buf.push(')');
    }

    /// Emit a built-in class used as a constructor (`Array()`, `new Map()`,
    /// `Set(1, 2)`, `Integer()`, …) and return `true`; return `false` for a
    /// name that isn't a built-in class (the caller then handles it as a user
    /// class or an unknown builtin). Collection classes build the matching
    /// `*LeekValue` exactly as an array/map/set/object literal would; the
    /// primitive classes collapse to their default literal, mirroring the
    /// upstream `LeekFunctionCall` `ClassValueType` arm. Used by both the
    /// `new C(...)` form ([`Self::write_new`]) and the call form `C(...)`.
    pub(crate) fn write_builtin_class_construct(
        &self,
        buf: &mut String,
        class: &str,
        args: &[Expr],
    ) -> bool {
        match class {
            // Collections build from the call args, just like a literal.
            "Array" => self.write_array(buf, args),
            "Set" => self.write_set(buf, args),
            // `Map()` / `Object()` take no positional elements — an empty one.
            "Map" => self.write_map(buf, &[]),
            "Object" => self.write_object(buf, &[]),
            // Primitive classes collapse to their default literal.
            "Integer" => buf.push_str("0l"),
            "Real" | "Number" => buf.push_str("0.0"),
            "Boolean" => buf.push_str("false"),
            "String" => buf.push_str("\"\""),
            _ => return false,
        }
        true
    }
}
