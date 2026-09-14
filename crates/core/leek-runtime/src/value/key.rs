//! Canonical map/set keys.
//!
//! Maps and sets compare keys by a *canonical form*, not by
//! [`Value`]'s loose equality, and that form splits in two along the
//! primitive/composite line — exactly as upstream's Java
//! `equals`/`hashCode` do.
//!
//! **Primitives are keyed by value.** `5`, `5.0` and `"5"` are three
//! distinct keys, every `NaN` is one key, `0.0` and `-0.0` are two,
//! and `5L` (`big_integer`) never collides with `5`. Null/bool/int/
//! real carry their payload inline and allocate nothing; string and
//! `big_integer` keys share the `Value`'s existing `Rc`, so building
//! one is a refcount bump. This half is the hot path — stress tests
//! hammer it with millions of `map[i] = …` writes — and over it the
//! equivalence with [`key_repr`](super::key_repr) still holds exactly.
//!
//! **Composites are keyed by identity.** An array, map, set, object,
//! instance or interval is one key iff it is *the same object*.
//! Upstream says so in the only place that matters, the methods
//! `LinkedHashMap`/`LinkedHashSet` call:
//! `ArrayLeekValue.java:1093-1101`, `MapLeekValue.java:556-564`,
//! `SetLeekValue.java:131-139` and `IntegerIntervalLeekValue.java:77-85`
//! all define `equals(Object o) { return o == this; }` alongside
//! `hashCode() { return this.id; }`; `ObjectLeekValue.java` and
//! `FunctionLeekValue.java` never override `Object.equals` at all,
//! which comes to the same thing. `MapLeekValue.java:25` is the
//! `LinkedHashMap` declaration that puts those methods on the lookup
//! path.
//!
//! Keying composites by their rendered text instead — which is what
//! this module used to do — got three things wrong. Two lambdas
//! collapsed into one entry, because every anonymous function
//! stringifies to `#Anonymous Function`. Two `new A()` with equal
//! fields collapsed into one entry, even though `==` on instances is
//! `Rc::ptr_eq` (see `Value::identity_eq`), so the map disagreed with
//! the language's own equality. And mutating a composite *after*
//! using it as a key stranded the entry under a key nothing could
//! spell again.
//!
//! ## Why a raw address is a sound key
//!
//! [`MapKey::Ref`] stores `Rc::as_ptr(..) as usize`. The address is
//! only ever compared and hashed, never dereferenced, so the sole
//! hazard would be a *recycled* address: the `Rc` dies, the allocator
//! hands the block to an unrelated value, and the stale key silently
//! matches the newcomer. That cannot happen, because the owning
//! collection holds a strong clone of the key `Value` for as long as
//! the index entry exists:
//!
//! - [`MapData::insert_canonical`](super::MapData::insert_canonical)
//!   pushes the key `Value` onto `entries` in the same call that
//!   inserts the slot into `index`, and
//!   [`MapData::remove_canonical`](super::MapData::remove_canonical)
//!   drops both together. `reindex` rebuilds `index` *from* `entries`,
//!   and `mapClear` empties the two at once.
//! - [`SetData::insert`](super::SetData::insert) pushes onto `items`
//!   only once `keys` has accepted the key; `remove` and `clear` take
//!   both down together.
//!
//! So every `Ref(a)` reachable from an index has a live `Rc` at `a`
//! keeping that address reserved. A key built for a *lookup* is a
//! temporary whose `Value` the caller is holding by reference, so it
//! too is alive for the duration of the probe.
//!
//! The two peeling arms hold that chain one link longer: a `Super`
//! key stores an `Rc<Value>` the receiver can never be swapped out
//! of, while a `Cell` key stores an `Rc<RefCell<Value>>` whose
//! contents *could* in principle be replaced, which would strand the
//! entry the way a mutated rendered key used to. No path puts a cell
//! in a collection — every read unboxes one first — so the case does
//! not arise; if cells ever do become storable, they need an identity
//! of their own rather than a peel.
//!
//! ## What `key_repr` is now
//!
//! `key_repr` stays the canonical *string* form, and it stays the
//! oracle for primitives: over the primitive corpus,
//! `MapKey::of(a) == MapKey::of(b)` exactly when
//! `key_repr(a) == key_repr(b)` (see `tests/map_key_equivalence.rs`).
//! For composites the two deliberately disagree, and the disagreement
//! is the point: `MapKey` is identity, `key_repr` is text. One happy
//! consequence is that a lookup no longer depends on the
//! `DISPLAY_VERSION` thread-local — a key inserted while rendering as
//! v1 is still found under v4.

use std::rc::Rc;

use super::types::{ClassId, FnId, Function, Value};

/// Canonical key of a map entry or set element.
///
/// Two values are the same key iff their `MapKey`s compare equal.
/// The variant discriminant stands in for the type prefix `key_repr`
/// writes for primitives (`i:`, `r:`, `b:`, `s:`, `I:`), so keys of
/// different kinds can never collide however their contents are
/// spelled — the string `"i:5"` is still a different key from the
/// integer `5`. Composite variants carry an identity token instead of
/// any rendering of the contents (see the module docs).
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
    /// Pointer identity of an `Array`/`Map`/`Set`/`Object`/
    /// `Instance`/`Interval` — the `Rc`'s address, never
    /// dereferenced. All six share one variant because no two of
    /// their `Rc`s can be alive at the same address at the same
    /// time, and the module docs explain why a live index entry keeps
    /// the address reserved.
    Ref(usize),
    /// Function value — see [`FnKey`].
    Fn(FnKey),
    /// A class reference (`class A {} return A`). Keyed by
    /// [`ClassId`] rather than by address because a `ClassRef` value
    /// is minted afresh at every evaluation site, while upstream has
    /// exactly one `ClassLeekValue` per class per AI: per-class
    /// identity *is* the faithful reading of `o == this` there.
    Class(ClassId),
    /// A built-in class reference (`Array`, `Map`, `String`, …).
    /// Same reasoning as [`MapKey::Class`], with the `&'static str`
    /// name standing in for the singleton.
    BuiltinClass(&'static str),
}

/// Identity of a [`Value::Function`].
///
/// Upstream `FunctionLeekValue` inherits `Object.equals`, so every
/// function value is its own key. The four shapes reach that from
/// different directions: a top-level function and a builtin each have
/// one instance per program, so the id or the name pins them down; a
/// lambda owns an `Rc<LambdaCapture>` whose address is its identity; a
/// bound method is an inline struct with no allocation of its own, so
/// it is keyed by the pair it is made of — which keeps bindings of
/// different methods, or of one method to different receivers, apart.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum FnKey {
    /// Top-level user function, by its loader-minted [`FnId`].
    User(FnId),
    /// Built-in function, by the name it dispatches on.
    Builtin(String),
    /// Lambda, by the address of its capture record. Two lambdas
    /// built from the same source expression are two keys — the bug
    /// that motivated all of this, since both render as
    /// `#Anonymous Function`.
    Lambda(usize),
    /// Method bound to a receiver, by the method's index into
    /// `MirProgram::functions` plus the receiver's own key.
    Bound {
        function_idx: usize,
        receiver: Box<MapKey>,
    },
}

impl MapKey {
    /// Canonicalise a value into its map key.
    ///
    /// Allocation-free for null/bool/int/real and for every composite
    /// (an address or an id is `Copy`); one refcount bump for string
    /// and `big_integer`; one `String` clone for a builtin function
    /// name, which is not a hot path.
    ///
    /// Deliberately has no `_` arm and calls no `Display`: a new
    /// [`Value`] variant has to be given a key here on purpose,
    /// rather than falling into a stringification that silently
    /// merges it with whatever else renders the same.
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
            Value::Array(x) => MapKey::Ref(Rc::as_ptr(x) as usize),
            Value::Map(x) => MapKey::Ref(Rc::as_ptr(x) as usize),
            Value::Set(x) => MapKey::Ref(Rc::as_ptr(x) as usize),
            Value::Object(x) => MapKey::Ref(Rc::as_ptr(x) as usize),
            Value::Instance(x) => MapKey::Ref(Rc::as_ptr(x) as usize),
            Value::Interval(x) => MapKey::Ref(Rc::as_ptr(x) as usize),
            Value::ClassRef(id, _) => MapKey::Class(*id),
            Value::BuiltinClass(name) => MapKey::BuiltinClass(name),
            Value::Function(Function::User(id)) => MapKey::Fn(FnKey::User(*id)),
            Value::Function(Function::Builtin(name)) => MapKey::Fn(FnKey::Builtin(name.clone())),
            Value::Function(Function::Lambda(capture)) => {
                MapKey::Fn(FnKey::Lambda(Rc::as_ptr(capture) as usize))
            }
            Value::Function(Function::BoundMethod {
                function_idx,
                receiver,
            }) => MapKey::Fn(FnKey::Bound {
                function_idx: *function_idx,
                receiver: Box::new(MapKey::of(receiver)),
            }),
            // Cells and `super` are pure storage — neither is a value
            // the language lets you hold, so neither gets an identity
            // of its own: they key as whatever they wrap. (Unreachable
            // from user code, since every read path unboxes a cell
            // first, but peeling them is what makes the match
            // exhaustive without a catch-all.)
            Value::Cell(inner) => MapKey::of(&inner.borrow()),
            Value::Super(sv) => MapKey::of(&sv.receiver),
        }
    }
}
