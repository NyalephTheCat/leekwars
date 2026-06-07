//! MIR interpreter value helpers.

use std::rc::Rc;

use leek_mir::{BinOp, BlockId, CastKind, Const as MirConst, UnOp};
use leek_types::Type;

use crate::value::Value;

use super::{Interpreter, Outcome};

// Pure value semantics now live in `leek-runtime` so both the interpreter
// and the native backend share one implementation. Re-export the ones the
// rest of the interpreter still references by these paths.
pub(crate) use leek_runtime::{
    legacy_map_key, make_foreach_iter, read_field, read_index, set_field, set_index,
};

pub(crate) enum StepResult {
    Goto(BlockId),
    Return(Value),
}

// ---- Helpers ----

pub(crate) fn const_to_value(c: &MirConst) -> Value {
    match c {
        MirConst::Null => Value::Null,
        MirConst::Bool(b) => Value::Bool(*b),
        MirConst::Int(i) => Value::Int(*i),
        MirConst::Real(bits) => Value::Real(f64::from_bits(*bits)),
        MirConst::String(s) => Value::String(Rc::new(s.clone())),
    }
}

// `read_field` / `read_index` / `set_field` / `set_index` / `legacy_map_key`
// now live in `leek_runtime::eval` (re-exported above).

pub(crate) fn slice_value(
    base: &Value,
    start: Option<i64>,
    end: Option<i64>,
    step: Option<f64>,
) -> Value {
    leek_runtime::slice(base, start, end, step)
}

pub(crate) fn apply_cast(kind: CastKind, v: &Value) -> Value {
    match kind {
        CastKind::IntToReal => Value::Real(v.to_real()),
        CastKind::RealToInt => Value::Int(v.to_long()),
        CastKind::ToBool => Value::Bool(v.is_truthy()),
        CastKind::ToString => Value::String(Rc::new(v.to_string())),
        CastKind::User => v.clone(),
    }
}

pub(crate) fn apply_unary(op: UnOp, v: &Value) -> Value {
    match op {
        UnOp::Neg => negate(v),
        UnOp::Pos => match v {
            Value::Bool(b) => Value::Int(i64::from(*b)),
            other => other.clone(),
        },
        UnOp::Not => Value::Bool(!v.is_truthy()),
        UnOp::BitNot => Value::Int(!v.as_int().unwrap_or(0)),
        UnOp::Ref => v.clone(),
    }
}

pub(crate) fn negate(v: &Value) -> Value {
    match v {
        Value::Int(i) => Value::Int(-i),
        Value::Real(r) => Value::Real(-r),
        Value::Bool(b) => Value::Int(if *b { -1 } else { 0 }),
        // Upstream treats `-null` as `0` (negation of the
        // default numeric value), not as `null`.
        Value::Null => Value::Int(0),
        _ => Value::Null,
    }
}

pub(crate) fn coerce_to_type(v: &Value, ty: &Type) -> Value {
    // Numeric promotion plus a null→0 default for plain numeric
    // slots. `T?` is the same as `T` for non-null values; null is
    // preserved unchanged.
    if let Type::Nullable(inner) = ty {
        return if matches!(v, Value::Null) {
            Value::Null
        } else {
            coerce_to_type(v, inner)
        };
    }
    match ty {
        Type::Real => match v {
            Value::Int(i) => Value::Real(leek_runtime::int_to_real(*i)),
            Value::Bool(b) => Value::Real(if *b { 1.0 } else { 0.0 }),
            Value::Null => Value::Real(0.0),
            other => other.clone(),
        },
        Type::Integer => match v {
            Value::Real(r) => Value::Int(leek_runtime::real_to_int(*r)),
            Value::Bool(b) => Value::Int(i64::from(*b)),
            Value::Null => Value::Int(0),
            other => other.clone(),
        },
        Type::Boolean => match v {
            Value::Null => Value::Bool(false),
            other => other.clone(),
        },
        _ => v.clone(),
    }
}

// ---- Binary operators ----

/// Per-binary-op cost, mirroring upstream's
/// `LeekValueType.{ADD,MUL,DIV,MOD,POW}_COST` constants. Everything
/// else (comparisons, bitwise, shifts, `in`, `is`) defaults to 1.
pub(crate) fn binary_op_cost(op: BinOp) -> u64 {
    // Single source of truth in `leek_mir` so the native backend charges the
    // identical cost (matching `.ops(N)` counts across backends).
    op.op_cost()
}

/// Dispatch a MIR binary operator to the shared per-operator
/// implementations in `leek_runtime::eval`. Never fails (the `Outcome`
/// error arm is kept for call-site compatibility), so the result is always
/// `Ok`.
#[allow(clippy::unnecessary_wraps)] // Result kept for call-site compatibility
pub(crate) fn apply_binary(op: BinOp, l: &Value, r: &Value, version: u8) -> Result<Value, Outcome> {
    use leek_runtime as rt;
    Ok(match op {
        BinOp::Add => rt::add(l, r),
        BinOp::Sub => rt::sub(l, r),
        BinOp::Mul => rt::mul(l, r),
        BinOp::Div => rt::div(l, r, version),
        BinOp::IntDiv => rt::int_div(l, r),
        BinOp::Mod => rt::rem(l, r),
        BinOp::Pow => rt::pow(l, r),
        BinOp::Eq => rt::eq(l, r, version),
        BinOp::Ne => rt::ne(l, r, version),
        BinOp::IdentityEq => rt::identity_eq(l, r),
        BinOp::IdentityNe => rt::identity_ne(l, r),
        BinOp::Lt => rt::lt(l, r),
        BinOp::Le => rt::le(l, r),
        BinOp::Gt => rt::gt(l, r),
        BinOp::Ge => rt::ge(l, r),
        BinOp::BitAnd => rt::bit_and(l, r),
        BinOp::BitOr => rt::bit_or(l, r),
        BinOp::BitXor => rt::bit_xor(l, r),
        BinOp::CompoundXor => rt::compound_xor(l, r, version),
        BinOp::Xor => rt::xor(l, r),
        BinOp::ShiftL => rt::shl(l, r),
        BinOp::ShiftR => rt::shr(l, r),
        BinOp::UShiftR => rt::ushr(l, r),
        BinOp::In => rt::in_op(l, r),
        BinOp::NotIn => rt::not_in(l, r),
        BinOp::Is => rt::is(l, r),
        BinOp::Instanceof => rt::instanceof(l, r),
    })
}

// ---- Built-in classes ----

/// Static-field lookup for a built-in class (`Integer.MIN_VALUE`,
/// `Real.PI`, etc.). Mirrors the small fixed table upstream
/// exposes — anything not listed here returns `None` and the
/// caller falls back to `read_field` (which gives `Null` for
/// classes).
/// The builtin-class helpers (`builtin_class_name`, `builtin_class_static`,
/// `construct_builtin_class`) now live in `leek_runtime` so the interpreter
/// and native backend share one definition.
pub(crate) use leek_runtime::{builtin_class_name, builtin_class_static, construct_builtin_class};

/// True when `class_name`'s parent chain (transitively) reaches
/// `target` — used to recognise `class A extends Array {}` so we
/// can materialise the instance as the underlying primitive.
pub(crate) fn walk_class_chain_parent(
    interp: &Interpreter,
    class_name: &str,
    target: &str,
) -> bool {
    let mut cursor = Some(class_name.to_string());
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    while let Some(name) = cursor {
        if !seen.insert(name.clone()) {
            return false;
        }
        let Some(&idx) = interp.class_by_name.get(&name) else {
            return false;
        };
        let c = &interp.program.classes[idx];
        match c.parent.as_deref() {
            Some(p) if p == target => return true,
            Some(p) => cursor = Some(p.to_string()),
            None => return false,
        }
    }
    false
}

