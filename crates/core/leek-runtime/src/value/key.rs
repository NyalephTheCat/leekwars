//! Canonical map/set keys.
//!
//! Maps and sets compare keys by a *canonical form*, not by
//! [`Value`]'s loose equality: `5`, `5.0` and `"5"` are three
//! distinct keys, every `NaN` is one key, `0.0` and `-0.0` are two,
//! and `5L` (`big_integer`) never collides with `5`. That form used
//! to be the string produced by [`key_repr`](super::key_repr), which
//! meant one heap allocation per lookup — including the `Int` hot
//! path that stress tests hammer with millions of `map[i] = …`
//! writes.
//!
//! [`MapKey`] is the same equivalence relation expressed as a typed
//! enum. Primitive keys (null/bool/int/real) carry their payload
//! inline and allocate nothing; string and `big_integer` keys share
//! the `Value`'s existing `Rc`, so building one is a refcount bump.
//! Only composite keys — arrays, maps, objects, instances, … — still
//! render, and those already went through the cycle-aware `Display`
//! writer.
//!
//! `key_repr` stays as the documented canonical *string* form and as
//! the oracle the equivalence test diffs against: for every pair of
//! values, `MapKey::of(a) == MapKey::of(b)` exactly when
//! `key_repr(a) == key_repr(b)` (see `tests/map_key_equivalence.rs`).

use std::rc::Rc;

use super::types::Value;

/// Canonical key of a map entry or set element.
///
/// Two values are the same key iff their `MapKey`s compare equal.
/// The variant discriminant stands in for the type prefix `key_repr`
/// writes (`i:`, `r:`, `b:`, `s:`, `I:`), so keys of different kinds
/// can never collide however their contents are spelled — the string
/// `"i:5"` is still a different key from the integer `5`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum MapKey {
    Null,
    Bool(bool),
    Int(i64),
    /// Raw `f64` bits. Bits rather than the value because `f64` is
    /// neither `Eq` nor `Hash`, and because the bit pattern
    /// reproduces `key_repr`'s two special cases for free: `-0.0`
    /// keeps its sign bit so it stays a different key from `0.0`
    /// (`r:-0` vs `r:0`), and every `NaN` is normalised to one
    /// pattern so all of them are the same key (`r:NaN`). Otherwise
    /// `{}` on `f64` is shortest-round-trip, so distinct bits print
    /// distinctly.
    Real(u64),
    /// Shares [`Value::String`]'s allocation — cloning is a refcount
    /// bump, not a copy.
    Str(Rc<String>),
    /// `big_integer` keys are distinct from integer keys upstream
    /// (`BigIntegerValue.equals` only matches another
    /// `BigIntegerValue`), so `5` and `5L` coexist in one map.
    /// `BigInt: Eq` is full-precision, matching `key_repr`'s use of
    /// the uncropped decimal rather than the cropped display form.
    BigInt(Rc<num_bigint::BigInt>),
    /// Composite key — the rendered `Display` form, byte-identical
    /// to what `key_repr` writes for the same value. The only arm
    /// that still allocates.
    Other(Rc<String>),
}

impl MapKey {
    /// Canonicalise a value into its map key.
    ///
    /// Allocation-free for null/bool/int/real; one refcount bump for
    /// string and `big_integer`; composites render through the
    /// cycle-aware `Display` writer exactly as `key_repr` does (so a
    /// self-referential `m[m] = …` behaves identically, and the
    /// `DISPLAY_VERSION` thread-local still applies).
    pub fn of(v: &Value) -> MapKey {
        match v {
            Value::Null => MapKey::Null,
            Value::Bool(b) => MapKey::Bool(*b),
            Value::Int(i) => MapKey::Int(*i),
            Value::Real(r) => MapKey::Real(if r.is_nan() {
                f64::NAN.to_bits()
            } else {
                r.to_bits()
            }),
            Value::String(s) => MapKey::Str(Rc::clone(s)),
            Value::BigInt(b) => MapKey::BigInt(Rc::clone(b)),
            // Cells and `super` are pure storage: `Display` peels
            // them and renders the value inside, so `Cell(Int(5))`
            // keys as `5` — *not* as the integer key `i:5`. That
            // makes the rendered form the one place a composite can
            // collide with an unprefixed primitive form, and `null`
            // is the only unprefixed form there is, so fold it back
            // by hand to keep `MapKey` and `key_repr` in exact
            // agreement. (Unreachable from user code — every read
            // path unboxes a cell first — but the equivalence test
            // asserts agreement with no exceptions, so it has to
            // hold here too.)
            other => {
                let s = other.to_string();
                if s == "null" {
                    MapKey::Null
                } else {
                    MapKey::Other(Rc::new(s))
                }
            }
        }
    }
}
