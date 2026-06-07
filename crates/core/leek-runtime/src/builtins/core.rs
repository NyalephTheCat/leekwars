//! Builtin core dispatch.
//!
//! The Leekscript stdlib has hundreds of free functions; this is a
//! best-effort subset focused on what the upstream test corpus
//! exercises. Anything unrecognised returns `null`, matching
//! upstream's "missing builtin" runtime behavior.

use std::rc::Rc;

use crate::value::Value;

pub(crate) fn dispatch_constant(name: &str) -> Option<Value> {
    Some(match name {
        "PI" => Value::Real(std::f64::consts::PI),
        "INFINITY" | "Infinity" => Value::Real(f64::INFINITY),
        "NAN" | "NaN" => Value::Real(f64::NAN),
        "E" => Value::Real(std::f64::consts::E),
        // Type tags upstream uses for `typeOf` — see
        // `runner/values/LeekValueType.java`.
        "TYPE_NULL" => Value::Int(0),
        "TYPE_NUMBER" => Value::Int(1),
        "TYPE_BOOLEAN" => Value::Int(2),
        "TYPE_STRING" => Value::Int(3),
        "TYPE_ARRAY" => Value::Int(4),
        "TYPE_FUNCTION" => Value::Int(5),
        "TYPE_CLASS" => Value::Int(6),
        "TYPE_OBJECT" => Value::Int(7),
        "TYPE_MAP" => Value::Int(8),
        "TYPE_SET" => Value::Int(9),
        "TYPE_INTERVAL" => Value::Int(10),
        // Sort flags.
        "SORT_ASC" => Value::Int(0),
        "SORT_DESC" => Value::Int(1),
        "SORT_RANDOM" => Value::Int(2),
        _ => return None,
    })
}

// ---- Unary math ----

pub(crate) fn dispatch_unary_math(name: &str, args: &[Value]) -> Option<Value> {
    // Upstream's math builtins are tolerant of arity: zero args
    // means "operate on 0", extra args are ignored (only the
    // first matters). Mirror that so `sqrt()` returns `0` and
    // `sqrt(25, 16, 9)` returns `5` like the corpus expects.
    let zero = Value::Int(0);
    let a = args.first().unwrap_or(&zero);
    Some(match name {
        "abs" => match a {
            Value::Int(i) => Value::Int(i.wrapping_abs()),
            Value::Real(r) => Value::Real(r.abs()),
            Value::Bool(b) => Value::Int(i64::from(*b)),
            // `abs(null)` returns `0.0` (real, not int) — matches
            // upstream's `LeekFunctions.abs` return type promotion.
            Value::Null => Value::Real(0.0),
            _ => return None,
        },
        // The pure scalar-math family delegates to the shared
        // implementations in `crate::builtin`, so the interpreter
        // and the native backend can never drift on these semantics.
        "sqrt" => Value::Real(crate::leek_sqrt(a.as_real()?)),
        "cbrt" => Value::Real(crate::leek_cbrt(a.as_real()?)),
        "ceil" => Value::Int(crate::leek_ceil(a.as_real()?)),
        "floor" => Value::Int(crate::leek_floor(a.as_real()?)),
        "round" => Value::Int(crate::leek_round(a.as_real()?)),
        "sin" => Value::Real(crate::leek_sin(a.as_real()?)),
        "cos" => Value::Real(crate::leek_cos(a.as_real()?)),
        "tan" => Value::Real(crate::leek_tan(a.as_real()?)),
        "asin" => Value::Real(crate::leek_asin(a.as_real()?)),
        "acos" => Value::Real(crate::leek_acos(a.as_real()?)),
        "atan" => Value::Real(crate::leek_atan(a.as_real()?)),
        "sinh" => Value::Real(crate::leek_sinh(a.as_real()?)),
        "cosh" => Value::Real(crate::leek_cosh(a.as_real()?)),
        "tanh" => Value::Real(crate::leek_tanh(a.as_real()?)),
        "exp" => Value::Real(crate::leek_exp(a.as_real()?)),
        "log" => Value::Real(crate::leek_log(a.as_real()?)),
        "log10" => Value::Real(crate::leek_log10(a.as_real()?)),
        "log2" => Value::Real(crate::leek_log2(a.as_real()?)),
        "signum" => match a.as_real() {
            Some(r) if r > 0.0 => Value::Int(1),
            Some(r) if r < 0.0 => Value::Int(-1),
            Some(_) => Value::Int(0),
            None => return None,
        },
        // `number(v)` returns int when v has no decimal, real
        // otherwise. Matches `ValueClass.number`.
        "number" => match a {
            Value::Int(_) | Value::Real(_) => a.clone(),
            Value::Bool(b) => Value::Int(i64::from(*b)),
            Value::String(s) => {
                if s.contains('.') {
                    Value::Real(s.parse::<f64>().unwrap_or(0.0))
                } else {
                    Value::Int(s.parse::<i64>().unwrap_or(0))
                }
            }
            _ => Value::Int(0),
        },
        "typeOf" => Value::Int(type_tag(a)),
        // `unknown(x)` — identity, used in upstream tests to
        // relax the static type. See `ValueClass.unknown`.
        "unknown" => a.clone(),
        // `string(v)` returns the bare string for strings, the
        // `export`/`toString`-equivalent otherwise. Mirrors
        // `ValueClass.string`.
        // `string(v)` mirrors Java's `String.valueOf(v)` — bare
        // strings (no enclosing quotes) for primitives AND for
        // strings nested inside containers. The default
        // `Display` quotes nested strings (to match upstream's
        // `.toString()`), so we use a dedicated formatter here.
        "string" => Value::String(Rc::new(crate::value::value_as_bare_string(a))),
        "length" => length_of(a)?,
        // Debug helpers — upstream signature is `void debug(value)`.
        // The arg is "logged" (we drop it) and the call returns
        // `null`.
        "debug" | "debugC" | "debugE" | "debugW" => Value::Null,
        // v4 number/bit helpers.
        "toDegrees" => Value::Real(crate::leek_to_degrees(a.as_real()?)),
        "toRadians" => Value::Real(crate::leek_to_radians(a.as_real()?)),
        "isFinite" => Value::Bool(a.as_real().is_none_or(f64::is_finite)),
        "isInfinite" => Value::Bool(a.as_real().is_some_and(f64::is_infinite)),
        "isNaN" => Value::Bool(a.as_real().is_some_and(f64::is_nan)),
        "bitCount" => Value::Int(i64::from(a.as_int()?.count_ones())),
        "leadingZeros" => Value::Int(i64::from(a.as_int()?.leading_zeros())),
        "trailingZeros" => Value::Int(i64::from(a.as_int()?.trailing_zeros())),
        "bitReverse" => Value::Int(a.as_int()?.reverse_bits()),
        "byteReverse" => Value::Int(a.as_int()?.swap_bytes()),
        "binString" => Value::String(Rc::new(format!("{:b}", a.as_int()?))),
        "hexString" => Value::String(Rc::new(format!("{:x}", a.as_int()?))),
        // `realBits`/`bitsToReal` reinterpret the IEEE-754 bit pattern, so the
        // signed/unsigned casts are deliberate (no checked form applies).
        #[allow(clippy::cast_possible_wrap)]
        "realBits" => Value::Int(a.as_real()?.to_bits() as i64),
        #[allow(clippy::cast_sign_loss)]
        "bitsToReal" => Value::Real(f64::from_bits(a.as_int()? as u64)),
        _ => return None,
    })
}

/// `typeOf` tag — matches `LeekValueType` constants in upstream.
/// Both `Int` and `Real` map to `NUMBER` (1).
pub(crate) fn type_tag(v: &Value) -> i64 {
    match v {
        Value::Null => 0,
        Value::Int(_) | Value::Real(_) => 1,
        Value::Bool(_) => 2,
        Value::String(_) => 3,
        Value::Array(_) => 4,
        Value::Function(_) => 5,
        Value::ClassRef(_, _) | Value::BuiltinClass(_) => 6,
        Value::Object(_) | Value::Instance(_) => 7,
        Value::Map(_) => 8,
        Value::Set(_) => 9,
        Value::Interval(_) => 10,
        Value::Super { .. } => 7,
        Value::Cell(c) => type_tag(&c.borrow()),
    }
}

pub(crate) fn length_of(v: &Value) -> Option<Value> {
    Some(match v {
        Value::String(s) => Value::Int(crate::len_as_int(s.chars().count())),
        Value::Array(a) => Value::Int(crate::len_as_int(a.borrow().len())),
        Value::Map(m) => Value::Int(crate::len_as_int(m.borrow().len())),
        Value::Set(s) => Value::Int(crate::len_as_int(s.borrow().len())),
        _ => return None,
    })
}

// ---- Array operations ----
