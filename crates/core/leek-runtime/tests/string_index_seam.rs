//! **Agreement test.** Every string builtin that takes a position indexes the
//! same thing: the string's UTF-16 code units, the unit Java's
//! `String.length()` counts and `String.charAt(int)` addresses (#268 RT-03,
//! #356).
//!
//! This file used to be a characterization test, recording a split: `charAt`,
//! `charCodeAt` and `substring` walked Rust `char`s (Unicode scalars, so an
//! emoji was *one* position) while `length`, `codePointAt`, `s[i]` and
//! `slice` counted UTF-16 units (so the same emoji was *two*). On a chat
//! message with an emoji in it, `charAt(s, i)` and `s[i]` were talking about
//! different characters, and an author's own index arithmetic — built on
//! `length` — agreed with only half of them.
//!
//! The `char`-indexed three moved onto the UTF-16 side, because upstream is
//! Java and Java is UTF-16 throughout. What is left is one index space, and
//! the table below asserts it: for every position of every input, the
//! builtins answer about the *same* code unit.
//!
//! Two things are deliberately not agreement, and are pinned separately at
//! the bottom:
//!
//! - out-of-range results differ by builtin, and each follows its own
//!   upstream method — `charAt` and `charCodeAt` answer null, `codePointAt`
//!   answers 0, `substring` answers null, `slice` clamps to `""`;
//! - `s[i]` wraps a negative index around the end of the string (upstream
//!   `getString`) where `charAt` rejects it (upstream `StringClass.charAt`).

use leek_runtime::{BuiltinHost, BuiltinResult, Value, call_builtin, read_index, slice};

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

/// One input, with its index space spelled out.
struct Row {
    text: &'static str,
    /// The UTF-16 code units of `text`, written out rather than computed:
    /// this is the index space the whole file asserts against, so it is
    /// stated, not derived from the same encoder under test.
    units: &'static [u16],
    /// What `codePointAt` reads at each of those positions — the same as
    /// `units` except where a well-formed surrogate pair *starts*, which
    /// recombines into the character it encodes.
    code_points: &'static [u32],
    /// A substring of `text`, and the UTF-16 position it occurs at.
    needle: (&'static str, i64),
}

/// ASCII, BMP-but-not-ASCII, and astral — the three cases where the byte,
/// scalar and code-unit views of a string come apart.
const ROWS: &[Row] = &[
    // ASCII: bytes, scalars and code units are all the same number, so this
    // row is the control — every builtin agreed here even before the fix.
    Row {
        text: "bonjour",
        units: &[0x62, 0x6F, 0x6E, 0x6A, 0x6F, 0x75, 0x72],
        code_points: &[0x62, 0x6F, 0x6E, 0x6A, 0x6F, 0x75, 0x72],
        needle: ("jour", 3),
    },
    // BMP: `é` and `à` are two bytes each but one code unit, so a byte-indexed
    // answer (what `str::find` returns) shows up here as a position that is
    // too large.
    Row {
        text: "héllo là",
        units: &[0x68, 0xE9, 0x6C, 0x6C, 0x6F, 0x20, 0x6C, 0xE0],
        code_points: &[0x68, 0xE9, 0x6C, 0x6C, 0x6F, 0x20, 0x6C, 0xE0],
        needle: ("là", 6),
    },
    // Astral: the emoji is one scalar and *two* code units, so a
    // scalar-indexed answer shows up as a position that is too small — and
    // each half of the pair is addressable on its own.
    Row {
        text: "a😀b",
        units: &[0x61, 0xD83D, 0xDE00, 0x62],
        code_points: &[0x61, 0x1_F600, 0xDE00, 0x62],
        needle: ("b", 3),
    },
    // Two astral characters back to back: position 2 is the *second* koala's
    // high surrogate, the case the upstream corpus pins
    // (`codePointAt('🐨🐨', 2)` → 128040).
    Row {
        text: "🐨🐨",
        units: &[0xD83D, 0xDC28, 0xD83D, 0xDC28],
        code_points: &[0x1_F428, 0xDC28, 0x1_F428, 0xDC28],
        needle: ("🐨", 0),
    },
];

/// The one-unit string a UTF-16 position holds. Half a surrogate pair is not
/// a Rust `char`, so it renders as `U+FFFD` — the only place the Rust answer
/// cannot be spelled the way Java's can.
fn unit_text(unit: u16) -> String {
    String::from_utf16_lossy(&[unit])
}

fn len16(row: &Row) -> i64 {
    i64::try_from(row.units.len()).expect("test inputs are tiny")
}

/// Every position-taking builtin, at every position of every input, answers
/// about the same code unit. This is the whole point of the file: one index
/// space, eight builtins.
#[test]
fn every_indexing_builtin_agrees_on_utf16_positions() {
    for row in ROWS {
        let v = s(row.text);
        let len = len16(row);
        assert_eq!(
            call("length", std::slice::from_ref(&v)).as_int(),
            Some(len),
            "length({:?}) is the number of positions the rest of the table uses",
            row.text,
        );
        for (i, &unit) in row.units.iter().enumerate() {
            let at = i64::try_from(i).expect("test inputs are tiny");
            let here = unit_text(unit);
            let what = format!("{:?} at {at}", row.text);

            assert_eq!(
                text(&call("charAt", &[v.clone(), Value::Int(at)])),
                here,
                "charAt: {what}",
            );
            assert_eq!(
                call("charCodeAt", &[v.clone(), Value::Int(at)]).as_int(),
                Some(i64::from(unit)),
                "charCodeAt: {what}",
            );
            assert_eq!(
                call("stringCharCodeAt", &[v.clone(), Value::Int(at)]).as_int(),
                Some(i64::from(unit)),
                "stringCharCodeAt is the same builtin under its other name: {what}",
            );
            assert_eq!(
                call("codePointAt", &[v.clone(), Value::Int(at)]).as_int(),
                Some(i64::from(row.code_points[i])),
                "codePointAt: {what}",
            );
            assert_eq!(
                text(&call(
                    "substring",
                    &[v.clone(), Value::Int(at), Value::Int(1)]
                )),
                here,
                "substring of length 1: {what}",
            );
            assert_eq!(
                text(&call("substring", &[v.clone(), Value::Int(at)])),
                String::from_utf16_lossy(&row.units[i..]),
                "substring to the end: {what}",
            );
            assert_eq!(text(&read_index(&v, &Value::Int(at))), here, "s[i]: {what}",);
            assert_eq!(
                text(&slice(&v, Some(at), Some(at + 1), None)),
                here,
                "slice of width 1: {what}",
            );
        }

        assert_eq!(
            call("ord", std::slice::from_ref(&v)).as_int(),
            Some(i64::from(row.units[0])),
            "ord({:?}) is charCodeAt(s, 0)",
            row.text,
        );
        assert_eq!(
            call("codePointAt", std::slice::from_ref(&v)).as_int(),
            Some(i64::from(row.code_points[0])),
            "codePointAt(s) is codePointAt(s, 0)",
        );

        // `indexOf` answers in the same positions, and takes its `from` in
        // them too: the needle is found at its own position and not one
        // later.
        let (needle, at) = row.needle;
        let n = s(needle);
        assert_eq!(
            call("indexOf", &[v.clone(), n.clone()]).as_int(),
            Some(at),
            "indexOf({:?}, {needle:?})",
            row.text,
        );
        assert_eq!(
            call("indexOf", &[v.clone(), n.clone(), Value::Int(at)]).as_int(),
            Some(at),
            "indexOf from the match's own position finds it",
        );
        assert_eq!(
            call("search", &[v.clone(), n.clone()]).as_int(),
            Some(at),
            "`search` is the same dispatch entry as `indexOf`",
        );
        assert_eq!(
            text(&call(
                "substring",
                &[
                    v.clone(),
                    Value::Int(at),
                    Value::Int(
                        i64::try_from(needle.encode_utf16().count()).expect("test inputs are tiny"),
                    )
                ]
            )),
            needle,
            "the position indexOf reports is the one substring cuts at",
        );
    }
}

/// Out of range, the builtins deliberately *don't* agree — each reproduces
/// its own upstream method, and the differences are load-bearing for programs
/// that test the result.
#[test]
fn out_of_range_results_follow_each_upstream_method() {
    for row in ROWS {
        let v = s(row.text);
        let len = len16(row);
        let what = row.text;

        // `StringClass.charAt`: `index < 0 || index >= length()` → null.
        for out in [len, len + 3, -1] {
            assert!(
                matches!(call("charAt", &[v.clone(), Value::Int(out)]), Value::Null),
                "charAt({what:?}, {out}) is null, not \"\"",
            );
            assert!(
                matches!(
                    call("charCodeAt", &[v.clone(), Value::Int(out)]),
                    Value::Null
                ),
                "charCodeAt({what:?}, {out}) is null",
            );
            // `StringClass.codePointAt`: the same range test, but **0**.
            assert_eq!(
                call("codePointAt", &[v.clone(), Value::Int(out)]).as_int(),
                Some(0),
                "codePointAt({what:?}, {out}) is 0, not null",
            );
            // `StringClass.substring`: `length() <= index || index < 0` → null.
            assert!(
                matches!(
                    call("substring", &[v.clone(), Value::Int(out)]),
                    Value::Null
                ),
                "substring({what:?}, {out}) is null",
            );
            assert!(
                matches!(
                    call("substring", &[v.clone(), Value::Int(out), Value::Int(1)]),
                    Value::Null
                ),
                "substring({what:?}, {out}, 1) is null",
            );
        }

        // The 3-argument form's two extra guards: a window that runs off the
        // end, and a negative length.
        assert!(
            matches!(
                call(
                    "substring",
                    &[v.clone(), Value::Int(0), Value::Int(len + 1)]
                ),
                Value::Null
            ),
            "substring({what:?}, 0, {}) runs past the end → null",
            len + 1,
        );
        assert!(
            matches!(
                call(
                    "substring",
                    &[v.clone(), Value::Int(len - 1), Value::Int(2)]
                ),
                Value::Null
            ),
            "a window starting inside and ending outside is null too",
        );
        assert!(
            matches!(
                call("substring", &[v.clone(), Value::Int(0), Value::Int(-1)]),
                Value::Null
            ),
            "a negative length is null",
        );
        assert_eq!(
            text(&call(
                "substring",
                &[v.clone(), Value::Int(0), Value::Int(len)]
            )),
            what,
            "the whole string is in range, right up to the last unit",
        );

        // Not found: the two arities answer differently, and always have —
        // `indexOf(s, needle)` is null, the `from` form is -1.
        assert!(
            matches!(call("indexOf", &[v.clone(), s("\u{2603}")]), Value::Null),
            "indexOf({what:?}, ☃) is null",
        );
        assert_eq!(
            call("indexOf", &[v.clone(), s("\u{2603}"), Value::Int(0)]).as_int(),
            Some(-1),
            "the 3-argument form answers -1",
        );
        assert_eq!(
            call(
                "indexOf",
                &[v.clone(), s(row.needle.0), Value::Int(len + 5)]
            )
            .as_int(),
            Some(-1),
            "a `from` past the end finds nothing, even a needle that is there",
        );

        // `slice` clamps instead of failing: past the end is the empty
        // string, and a negative start counts back from the end.
        assert_eq!(
            text(&slice(&v, Some(len), Some(len + 5), None)),
            "",
            "slice past the end is empty",
        );
        assert_eq!(
            text(&slice(&v, Some(-1), None, None)),
            unit_text(row.units[row.units.len() - 1]),
            "slice's negative start counts back in code units",
        );

        // `s[i]` is upstream `getString`, which wraps a negative index once
        // and only then rejects — the one index rule on this page that is
        // not `charAt`'s.
        assert_eq!(
            text(&read_index(&v, &Value::Int(-1))),
            unit_text(row.units[row.units.len() - 1]),
            "s[-1] is the last code unit",
        );
        assert!(
            matches!(read_index(&v, &Value::Int(len)), Value::Null),
            "s[length] is null",
        );
        assert!(
            matches!(read_index(&v, &Value::Int(-len - 1)), Value::Null),
            "a negative index that wraps past the front is null",
        );
    }
}

/// The empty string is every out-of-range case at once, and the arity-1
/// builtins have their own answer for it.
#[test]
fn the_empty_string_has_no_positions() {
    let empty = s("");
    assert_eq!(
        call("length", std::slice::from_ref(&empty)).as_int(),
        Some(0)
    );
    assert!(matches!(
        call("charAt", &[empty.clone(), Value::Int(0)]),
        Value::Null
    ));
    assert!(matches!(
        call("charCodeAt", &[empty.clone(), Value::Int(0)]),
        Value::Null
    ));
    assert!(
        matches!(call("ord", std::slice::from_ref(&empty)), Value::Null),
        "`ord` has no code unit to read",
    );
    assert_eq!(
        call("codePointAt", std::slice::from_ref(&empty)).as_int(),
        Some(0),
        "`StringClass.codePointAt(ai, string)` opens with \
         `if (string.length() == 0) return 0;`",
    );
    assert_eq!(
        call("codePointAt", &[empty.clone(), Value::Int(0)]).as_int(),
        Some(0),
    );
    assert!(
        matches!(
            call("substring", &[empty.clone(), Value::Int(0)]),
            Value::Null
        ),
        "`length() <= index` holds at 0 <= 0, so even the empty window is null",
    );
    assert!(matches!(read_index(&empty, &Value::Int(0)), Value::Null));
    assert_eq!(text(&slice(&empty, Some(0), Some(1), None)), "");
}
