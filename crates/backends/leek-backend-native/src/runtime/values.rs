//! Scalar boxing/unboxing, cells, casts/promotions, and the
//! unary / binary operator shims (delegating to shared `leek_runtime`
//! semantics so native matches the interpreter exactly).

use super::{handle, val};
use leek_mir::ir::BinOp;
use leek_runtime::Value;
use std::cell::RefCell;
use std::rc::Rc;

/// Coerce a boxed value to a declared scalar kind (`0`=int, `1`=real,
/// `2`=bool), preserving `null` (for nullable declared types). Used to make
/// a typed static field's stored value match its declaration (`real? a = 12`
/// reads back `12.0`), mirroring the interpreter's `coerce_to_type`.
#[unsafe(no_mangle)]
pub extern "C" fn leek_coerce_scalar(h: *mut Value, kind: i64) -> *mut Value {
    let v = unsafe { val(h) };
    if matches!(v, Value::Null) {
        return h;
    }
    let coerced = match kind {
        0 => Value::Int(v.to_long()),
        1 => Value::Real(v.to_real()),
        _ => Value::Bool(v.is_truthy()),
    };
    handle(coerced)
}

#[unsafe(no_mangle)]
pub extern "C" fn leek_box_int(i: i64) -> *mut Value {
    handle(Value::Int(i))
}

#[unsafe(no_mangle)]
pub extern "C" fn leek_box_real(r: f64) -> *mut Value {
    handle(Value::Real(r))
}

#[unsafe(no_mangle)]
pub extern "C" fn leek_box_bool(b: i64) -> *mut Value {
    handle(Value::Bool(b != 0))
}

#[unsafe(no_mangle)]
pub extern "C" fn leek_box_null() -> *mut Value {
    handle(Value::Null)
}

/// Build a `Value::String` from `len` bytes at `ptr`. The generated code
/// materializes the literal's bytes in-binary (immediate stores) and passes a
/// pointer to them, so nothing references the *compiler* process's heap — the
/// AOT-relocatable replacement for baking a `box_string` handle as an immediate.
///
/// # Safety
/// `ptr` must point to `len` readable bytes (or be null when `len <= 0`).
#[unsafe(no_mangle)]
pub extern "C" fn leek_const_string(ptr: *const u8, len: i64) -> *mut Value {
    let s = if len <= 0 || ptr.is_null() {
        String::new()
    } else {
        let bytes = unsafe { std::slice::from_raw_parts(ptr, len as usize) };
        String::from_utf8_lossy(bytes).into_owned()
    };
    handle(Value::String(Rc::new(s)))
}

#[unsafe(no_mangle)]
pub extern "C" fn leek_unbox_int(p: *mut Value) -> i64 {
    unsafe { val(p) }.to_long()
}

#[unsafe(no_mangle)]
pub extern "C" fn leek_unbox_real(p: *mut Value) -> f64 {
    unsafe { val(p) }.to_real()
}

#[unsafe(no_mangle)]
pub extern "C" fn leek_unbox_bool(p: *mut Value) -> i64 {
    i64::from(unsafe { val(p) }.is_truthy())
}

/// Truthiness of a boxed value (for branching / `!` on a dynamic value),
/// using the shared `Value::is_truthy`. Returns 0 or 1.
#[unsafe(no_mangle)]
pub extern "C" fn leek_truthy(p: *mut Value) -> i64 {
    unsafe { val(p) }.is_truthy() as i64
}

/// Deep-clone a boxed value for v1 value semantics (assignment / pass-by-
/// value of a composite copies it). Scalars clone trivially.
#[unsafe(no_mangle)]
pub extern "C" fn leek_clone_v1(p: *mut Value) -> *mut Value {
    handle(leek_runtime::deep_clone(unsafe { val(p) }))
}

/// Give a local stable, shared `Value::Cell` storage so writes from either
/// the enclosing scope or a closure are visible to both. If `inner` is
/// already a cell (e.g. a lambda capture-parameter that arrives holding the
/// enclosing scope's cell handle), it is returned unchanged so the shared
/// `Rc` is preserved; otherwise its value is wrapped in a fresh cell.
#[unsafe(no_mangle)]
pub extern "C" fn leek_make_cell(inner: *mut Value) -> *mut Value {
    match unsafe { val(inner) } {
        Value::Cell(_) => inner,
        v => handle(Value::Cell(std::rc::Rc::new(RefCell::new(v.clone())))),
    }
}

/// Read a cell local: clone the value currently behind the cell (peeled).
/// A non-cell handle (defensive) is returned cloned unchanged.
#[unsafe(no_mangle)]
pub extern "C" fn leek_cell_get(cell: *mut Value) -> *mut Value {
    match unsafe { val(cell) } {
        Value::Cell(rc) => handle(rc.borrow().clone()),
        other => handle(other.clone()),
    }
}

/// Write a cell local: store `v` (peeled) into the shared slot, so any
/// closure sharing the cell's `Rc` observes the new value. A no-op on a
/// non-cell handle.
#[unsafe(no_mangle)]
pub extern "C" fn leek_cell_set(cell: *mut Value, v: *mut Value) {
    if let Value::Cell(rc) = unsafe { val(cell) } {
        *rc.borrow_mut() = unsafe { val(v) }.unbox();
    }
}

/// Consume a pending v1 LegacyArray promotion (stashed by a mutating
/// builtin like `push`) and return the promoted value; if none is pending,
/// return `current` unchanged. Used to lower `Statement::ApplyPromotion`.
#[unsafe(no_mangle)]
pub extern "C" fn leek_apply_promotion(current: *mut Value) -> *mut Value {
    match leek_runtime::take_pending_promotion() {
        Some(v) => handle(v),
        None => current,
    }
}

/// Apply a unary operator to a boxed value, returning a new handle.
/// `code`: 0 = negate (`-x`), 1 = bitwise-not (`~x`). Delegates to the
/// shared `leek_runtime` ops so the result matches the interpreter.
#[unsafe(no_mangle)]
pub extern "C" fn leek_value_unary(code: i64, p: *mut Value) -> *mut Value {
    let v = unsafe { val(p) };
    let r = match code {
        0 => leek_runtime::neg(v),
        1 => leek_runtime::bit_not(v),
        _ => Value::Null,
    };
    handle(r)
}

/// Apply a [`leek_mir::ir::CastKind`] to a boxed value, returning a new
/// handle. `code`: 0 = IntToReal, 1 = RealToInt, 2 = ToBool, 3 = ToString,
/// else = User (identity clone). Mirrors the interpreter's `apply_cast`
/// (same `Value` conversion methods), so the result matches exactly.
#[unsafe(no_mangle)]
pub extern "C" fn leek_apply_cast(code: i64, p: *mut Value) -> *mut Value {
    let v = unsafe { val(p) };
    let r = match code {
        0 => Value::Real(v.to_real()),
        1 => Value::Int(v.to_long()),
        2 => Value::Bool(v.is_truthy()),
        3 => Value::String(std::rc::Rc::new(v.to_string())),
        _ => v.clone(),
    };
    handle(r)
}

/// Apply a binary operator to two boxed values, returning a new handle.
/// Delegates to the interpreter's shared `apply_binary`, so the result
/// matches the interpreter exactly (string concat, array `+`, version-
/// specific division, etc.). `code` is a [`BinOp`] encoded via
/// [`binop_code`].
#[unsafe(no_mangle)]
pub extern "C" fn leek_value_binop(
    code: i64,
    a: *mut Value,
    b: *mut Value,
    version: i64,
) -> *mut Value {
    let Some(op) = binop_from_code(code) else {
        return handle(Value::Null);
    };
    let (l, r) = (unsafe { val(a) }, unsafe { val(b) });
    handle(apply_binop(op, l, r, version as u8))
}

/// Like [`leek_value_binop`] but the RIGHT operand is an integer *constant*
/// passed by value — so the backend never boxes it. Used for `dyn OP <int lit>`
/// (`n - 1`, `n < 2`, …), removing one heap allocation per such operation; the
/// constant `Value::Int` lives on the stack. Identical result to boxing it.
#[unsafe(no_mangle)]
pub extern "C" fn leek_value_binop_cir(
    code: i64,
    a: *mut Value,
    c: i64,
    version: i64,
) -> *mut Value {
    let Some(op) = binop_from_code(code) else {
        return handle(Value::Null);
    };
    let l = unsafe { val(a) };
    handle(apply_binop(op, l, &Value::Int(c), version as u8))
}

/// Mirror of [`leek_value_binop_cir`] for a LEFT integer constant
/// (`<int lit> OP dyn`) — order preserved for non-commutative ops.
#[unsafe(no_mangle)]
pub extern "C" fn leek_value_binop_cil(
    code: i64,
    c: i64,
    b: *mut Value,
    version: i64,
) -> *mut Value {
    let Some(op) = binop_from_code(code) else {
        return handle(Value::Null);
    };
    let r = unsafe { val(b) };
    handle(apply_binop(op, &Value::Int(c), r, version as u8))
}

/// `real`-typed counterparts of [`leek_value_binop_cir`] / `_cil`: the
/// statically-`real` operand is passed by value as an `f64`, so a `dyn OP
/// <real>` (or `<real> OP dyn`) never heap-boxes it. Building `Value::Real(c)`
/// on the stack is identical to boxing the operand and dispatching.
#[unsafe(no_mangle)]
pub extern "C" fn leek_value_binop_crr(
    code: i64,
    a: *mut Value,
    c: f64,
    version: i64,
) -> *mut Value {
    let Some(op) = binop_from_code(code) else {
        return handle(Value::Null);
    };
    let l = unsafe { val(a) };
    handle(apply_binop(op, l, &Value::Real(c), version as u8))
}

/// Left-`real`-operand mirror of [`leek_value_binop_crr`].
#[unsafe(no_mangle)]
pub extern "C" fn leek_value_binop_crl(
    code: i64,
    c: f64,
    b: *mut Value,
    version: i64,
) -> *mut Value {
    let Some(op) = binop_from_code(code) else {
        return handle(Value::Null);
    };
    let r = unsafe { val(b) };
    handle(apply_binop(op, &Value::Real(c), r, version as u8))
}

/// Pure dispatch of a [`BinOp`] onto the shared `leek_runtime` operators —
/// the single source of truth shared by the JIT `leek_value_binop` shim and
/// the compile-time const-evaluator (`const_eval_default`). Matches the
/// interpreter exactly (string concat, array `+`, version-specific division).
pub fn apply_binop(op: BinOp, l: &Value, r: &Value, v: u8) -> Value {
    use BinOp::{
        Add, BitAnd, BitOr, BitXor, CompoundXor, Div, Eq, Ge, Gt, IdentityEq, IdentityNe, In,
        Instanceof, IntDiv, Is, Le, Lt, Mod, Mul, Ne, NotIn, Pow, ShiftL, ShiftR, Sub, UShiftR,
        Xor,
    };
    use leek_runtime as rt;
    match op {
        Add => rt::add(l, r),
        Sub => rt::sub(l, r),
        Mul => rt::mul(l, r),
        Div => rt::div(l, r, v),
        Mod => rt::rem(l, r),
        IntDiv => rt::int_div(l, r),
        Pow => rt::pow(l, r),
        Eq => rt::eq(l, r, v),
        Ne => rt::ne(l, r, v),
        IdentityEq => rt::identity_eq(l, r),
        IdentityNe => rt::identity_ne(l, r),
        Lt => rt::lt(l, r),
        Le => rt::le(l, r),
        Gt => rt::gt(l, r),
        Ge => rt::ge(l, r),
        BitAnd => rt::bit_and(l, r),
        BitOr => rt::bit_or(l, r),
        BitXor => rt::bit_xor(l, r),
        CompoundXor => rt::compound_xor(l, r, v),
        Xor => rt::xor(l, r),
        ShiftL => rt::shl(l, r),
        ShiftR => rt::shr(l, r),
        UShiftR => rt::ushr(l, r),
        In => rt::in_op(l, r),
        NotIn => rt::not_in(l, r),
        Is => rt::is(l, r),
        Instanceof => rt::instanceof(l, r),
    }
}

/// Encode a [`BinOp`] as a stable `i64` for the FFI boundary.
pub fn binop_code(op: BinOp) -> i64 {
    op as i64
}

/// Decode a [`binop_code`] back into a [`BinOp`].
pub(super) fn binop_from_code(c: i64) -> Option<BinOp> {
    use BinOp::{
        Add, BitAnd, BitOr, BitXor, CompoundXor, Div, Eq, Ge, Gt, IdentityEq, IdentityNe, In,
        Instanceof, IntDiv, Is, Le, Lt, Mod, Mul, Ne, NotIn, Pow, ShiftL, ShiftR, Sub, UShiftR,
        Xor,
    };
    let op = match c {
        x if x == Add as i64 => Add,
        x if x == Sub as i64 => Sub,
        x if x == Mul as i64 => Mul,
        x if x == Div as i64 => Div,
        x if x == Mod as i64 => Mod,
        x if x == IntDiv as i64 => IntDiv,
        x if x == Pow as i64 => Pow,
        x if x == Eq as i64 => Eq,
        x if x == Ne as i64 => Ne,
        x if x == IdentityEq as i64 => IdentityEq,
        x if x == IdentityNe as i64 => IdentityNe,
        x if x == Lt as i64 => Lt,
        x if x == Le as i64 => Le,
        x if x == Gt as i64 => Gt,
        x if x == Ge as i64 => Ge,
        x if x == BitAnd as i64 => BitAnd,
        x if x == BitOr as i64 => BitOr,
        x if x == BitXor as i64 => BitXor,
        x if x == CompoundXor as i64 => CompoundXor,
        x if x == Xor as i64 => Xor,
        x if x == ShiftL as i64 => ShiftL,
        x if x == ShiftR as i64 => ShiftR,
        x if x == UShiftR as i64 => UShiftR,
        x if x == In as i64 => In,
        x if x == NotIn as i64 => NotIn,
        x if x == Is as i64 => Is,
        x if x == Instanceof as i64 => Instanceof,
        _ => return None,
    };
    Some(op)
}
