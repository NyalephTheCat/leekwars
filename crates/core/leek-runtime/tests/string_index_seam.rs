//! **Characterization test.** The string builtins do not agree on what an
//! index means, and this pins what they do today rather than what they
//! should do (#188).
//!
//! Two conventions are live in `builtins/string.rs` at once:
//!
//! - `charAt`, `charCodeAt` and `substring` index by Rust `char` — Unicode
//!   scalar values, so an emoji is *one* position;
//! - `codePointAt` indexes by UTF-16 code unit (`String.codePointAt(int)`
//!   upstream), so the same emoji is *two* positions, and so does `slice`
//!   (`eval.rs`, which documents itself as matching `stringSlice`).
//!
//! On any string that stays inside the BMP the two agree and nothing shows.
//! On a string with an astral character — an emoji in a chat message, say —
//! `charAt(s, i)` and `codePointAt(s, i)` are talking about different
//! characters.
//!
//! Java, which is the oracle here, is UTF-16 throughout, so the `char`-indexed
//! three are the ones likely to be wrong. Fixing them means changing three
//! builtins and revalidating the corpus, which is not this PR. **When someone
//! does that, this file is the thing to update, not the thing to defend** —
//! the assertions below record a divergence, they do not bless it.

use leek_runtime::{BuiltinHost, BuiltinResult, Value, call_builtin};

/// The minimum host `call_builtin` needs. None of the string builtins call
/// back or draw random numbers, so every hook here is unreachable in this
/// file.
struct PureHost;

impl BuiltinHost for PureHost {
    fn version(&self) -> u8 {
        4
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
        unreachable!("no string builtin invokes a callback")
    }
}

fn s(text: &str) -> Value {
    Value::String(std::rc::Rc::new(text.to_string()))
}

fn call(name: &str, args: &[Value]) -> Value {
    call_builtin(&mut PureHost, name, args).expect("string builtins never error")
}

fn text(v: &Value) -> String {
    match v {
        Value::String(s) => s.as_ref().clone(),
        other => panic!("expected a string, got {other:?}"),
    }
}

/// `"a😀b"`: one astral character between two ASCII ones. Three `char`s, four
/// UTF-16 code units.
const MIXED: &str = "a😀b";

#[test]
fn the_two_index_conventions_agree_on_a_bmp_string() {
    let plain = s("héllo");
    assert_eq!(text(&call("charAt", &[plain.clone(), Value::Int(1)])), "é");
    assert_eq!(
        call("codePointAt", &[plain.clone(), Value::Int(1)]).as_int(),
        Some(0xE9),
        "é is one code unit, so both conventions point at it",
    );
    assert_eq!(
        call("charCodeAt", &[plain, Value::Int(1)]).as_int(),
        Some(0xE9)
    );
}

#[test]
fn char_at_indexes_by_unicode_scalar_so_the_emoji_is_one_position() {
    let v = s(MIXED);
    assert_eq!(text(&call("charAt", &[v.clone(), Value::Int(0)])), "a");
    assert_eq!(text(&call("charAt", &[v.clone(), Value::Int(1)])), "😀");
    assert_eq!(
        text(&call("charAt", &[v.clone(), Value::Int(2)])),
        "b",
        "index 2 is already past the emoji",
    );
    // Out of range is the empty string, not null and not a panic.
    assert_eq!(text(&call("charAt", &[v, Value::Int(9)])), "");
}

#[test]
fn code_point_at_indexes_by_utf16_unit_so_the_emoji_spans_two_positions() {
    let v = s(MIXED);
    assert_eq!(
        call("codePointAt", &[v.clone(), Value::Int(0)]).as_int(),
        Some(0x61)
    );
    assert_eq!(
        call("codePointAt", &[v.clone(), Value::Int(1)]).as_int(),
        Some(0x1_F600),
        "index 1 is the high surrogate, and the pair is recombined",
    );
    assert_eq!(
        call("codePointAt", &[v.clone(), Value::Int(2)]).as_int(),
        Some(0xDE00),
        "index 2 is the *low* surrogate alone — a lone half of the emoji",
    );
    assert_eq!(
        call("codePointAt", &[v.clone(), Value::Int(3)]).as_int(),
        Some(0x62),
        "`b` sits at 3 here and at 2 for charAt: the divergence, in one line",
    );
    assert!(matches!(
        call("codePointAt", &[v, Value::Int(4)]),
        Value::Null
    ));
}

#[test]
fn char_code_at_follows_char_at_not_code_point_at() {
    let v = s(MIXED);
    // `charCodeAt` walks `chars()`, so index 1 is the whole emoji scalar —
    // where Java would answer with the high surrogate 0xD83D.
    assert_eq!(
        call("charCodeAt", &[v.clone(), Value::Int(1)]).as_int(),
        Some(0x1_F600),
    );
    assert_eq!(call("charCodeAt", &[v, Value::Int(2)]).as_int(), Some(0x62));
}

#[test]
fn substring_counts_in_scalars_while_slice_counts_in_code_units() {
    let v = s(MIXED);
    assert_eq!(
        text(&call(
            "substring",
            &[v.clone(), Value::Int(1), Value::Int(1)]
        )),
        "😀",
        "substring takes one *scalar* from position 1",
    );
    // `slice` (eval.rs) is UTF-16 indexed, so the same numbers cut the
    // surrogate pair in half and the lost half renders as U+FFFD.
    let sliced = text(&leek_runtime::slice(&v, Some(1), Some(2), None));
    assert_eq!(
        sliced.chars().count(),
        1,
        "one code unit came back, not one character: {sliced:?}",
    );
    assert_ne!(sliced, "😀", "the pair was split");
}

/// `length` decides which of the two an author's own index arithmetic will
/// be based on, so which convention it follows is the one that matters most.
#[test]
fn length_counts_scalars_like_char_at() {
    assert_eq!(call("length", &[s(MIXED)]).as_int(), Some(3));
    assert_eq!(
        s(MIXED).to_string().encode_utf16().count(),
        6,
        "for reference: 4 units plus the two quotes Display adds",
    );
}
