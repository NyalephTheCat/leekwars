//! Builtin string dispatch.
//!
//! The Leekscript stdlib has hundreds of free functions; this is a
//! best-effort subset focused on what the upstream test corpus
//! exercises. Anything unrecognised returns `null`, matching
//! upstream's "missing builtin" runtime behavior.

use std::rc::Rc;

use crate::value::Value;

use super::array::split_string;
use super::misc::value_as_concat_string_for_join;

pub(crate) fn dispatch_string(name: &str, args: &[Value]) -> Option<Value> {
    Some(match (name, args.len()) {
        ("toUpper", 1) => match &args[0] {
            Value::String(s) => Value::String(Rc::new(s.to_uppercase())),
            _ => return None,
        },
        ("toLower", 1) => match &args[0] {
            Value::String(s) => Value::String(Rc::new(s.to_lowercase())),
            _ => return None,
        },
        ("charAt", 2) => {
            if let (Value::String(s), Some(i)) = (&args[0], args[1].as_int()) {
                let chars: Vec<char> = s.chars().collect();
                if i >= 0 && (crate::clamp_index(i)) < chars.len() {
                    return Some(Value::String(Rc::new(chars[crate::clamp_index(i)].to_string())));
                }
            }
            Value::String(Rc::new(String::new()))
        }
        // `codePointAt(s, i)` — Unicode code point at the i-th
        // *UTF-16 code unit* (matches Java's
        // `String.codePointAt(int)`). Surrogates count separately
        // so multi-unit emoji are addressable by their high
        // surrogate index. Out-of-range / non-string returns `null`.
        ("codePointAt", 2) => match (&args[0], args[1].as_int()) {
            (Value::String(s), Some(i)) if i >= 0 => {
                let units: Vec<u16> = s.encode_utf16().collect();
                let idx = crate::clamp_index(i);
                if idx >= units.len() {
                    Value::Null
                } else {
                    let hi = units[idx];
                    // Surrogate pair: combine with the following
                    // low surrogate into a full Unicode codepoint.
                    if (0xD800..=0xDBFF).contains(&hi) && idx + 1 < units.len() {
                        let lo = units[idx + 1];
                        if (0xDC00..=0xDFFF).contains(&lo) {
                            let cp =
                                0x10000 + (((u32::from(hi)) - 0xD800) << 10) + ((u32::from(lo)) - 0xDC00);
                            return Some(Value::Int(i64::from(cp)));
                        }
                    }
                    Value::Int(i64::from(hi))
                }
            }
            _ => Value::Null,
        },
        ("fromCodePoint", 1) => match args[0].as_int() {
            Some(i) => {
                let c = char::from_u32(u32::try_from(i.max(0)).unwrap_or(0)).unwrap_or('\u{0}');
                Value::String(Rc::new(c.to_string()))
            }
            _ => Value::Null,
        },
        ("substring", 2) => match (&args[0], args[1].as_int()) {
            (Value::String(s), Some(start)) => {
                let chars: Vec<char> = s.chars().collect();
                let start = crate::clamp_index(start);
                let out: String = chars.into_iter().skip(start).collect();
                Value::String(Rc::new(out))
            }
            _ => return None,
        },
        ("substring", 3) => match (&args[0], args[1].as_int(), args[2].as_int()) {
            (Value::String(s), Some(start), Some(len)) => {
                let chars: Vec<char> = s.chars().collect();
                let start = crate::clamp_index(start);
                let len = crate::clamp_index(len);
                let out: String = chars.into_iter().skip(start).take(len).collect();
                Value::String(Rc::new(out))
            }
            _ => return None,
        },
        ("split", 2) => split_string(&args[0], &args[1], None),
        ("split", 3) => split_string(&args[0], &args[1], Some(&args[2])),
        ("join", 2) => match (&args[0], &args[1]) {
            (Value::Array(a), Value::String(sep)) => {
                // Join the array with bare-string elements (no
                // quoting) — matches Java `String.join` over the
                // array's element `toString` form. Our Display
                // for `Value::String` quotes strings; use the
                // `value_as_concat_string` helper to strip the
                // quotes for primitive strings.
                let parts: Vec<String> = a
                    .borrow()
                    .iter()
                    .map(value_as_concat_string_for_join)
                    .collect();
                Value::String(Rc::new(parts.join(sep)))
            }
            _ => return None,
        },
        ("replace", 3) => match (&args[0], &args[1], &args[2]) {
            (Value::String(s), Value::String(from), Value::String(to)) => {
                Value::String(Rc::new(s.replace(from.as_str(), to.as_str())))
            }
            _ => return None,
        },
        ("startsWith", 2) => match (&args[0], &args[1]) {
            (Value::String(s), Value::String(p)) => Value::Bool(s.starts_with(p.as_str())),
            _ => return None,
        },
        ("endsWith", 2) => match (&args[0], &args[1]) {
            (Value::String(s), Value::String(p)) => Value::Bool(s.ends_with(p.as_str())),
            _ => return None,
        },
        ("trim", 1) => match &args[0] {
            Value::String(s) => Value::String(Rc::new(s.trim().to_string())),
            _ => return None,
        },
        ("repeat" | "stringRepeat", 2) => {
            if let (Value::String(s), Some(n)) = (&args[0], args[1].as_int())
                && n >= 0
            {
                return Some(Value::String(Rc::new(s.repeat(crate::clamp_index(n)))));
            }
            return None;
        }
        ("charCodeAt" | "stringCharCodeAt", 2) => {
            if let (Value::String(s), Some(i)) = (&args[0], args[1].as_int()) {
                let chars: Vec<char> = s.chars().collect();
                if i >= 0 && (crate::clamp_index(i)) < chars.len() {
                    return Some(Value::Int(chars[crate::clamp_index(i)] as i64));
                }
            }
            Value::Null
        }
        ("chr" | "fromCharCode", 1) => {
            if let Some(i) = args[0].as_int()
                && let Some(c) = char::from_u32(u32::try_from(i).unwrap_or(u32::MAX))
            {
                return Some(Value::String(Rc::new(c.to_string())));
            }
            Value::Null
        }
        ("ord", 1) => match &args[0] {
            Value::String(s) => s
                .chars()
                .next()
                .map_or(Value::Null, |c| Value::Int(c as i64)),
            _ => return None,
        },
        _ => return None,
    })
}

// ---- Misc ----
