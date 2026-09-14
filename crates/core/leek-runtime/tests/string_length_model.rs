//! One length model for strings: Java's, in UTF-16 code units (#268 RT-03,
//! #336 RT-M3).
//!
//! Upstream is Java, and `StringClass.length` is `ai.ops(1); return
//! string.length();` (StringClass.java:29-32) — `String.length()` counts
//! UTF-16 code units. So does `AI.longint(String)`'s fallback
//! (AI.java:1155-1166), and so does every op charge upstream derives from a
//! string's size. This workspace used to answer with `chars().count()` in
//! some places, `encode_utf16().count()` in others and `len()` (bytes!) in a
//! third, so one string measured three different ways. Everything now routes
//! through `leek_runtime::jstr`, and this file is the cross-check: the same
//! strings put through `length`, `count`, `to_long` and the op-cost meter,
//! with the answer written out rather than derived.
//!
//! The three interesting shapes, and why each is here:
//!
//! - **ASCII** — the fast path every `jstr` function takes. Bytes, scalars
//!   and code units are all the same number, so it catches nothing on its
//!   own and everything if the fast path ever diverges from the slow one.
//! - **BMP, non-ASCII** — `"héllo"`, `"日本"`. One code unit per character,
//!   but *more than one byte*, so it separates the UTF-16 answer from the
//!   UTF-8 one.
//! - **Astral** — `"a😀b"`, `"😀😀"`. A surrogate pair per emoji, so it
//!   separates the UTF-16 answer from the scalar one. This is the case that
//!   was answered three ways.

use leek_runtime::{
    BuiltinHost, BuiltinResult, Value, builtin_cost, builtin_op_cost, call_builtin,
};
use std::rc::Rc;

/// A host that only has to report a language version: none of `length`,
/// `count` or the op meter calls back or draws random numbers.
struct VersionHost(u8);

impl BuiltinHost for VersionHost {
    fn version(&self) -> u8 {
        self.0
    }
    fn rng_int(&mut self, lo: i64, _hi: i64) -> i64 {
        lo
    }
    fn rng_real(&mut self, lo: f64, _hi: f64) -> f64 {
        lo
    }
    fn callback_arity(&self, _callee: &Value) -> Option<usize> {
        None
    }
    fn param_byref_mask(&self, _callee: &Value) -> Option<Vec<bool>> {
        None
    }
    fn call_value(&mut self, _callee: &Value, _args: Vec<Value>) -> BuiltinResult {
        unreachable!("neither length nor count invokes a callback")
    }
}

fn s(text: &str) -> Value {
    Value::String(Rc::new(text.to_string()))
}

fn call_at(version: u8, name: &str, args: &[Value]) -> Value {
    call_builtin(&mut VersionHost(version), name, args).expect("no error path here")
}

/// Every version the language has. `count` and `length` are checked against
/// all of them, because "which version is this" was exactly the thing that
/// used to change the answer.
const VERSIONS: [u8; 4] = [1, 2, 3, 4];

/// `(string, utf-16 length, bytes, unicode scalars)`. The last two columns
/// are not what anything returns — they are here so a wrong answer names
/// which of the three models produced it.
const STRINGS: &[(&str, i64, usize, usize)] = &[
    ("", 0, 0, 0),
    ("abc", 3, 3, 3),
    ("hello world", 11, 11, 11),
    ("héllo", 5, 6, 5),
    ("日本", 2, 6, 2),
    ("a😀b", 4, 6, 3),
    ("😀😀", 4, 8, 2),
];

#[test]
fn the_table_really_does_separate_the_three_models() {
    // Guard against a typo turning a discriminating row into a vacuous one:
    // some row must have bytes != units, and some row units != scalars.
    assert!(
        STRINGS
            .iter()
            .any(|&(_, u, b, _)| usize::try_from(u) != Ok(b)),
        "no row distinguishes UTF-16 units from UTF-8 bytes",
    );
    assert!(
        STRINGS
            .iter()
            .any(|&(_, u, _, c)| usize::try_from(u) != Ok(c)),
        "no row distinguishes UTF-16 units from Unicode scalars",
    );
    for &(text, units, bytes, scalars) in STRINGS {
        assert_eq!(text.len(), bytes, "byte column wrong for {text:?}");
        assert_eq!(
            text.chars().count(),
            scalars,
            "scalar column wrong for {text:?}",
        );
        assert_eq!(
            leek_runtime::jstr::len16(text),
            usize::try_from(units).expect("non-negative"),
            "unit column wrong for {text:?}",
        );
    }
}

/// `length(s)` is `String.length()` — UTF-16 code units, at every version.
#[test]
fn length_is_the_utf16_code_unit_count() {
    for &(text, units, ..) in STRINGS {
        for version in VERSIONS {
            assert_eq!(
                call_at(version, "length", &[s(text)]).as_int(),
                Some(units),
                "length({text:?}) at v{version}",
            );
        }
    }
}

/// `count(s)` is **0** for a string at every version. `count` is declared
/// over `Type.ARRAY` (`LeekFunctions.java:140`, no `setMinVersion`), so
/// upstream converts the receiver with `toLegacyArray` (v1–v3) or `toArray`
/// (v4) first and neither accepts a `String` — v4's throws
/// `ClassCastException` and the generated helper answers 0, v1–v3's yields
/// the empty fallback array. `reference.tsv` records `count('hello')` → 0 at
/// v1/v2/v3 and `count(unknown(12))` → 0 at all four.
///
/// This is not `length` with a different name, and it never was: the two
/// were only ever confusable at v4, where this workspace used to return the
/// character count.
#[test]
fn count_of_a_string_is_zero_at_every_version() {
    for &(text, ..) in STRINGS {
        for version in VERSIONS {
            assert_eq!(
                call_at(version, "count", &[s(text)]).as_int(),
                Some(0),
                "count({text:?}) at v{version}",
            );
        }
    }
    // The contrast, so the test is not just "count always answers 0":
    // a real array still counts.
    let arr = Value::Array(Rc::new(std::cell::RefCell::new(vec![
        Value::Int(1),
        Value::Int(2),
    ])));
    for version in VERSIONS {
        assert_eq!(
            call_at(version, "count", std::slice::from_ref(&arr)).as_int(),
            Some(2),
            "count([1, 2]) at v{version}",
        );
    }
}

/// `Value::to_long` on a string follows `AI.longint` (AI.java:1155-1166) in
/// order: the three literals, then `Long.parseLong`, then the UTF-16 length
/// as the fallback. The fallback is where the length model shows up — an
/// unparseable astral string answers with its code-unit count, not its byte
/// count and not its character count.
#[test]
fn to_long_parses_first_and_falls_back_to_the_utf16_length() {
    // The literals that short-circuit before any parse.
    assert_eq!(s("true").to_long(), 1, "not the length 4");
    assert_eq!(s("false").to_long(), 0, "not the length 5");
    assert_eq!(s("").to_long(), 0);
    // A parseable string is its value, whatever its length.
    assert_eq!(s("42").to_long(), 42);
    assert_eq!(s("-7").to_long(), -7);
    assert_eq!(s("0").to_long(), 0);
    // Everything else falls back to `String.length()`.
    for &(text, units, ..) in STRINGS {
        if text.is_empty() || text.parse::<i64>().is_ok() {
            continue;
        }
        assert_eq!(s(text).to_long(), units, "to_long({text:?})");
    }
}

/// `length`'s op charge is `ai.ops(1)` on top of the catalog cost — a
/// *constant*, independent of the string. Unlike `contains` or `toUpper`,
/// whose charges scale with `string.length()`, measuring a long string costs
/// no more than measuring a short one.
#[test]
fn the_length_op_charge_is_one_plus_the_catalog_cost_whatever_the_string() {
    let expected = builtin_cost("length") + 1;
    for &(text, ..) in STRINGS {
        for version in VERSIONS {
            assert_eq!(
                builtin_op_cost("length", &[s(text)], version),
                expected,
                "op cost of length({text:?}) at v{version}",
            );
        }
    }
    // And the charges that *do* scale count units, not bytes or scalars:
    // `toUpper` is `ops(1 + string.length())` (StringClass.java).
    for &(text, units, ..) in STRINGS {
        assert_eq!(
            builtin_op_cost("toUpper", &[s(text)], 4),
            builtin_cost("toUpper") + 1 + u64::try_from(units).expect("non-negative"),
            "op cost of toUpper({text:?})",
        );
    }
}
