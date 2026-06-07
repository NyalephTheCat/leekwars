//! Runtime values produced by the interpreter.
//!
//! Mirrors the upstream Java runtime's tagged-value model: a single
//! `Value` enum covers all primitive and composite kinds. Arrays and
//! maps share interior mutability via `Rc<RefCell<…>>` so two
//! references to the same array see each other's writes (matching
//! Leekscript's reference-array semantics).

use std::rc::Rc;

use super::display::loose_eq_inner;
use super::types::Value;

impl Value {
    /// Peel any [`Value::Cell`] wrapper to expose the underlying
    /// value. Used at boundary points where downstream code expects
    /// "real" values — arithmetic, equality, display, builtins.
    /// Cells nest at most one level in practice but the method
    /// recurses defensively.
    pub fn unbox(&self) -> Value {
        let mut cur = self.clone();
        while let Value::Cell(c) = cur {
            cur = c.borrow().clone();
        }
        cur
    }

    pub fn is_truthy(&self) -> bool {
        match self {
            Value::Null => false,
            Value::Bool(b) => *b,
            Value::Int(i) => *i != 0,
            Value::Real(f) => *f != 0.0 && !f.is_nan(),
            Value::String(s) => !s.is_empty(),
            Value::Array(a) => !a.borrow().is_empty(),
            Value::Map(m) => !m.borrow().is_empty(),
            Value::Set(s) => !s.borrow().is_empty(),
            Value::Object(o) => !o.borrow().is_empty(),
            Value::Interval(iv) => !iv.is_empty(),
            Value::Cell(c) => c.borrow().is_truthy(),
            _ => true,
        }
    }

    pub fn type_name(&self) -> &'static str {
        match self {
            Value::Null => "null",
            Value::Bool(_) => "boolean",
            Value::Int(_) => "integer",
            Value::Real(_) => "real",
            Value::String(_) => "string",
            Value::Array(_) => "Array",
            Value::Map(_) => "Map",
            Value::Set(_) => "Set",
            Value::Object(_) => "Object",
            Value::Instance(_) => "Object",
            Value::ClassRef(_, _) | Value::BuiltinClass(_) => "Class",
            Value::Function(_) => "function",
            Value::Interval(_) => "Interval",
            Value::Super { .. } => "super",
            // Cells should be unboxed before reaching here — they
            // wrap an actual value and are an internal storage
            // marker, not a user-facing type.
            Value::Cell(_) => "any",
        }
    }

    /// Coerce to a real for arithmetic. Bools and ints widen.
    pub fn as_real(&self) -> Option<f64> {
        match self {
            Value::Bool(b) => Some(if *b { 1.0 } else { 0.0 }),
            Value::Int(i) => Some(crate::int_to_real(*i)),
            Value::Real(f) => Some(*f),
            Value::Cell(c) => c.borrow().as_real(),
            _ => None,
        }
    }

    /// Coerce to an integer where possible (truncating reals).
    pub fn as_int(&self) -> Option<i64> {
        match self {
            Value::Bool(b) => Some(i64::from(*b)),
            Value::Int(i) => Some(*i),
            Value::Real(f) => Some(crate::real_to_int(*f)),
            Value::Cell(c) => c.borrow().as_int(),
            _ => None,
        }
    }

    /// Resolve this value to a non-negative, in-bounds index into a container
    /// of length `len`. Returns `None` for non-integers, negative indices, or
    /// positions `>= len`. Centralizes the `integer → array position` boundary.
    #[must_use]
    pub fn as_index(&self, len: usize) -> Option<usize> {
        let i = self.as_int()?;
        if i < 0 {
            return None;
        }
        let idx = crate::clamp_index(i); // non-negative here, so exact
        (idx < len).then_some(idx)
    }

    /// Upstream `AI.longint()` semantics — coerces *any* value to
    /// a `long` for arithmetic. Arrays/maps/sets/objects use their
    /// size; null is 0; strings try parsing and fall back to length.
    /// Used for arithmetic ops on container values like `[1] % 2`.
    pub fn to_long(&self) -> i64 {
        match self {
            Value::Null => 0,
            Value::Bool(b) => i64::from(*b),
            Value::Int(i) => *i,
            Value::Real(f) => crate::real_to_int(*f),
            Value::String(s) => match s.parse::<i64>() {
                Ok(n) => n,
                Err(_) => crate::len_as_int(s.len()),
            },
            Value::Array(a) => crate::len_as_int(a.borrow().len()),
            Value::Map(m) => crate::len_as_int(m.borrow().len()),
            Value::Set(s) => crate::len_as_int(s.borrow().len()),
            Value::Object(o) => crate::len_as_int(o.borrow().len()),
            Value::Instance(i) => crate::len_as_int(i.borrow().fields.len()),
            Value::Function(_) | Value::ClassRef(_, _) | Value::BuiltinClass(_) => 0,
            Value::Interval(iv) => {
                let lo = iv.start.map_or(0, |s| crate::real_to_int(s.ceil()));
                let hi = iv.end.map_or(0, |e| crate::real_to_int(e.floor()));
                // Saturating so a very wide interval (e.g. `[-1e18..1e18]`)
                // can't overflow `i64` before the `.max(0)` clamp.
                hi.saturating_sub(lo).saturating_add(1).max(0)
            }
            Value::Super { receiver, .. } => receiver.to_long(),
            Value::Cell(c) => c.borrow().to_long(),
        }
    }

    /// Upstream `AI.real()` — coerces *any* value to a double for
    /// arithmetic. Like `to_long` but yields the same magnitude as
    /// a float.
    pub fn to_real(&self) -> f64 {
        match self {
            Value::Real(f) => *f,
            other => crate::int_to_real(other.to_long()),
        }
    }

    /// Loose equality comparison — `==`. Numbers compare across
    /// int/real; everything else compares structurally. Cycle-safe:
    /// once we're already comparing a pair of composites, recursing
    /// into the same pair short-circuits to `true` (standard
    /// bisimulation), so `a = [a]; b = [b]; a == b` terminates
    /// instead of overflowing the stack.
    pub fn loose_eq(&self, other: &Value) -> bool {
        let mut visited = std::collections::HashSet::new();
        loose_eq_inner(self, other, &mut visited)
    }

    /// Strict identity comparison — `===`. Same kind required,
    /// EXCEPT that Int and Real cross compare numerically
    /// (`1 === 1.0` is true upstream). Composite kinds compare
    /// by reference identity; other primitives by value.
    pub fn identity_eq(&self, other: &Value) -> bool {
        match (self, other) {
            (Value::Null, Value::Null) => true,
            (Value::Bool(a), Value::Bool(b)) => a == b,
            (Value::Int(a), Value::Int(b)) => a == b,
            (Value::Real(a), Value::Real(b)) => a == b,
            // Numeric `===` is loose across Int/Real per upstream:
            // `LeekConstants.equalsEquals` promotes both sides to
            // double when one is real.
            (Value::Int(a), Value::Real(b)) => crate::int_to_real(*a) == *b,
            (Value::Real(a), Value::Int(b)) => *a == crate::int_to_real(*b),
            (Value::String(a), Value::String(b)) => a == b,
            (Value::Array(a), Value::Array(b)) => Rc::ptr_eq(a, b),
            (Value::Map(a), Value::Map(b)) => Rc::ptr_eq(a, b),
            (Value::Set(a), Value::Set(b)) => Rc::ptr_eq(a, b),
            (Value::Object(a), Value::Object(b)) => Rc::ptr_eq(a, b),
            (Value::Instance(a), Value::Instance(b)) => Rc::ptr_eq(a, b),
            _ => false,
        }
    }

    /// Compare for ordering (`<`, `>`, etc.). Returns `None` for
    /// incomparable types.
    pub fn cmp_partial(&self, other: &Value) -> Option<std::cmp::Ordering> {
        match (self, other) {
            (Value::Int(a), Value::Int(b)) => a.partial_cmp(b),
            (Value::Real(a), Value::Real(b)) => a.partial_cmp(b),
            (Value::Int(a), Value::Real(b)) => crate::int_to_real(*a).partial_cmp(b),
            (Value::Real(a), Value::Int(b)) => a.partial_cmp(&crate::int_to_real(*b)),
            (Value::Bool(a), Value::Bool(b)) => a.partial_cmp(b),
            // Bool ↔ Int/Real cross compare: true=1, false=0.
            (Value::Bool(a), Value::Int(b)) => i64::from(*a).partial_cmp(b),
            (Value::Int(a), Value::Bool(b)) => a.partial_cmp(&i64::from(*b)),
            (Value::Bool(a), Value::Real(b)) => (if *a { 1.0_f64 } else { 0.0 }).partial_cmp(b),
            (Value::Real(a), Value::Bool(b)) => a.partial_cmp(&(if *b { 1.0_f64 } else { 0.0 })),
            (Value::String(a), Value::String(b)) => a.partial_cmp(b),
            // Null compares as 0 against numbers — `null < 0.8` is
            // `true` upstream because the runtime coerces both
            // sides to numbers (an uninitialised `real? a` reads
            // as 0 for ordering purposes).
            (Value::Null, Value::Int(b)) => 0i64.partial_cmp(b),
            (Value::Null, Value::Real(b)) => 0.0_f64.partial_cmp(b),
            (Value::Int(a), Value::Null) => a.partial_cmp(&0),
            (Value::Real(a), Value::Null) => a.partial_cmp(&0.0),
            (Value::Null, Value::Null) => Some(std::cmp::Ordering::Equal),
            _ => None,
        }
    }
}
