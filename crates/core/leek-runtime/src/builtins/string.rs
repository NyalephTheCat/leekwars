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
        // `charAt(s, i)` — the *UTF-16 code unit* at `i`, as a one-unit
        // string. `StringClass.charAt` is
        // `if (index < 0 || index >= string.length()) return null;` then
        // `String.valueOf(string.charAt((int) index))`, so out of range is
        // **null**, not the empty string, and an astral character is
        // addressed by each of its two surrogate halves — half a pair is
        // not a `char` in Rust, so it renders as `U+FFFD`.
        ("charAt", 2) => match (&args[0], args[1].as_int()) {
            (Value::String(s), Some(i)) if i >= 0 => {
                match crate::jstr::unit_at(s, crate::clamp_index(i)) {
                    Some(u) => Value::String(Rc::new(String::from_utf16_lossy(&[u]))),
                    None => Value::Null,
                }
            }
            _ => Value::Null,
        },
        // `codePointAt(s)` — `StringClass.codePointAt(AI, String)`:
        // `if (string.length() == 0) return 0;` then `codePointAt(0)`. This
        // one-argument form had no arm at all, so it answered null.
        ("codePointAt", 1) => match &args[0] {
            Value::String(s) => {
                Value::Int(i64::from(crate::jstr::code_point_at(s, 0).unwrap_or(0)))
            }
            _ => Value::Null,
        },
        // `codePointAt(s, i)` — the code point starting at the i-th *UTF-16
        // code unit*, so an astral character is addressed by its high
        // surrogate index and the pair is recombined (that recombination
        // lives in `jstr::code_point_at`, which mirrors
        // `String.codePointAt(int)`).
        //
        // Out of range is **0**, not null: `StringClass.codePointAt` opens
        // with `if (index < 0 || index >= string.length()) return 0;`.
        ("codePointAt", 2) => match (&args[0], args[1].as_int()) {
            (Value::String(s), Some(i)) if i >= 0 => Value::Int(i64::from(
                crate::jstr::code_point_at(s, crate::clamp_index(i)).unwrap_or(0),
            )),
            (Value::String(_), Some(_)) => Value::Int(0),
            _ => Value::Null,
        },
        ("fromCodePoint", 1) => match args[0].as_int() {
            Some(i) => {
                let c = char::from_u32(u32::try_from(i.max(0)).unwrap_or(0)).unwrap_or('\u{0}');
                Value::String(Rc::new(c.to_string()))
            }
            _ => Value::Null,
        },
        // `substring(s, index)` — `StringClass.substring(AI, String, long)`
        // guards with `if (string.length() <= index || index < 0) return
        // null;` and then takes `[index, length())` in *UTF-16* positions.
        // Both halves matter: an index at or past the end is null (so
        // `substring("abc", 3)` is null, not `""`), and a negative one is
        // null rather than a clamp to 0.
        ("substring", 2) => match (&args[0], args[1].as_int()) {
            (Value::String(s), Some(index)) => {
                let len = crate::len_as_int(crate::jstr::len16(s));
                if len <= index || index < 0 {
                    Value::Null
                } else {
                    substring16_value(s, index, len)
                }
            }
            _ => return None,
        },
        // `substring(s, index, length)` — the same, plus upstream's two
        // further guards: `index + length > string.length() || length < 0`.
        // A window that runs off the end is null, not a short string.
        ("substring", 3) => match (&args[0], args[1].as_int(), args[2].as_int()) {
            (Value::String(s), Some(index), Some(length)) => {
                let len = crate::len_as_int(crate::jstr::len16(s));
                // `index + length` is compared the way Java writes it; the
                // saturating add only stops a hostile `Number.MAX_VALUE`
                // pair from wrapping into a window that looks legal.
                if len <= index || index < 0 || index.saturating_add(length) > len || length < 0 {
                    Value::Null
                } else {
                    substring16_value(s, index, index + length)
                }
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
        // `charCodeAt` is a local extension — `StringClass.java` has no such
        // method, so there is no upstream result to match and no corpus row
        // pins it. What it *can* do is agree with its neighbours: it indexes
        // UTF-16 code units like `charAt`, `codePointAt` and `s[i]`, which
        // makes `charCodeAt(s, i)` the numeric form of `charAt(s, i)` at
        // every index. On a surrogate it therefore answers the surrogate
        // (0xD83D…), not the character the pair encodes — `codePointAt` is
        // the one that recombines. Null out of range, as before.
        ("charCodeAt" | "stringCharCodeAt", 2) => match (&args[0], args[1].as_int()) {
            (Value::String(s), Some(i)) if i >= 0 => crate::jstr::unit_at(s, crate::clamp_index(i))
                .map_or(Value::Null, |u| Value::Int(i64::from(u))),
            _ => Value::Null,
        },
        ("chr" | "fromCharCode", 1) => {
            if let Some(i) = args[0].as_int()
                && let Some(c) = char::from_u32(u32::try_from(i).unwrap_or(u32::MAX))
            {
                return Some(Value::String(Rc::new(c.to_string())));
            }
            Value::Null
        }
        // `ord(s)` is `charCodeAt(s, 0)` spelled shorter, so it reads the
        // first *code unit*: on an astral first character that is the high
        // surrogate rather than the whole scalar, and the two agree.
        ("ord", 1) => match &args[0] {
            Value::String(s) => {
                crate::jstr::unit_at(s, 0).map_or(Value::Null, |u| Value::Int(i64::from(u)))
            }
            _ => return None,
        },
        _ => return None,
    })
}

/// `s.substring(start, end)` in UTF-16 positions, as a `Value`.
///
/// Callers apply upstream's guards first, so a `None` from
/// [`crate::jstr::substring16`] here would mean the guards and the slice
/// disagree; null is the safe answer rather than a panic.
fn substring16_value(s: &str, start: i64, end: i64) -> Value {
    crate::jstr::substring16(s, crate::clamp_index(start), crate::clamp_index(end))
        .map_or(Value::Null, |out| Value::String(Rc::new(out)))
}

// ---- Misc ----
