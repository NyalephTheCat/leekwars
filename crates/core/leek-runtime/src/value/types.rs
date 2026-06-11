//! Runtime values produced by the interpreter.
//!
//! Mirrors the upstream Java runtime's tagged-value model: a single
//! `Value` enum covers all primitive and composite kinds. Arrays and
//! maps share interior mutability via `Rc<RefCell<…>>` so two
//! references to the same array see each other's writes (matching
//! Leekscript's reference-array semantics).

use std::cell::RefCell;
use std::rc::Rc;

use leek_hir::DefId;

use super::display::key_repr;

/// All runtime value shapes. Cheap to clone — composites are
/// shared via `Rc`.
#[derive(Debug, Clone)]
pub enum Value {
    Null,
    Bool(bool),
    Int(i64),
    Real(f64),
    /// Arbitrary-precision integer (`big_integer`, `2L` literals).
    /// Immutable — shared via `Rc` like upstream's immutable
    /// `BigIntegerValue` wrapper.
    BigInt(Rc<num_bigint::BigInt>),
    String(Rc<String>),
    /// Mutable ordered list. Two `Value::Array` aliases sharing the
    /// same Rc see each other's writes.
    Array(Rc<RefCell<Vec<Value>>>),
    /// Mutable insertion-ordered key/value collection. Internally
    /// stores entries in a `Vec` for stable iteration order plus a
    /// `HashMap` from canonical key string to position so
    /// `get`/`set` are O(1) amortized.
    Map(Rc<RefCell<MapData>>),
    /// Mutable set with insertion order. Backed by a Vec for stable
    /// iteration plus a HashSet on the canonical key string so
    /// `contains`/`insert` are O(1) amortised.
    Set(Rc<RefCell<SetData>>),
    /// Object literal — string-keyed records (insertion order).
    Object(Rc<RefCell<ObjectData>>),
    /// Class instance. `class` is the class's `DefId`; fields are
    /// stored inline so two refs share state.
    Instance(Rc<RefCell<Instance>>),
    /// Class-name reference — `class A {} return A` returns this.
    /// Carries both the `DefId` (for method/field lookup) and the
    /// source name (so `Display` can produce `<class A>` without
    /// reaching back into the program table).
    ClassRef(DefId, Rc<String>),
    /// Reference to one of the built-in classes (`Array`, `Map`,
    /// `Set`, `Object`, `Integer`, `Real`, `String`, …). Stringified
    /// as `<class Name>`. Callable as a constructor:
    /// `Array(1, 2, 3)` returns `[1, 2, 3]`.
    BuiltinClass(&'static str),
    /// `[a..b]` interval literal. Bounds are inclusive both ends in
    /// the canonical Leekscript form; `]a..b[` / `[a..b[` open-end
    /// variants store the open/closed bit.
    Interval(Rc<IntervalValue>),
    /// First-class function value. Either a user-defined function
    /// (referenced by `DefId`), a captured lambda body, or a bound
    /// method.
    Function(Function),
    /// `super` expression inside a method body. Behaves like the
    /// stored receiver for everything except method dispatch,
    /// which statically resolves against `parent_class`. Built by
    /// [`leek_mir::Rvalue::MakeSuper`] in the lowering pass. Boxed —
    /// it's a rare, large variant (an inline `String` + `Rc`), so
    /// keeping it out of line shrinks every `Value` (32 → 24 bytes).
    Super(Box<SuperValue>),
    /// Shared mutable storage — wraps a local that's captured by
    /// one or more lambdas so writes propagate between the outer
    /// scope and every closure that holds a reference. Reads peek
    /// through; writes go through the cell. Constructed by the
    /// interpreter at frame init for locals the lowerer marked
    /// `is_shared`. Never produced by user code directly.
    Cell(Rc<RefCell<Value>>),
}

/// Payload of [`Value::Super`] — kept behind a `Box` so it doesn't bloat the
/// `Value` enum (see the `Super` variant).
#[derive(Debug, Clone)]
pub struct SuperValue {
    pub parent_class: String,
    pub receiver: Rc<Value>,
}

#[derive(Debug, Clone)]
pub struct Instance {
    pub class: DefId,
    /// Class name kept alongside the `DefId` so `Display` can
    /// render `ClassName {…}` without a back-reference to the HIR.
    pub class_name: String,
    pub fields: ObjectData,
}

/// Closed or half-open interval value. `start.is_none()` means
/// unbounded left; `end.is_none()` means unbounded right.
#[derive(Debug, Clone)]
pub struct IntervalValue {
    pub start: Option<f64>,
    pub end: Option<f64>,
    pub start_inclusive: bool,
    pub end_inclusive: bool,
    /// True when both ends were integer-typed at creation —
    /// affects iteration step (1 for integer, 1.0 for real).
    pub integer_typed: bool,
    /// Per-endpoint integer flags — controls per-bound formatting
    /// (`5` vs `5.0`). Independent of `integer_typed` (which gates
    /// iteration step).
    pub start_is_int: bool,
    pub end_is_int: bool,
    /// `true` when the endpoint's source-level form was an
    /// `Infinity` builtin reference — display widens the OTHER
    /// bound to real even though the value here is `±inf`. (The
    /// `∞` symbol leaves this flag `false`.)
    pub start_forces_real: bool,
    pub end_forces_real: bool,
}

impl IntervalValue {
    // Interval endpoints are compared exactly to detect a degenerate `a..a`.
    #[allow(clippy::float_cmp)]
    pub fn is_empty(&self) -> bool {
        match (self.start, self.end) {
            (Some(a), Some(b)) => {
                if a > b {
                    return true;
                }
                if a == b {
                    return !(self.start_inclusive && self.end_inclusive);
                }
                false
            }
            // `[..]` (both ends *inclusive* but with no bounds)
            // is an empty interval in Leekscript — no concrete
            // values. `]..[` (both ends *exclusive* with no
            // bounds) covers the whole number line and is *not*
            // empty. The inclusiveness flags carry that bit.
            (None, None) => self.start_inclusive && self.end_inclusive,
            _ => false,
        }
    }
}

/// Insertion-ordered map with O(1) key lookup. `entries` is the
/// authoritative storage; `index` maps canonical key strings to
/// positions in `entries` and is kept in sync with every write.
#[derive(Debug, Default, Clone)]
pub struct MapData {
    pub entries: Vec<(Value, Value)>,
    pub index: std::collections::HashMap<String, usize>,
}

impl MapData {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn from_pairs(pairs: Vec<(Value, Value)>) -> Self {
        let mut m = Self::new();
        for (k, v) in pairs {
            m.insert(k, v);
        }
        m
    }

    /// Insert when the caller already has the canonical key. The
    /// no-canonical path (e.g. for literals) is [`insert`].
    pub fn insert_canonical(&mut self, canonical: String, key: Value, value: Value) {
        if let Some(&i) = self.index.get(&canonical) {
            self.entries[i].1 = value;
        } else {
            let i = self.entries.len();
            self.entries.push((key, value));
            self.index.insert(canonical, i);
        }
    }

    pub fn insert(&mut self, key: Value, value: Value) {
        let canonical = key_repr(&key);
        self.insert_canonical(canonical, key, value);
    }

    pub fn get(&self, key: &Value) -> Option<&Value> {
        let canonical = key_repr(key);
        self.index.get(&canonical).map(|&i| &self.entries[i].1)
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// Insertion-ordered string→Value record used by `Value::Object` and
/// `Instance.fields`. Same shape as [`MapData`] (Vec for stable
/// iteration order + HashMap for O(1) lookup) — repeated field
/// writes on a single object are O(1) amortised instead of O(N).
#[derive(Debug, Default, Clone)]
pub struct ObjectData {
    pub fields: Vec<(String, Value)>,
    pub index: std::collections::HashMap<String, usize>,
}

impl ObjectData {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_capacity(n: usize) -> Self {
        Self {
            fields: Vec::with_capacity(n),
            index: std::collections::HashMap::with_capacity(n),
        }
    }

    /// Set a field. Preserves insertion order on first set; later
    /// writes update in place.
    pub fn set(&mut self, name: &str, value: Value) {
        if let Some(&i) = self.index.get(name) {
            self.fields[i].1 = value;
        } else {
            let i = self.fields.len();
            self.fields.push((name.to_string(), value));
            self.index.insert(name.to_string(), i);
        }
    }

    pub fn get(&self, name: &str) -> Option<&Value> {
        self.index.get(name).map(|&i| &self.fields[i].1)
    }

    /// Read a field by its dense slot index (its position in `fields`).
    /// The native backend resolves a known class's field name to a stable
    /// slot at compile time (`MirClass::field_layout`) and reads through
    /// this — skipping the `index` hash. Sound because a natively-built
    /// instance lays its fields out in `field_layout` slot order.
    pub fn get_slot(&self, slot: usize) -> Option<&Value> {
        self.fields.get(slot).map(|(_, v)| v)
    }

    /// Write a field by its dense slot index. Returns `false` if the slot
    /// is out of range (the caller then falls back to the name path).
    pub fn set_slot(&mut self, slot: usize, value: Value) -> bool {
        match self.fields.get_mut(slot) {
            Some(entry) => {
                entry.1 = value;
                true
            }
            None => false,
        }
    }

    pub fn contains_key(&self, name: &str) -> bool {
        self.index.contains_key(name)
    }

    pub fn len(&self) -> usize {
        self.fields.len()
    }

    pub fn is_empty(&self) -> bool {
        self.fields.is_empty()
    }

    pub fn iter(&self) -> std::slice::Iter<'_, (String, Value)> {
        self.fields.iter()
    }
}

impl<'a> IntoIterator for &'a ObjectData {
    type Item = &'a (String, Value);
    type IntoIter = std::slice::Iter<'a, (String, Value)>;
    fn into_iter(self) -> Self::IntoIter {
        self.fields.iter()
    }
}

impl FromIterator<(String, Value)> for ObjectData {
    fn from_iter<I: IntoIterator<Item = (String, Value)>>(iter: I) -> Self {
        let iter = iter.into_iter();
        let mut out = Self::with_capacity(iter.size_hint().0);
        for (k, v) in iter {
            out.set(&k, v);
        }
        out
    }
}

/// Insertion-ordered set with O(1) `contains`/`insert`. Mirrors
/// upstream's `SetLeekValue` (LinkedHashSet) — first occurrence
/// wins, iteration order matches insertion order.
#[derive(Debug, Default, Clone)]
pub struct SetData {
    pub items: Vec<Value>,
    pub keys: std::collections::HashSet<String>,
}

impl SetData {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_capacity(n: usize) -> Self {
        Self {
            items: Vec::with_capacity(n),
            keys: std::collections::HashSet::with_capacity(n),
        }
    }

    /// Insert if not already present. Returns true if a new element
    /// was added.
    pub fn insert(&mut self, v: Value) -> bool {
        let k = key_repr(&v);
        if self.keys.insert(k) {
            self.items.push(v);
            true
        } else {
            false
        }
    }

    pub fn contains(&self, v: &Value) -> bool {
        self.keys.contains(&key_repr(v))
    }

    pub fn remove(&mut self, v: &Value) -> bool {
        let k = key_repr(v);
        if !self.keys.remove(&k) {
            return false;
        }
        // Rare branch — only on `setRemove` etc. We keep iteration
        // order, so a linear scan is fine here.
        if let Some(i) = self.items.iter().position(|x| key_repr(x) == k) {
            self.items.remove(i);
        }
        true
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    pub fn iter(&self) -> std::slice::Iter<'_, Value> {
        self.items.iter()
    }

    pub fn clear(&mut self) {
        self.items.clear();
        self.keys.clear();
    }
}

impl<'a> IntoIterator for &'a SetData {
    type Item = &'a Value;
    type IntoIter = std::slice::Iter<'a, Value>;
    fn into_iter(self) -> Self::IntoIter {
        self.items.iter()
    }
}

impl FromIterator<Value> for SetData {
    fn from_iter<I: IntoIterator<Item = Value>>(iter: I) -> Self {
        let iter = iter.into_iter();
        let mut out = Self::with_capacity(iter.size_hint().0);
        for v in iter {
            out.insert(v);
        }
        out
    }
}

/// First-class function values. The interpreter dispatches each
/// case to a different path.
#[derive(Debug, Clone)]
pub enum Function {
    /// Top-level user function. Body lookup is by `DefId` into
    /// `HirFile::defs`.
    User(DefId),
    /// Lambda capture. We snapshot the captured locals at lambda
    /// creation time so subsequent edits don't affect the closure
    /// (matches Leekscript value-capture semantics).
    Lambda(Rc<LambdaCapture>),
    /// Method bound to a specific receiver. `function_idx` indexes
    /// into [`leek_mir::MirProgram::functions`]; the interpreter
    /// prepends `receiver` to the caller's arg list when invoking,
    /// matching how methods are lowered (first param is the
    /// synthetic `this`).
    BoundMethod {
        function_idx: usize,
        receiver: Box<Value>,
    },
    /// Builtin function name — dispatched by string.
    Builtin(String),
}

#[derive(Debug)]
pub struct LambdaCapture {
    /// Index into [`leek_mir::MirProgram::functions`] for the
    /// lambda's body. Its `params` list begins with the captured
    /// slots, in the same order as `captured` below.
    pub function_idx: usize,
    /// Captured values pre-bound at lambda-construction time. The
    /// interpreter prepends these to the user arguments before
    /// entering the lambda's frame.
    pub captured: std::cell::RefCell<Vec<Value>>,
}
