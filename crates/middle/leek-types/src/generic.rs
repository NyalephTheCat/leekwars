//! Experimental generic-type signatures.
//!
//! Leekscript user code has no generic syntax, but many builtins are
//! naturally generic — `first(Array<T>) -> T`, `push(Array<T>, T)`,
//! `arrayMap(Array<T>, T -> U) -> Array<U>`. To describe those we use
//! [`GType`], a type *pattern* that may reference generic variables.
//!
//! A [`GenericSig`] pairs parameter patterns with a return pattern.
//! [`GenericSig::instantiate`] solves the variables against the
//! concrete argument types of a call site and substitutes them into
//! the return pattern, yielding a plain [`Type`]. Generics therefore
//! live *only* inside signature definitions: they are resolved during
//! inference and never enter the shared [`Type`] enum or codegen.
//!
//! This module is the foundation a future signature parser (so authors
//! can write `push<T>(Array<T>, T) -> Array<T>`) and the full builtin
//! return-type table will build on. It is gated behind
//! [`crate::Options::experimental_generics`].

use std::collections::HashMap;

use crate::ty::{Type, unify_types};

/// A type pattern, possibly mentioning generic variables.
///
/// `Var("T")` is a hole; the structural cases mirror [`Type`] so a
/// variable can sit *inside* a composite (`Array<T>`). A
/// fully-concrete leaf is wrapped in [`GType::Concrete`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GType {
    /// A generic variable such as `T`.
    Var(String),
    /// A fully concrete type with no variables.
    Concrete(Type),
    Array(Box<GType>),
    Map(Box<GType>, Box<GType>),
    Set(Box<GType>),
    Nullable(Box<GType>),
}

impl GType {
    pub fn var(name: impl Into<String>) -> Self {
        GType::Var(name.into())
    }
    pub fn array(inner: GType) -> Self {
        GType::Array(Box::new(inner))
    }
    pub fn set(inner: GType) -> Self {
        GType::Set(Box::new(inner))
    }
    pub fn map(k: GType, v: GType) -> Self {
        GType::Map(Box::new(k), Box::new(v))
    }
}

/// Bindings solved while unifying a signature against a call's args.
type Bindings = HashMap<String, Type>;

/// A generic builtin signature: parameter patterns plus a return
/// pattern. Variable names are shared across params and the return.
#[derive(Debug, Clone)]
pub struct GenericSig {
    pub params: Vec<GType>,
    pub ret: GType,
}

impl GenericSig {
    /// Solve the signature's variables against `args` and substitute
    /// them into the return pattern. Unsolved variables widen to
    /// `Any`, and surplus/missing args are tolerated — generic
    /// inference is best-effort and never fails a call.
    pub fn instantiate(&self, args: &[Type]) -> Type {
        let mut bindings = Bindings::new();
        for (pat, arg) in self.params.iter().zip(args) {
            unify(pat, arg, &mut bindings);
        }
        substitute(&self.ret, &bindings)
    }
}

/// Unify each pattern against the corresponding concrete argument,
/// accumulating bindings into `b`. Pre-seeded bindings (e.g. a generic
/// class's type arguments) are refined, not overwritten-blindly:
/// repeated variables join. Surplus/missing args are tolerated.
pub fn solve(params: &[GType], args: &[Type], b: &mut HashMap<String, Type>) {
    for (pat, arg) in params.iter().zip(args) {
        unify(pat, arg, b);
    }
}

/// Substitute solved `bindings` into a pattern, yielding a plain type.
/// Unbound variables widen to `Any`. Public wrapper over the internal
/// recursive `substitute` so callers outside this module (generic class
/// field/method resolution) can reuse it.
pub fn apply(pat: &GType, bindings: &HashMap<String, Type>) -> Type {
    substitute(pat, bindings)
}

/// Match a pattern against a concrete type, recording variable
/// bindings. A variable seen twice is *joined* (e.g. `[1, 2.5]` makes
/// `T` = `real`) rather than conflicting. Mismatched shapes simply
/// leave variables unbound — they'll widen to `Any`.
fn unify(pat: &GType, concrete: &Type, b: &mut Bindings) {
    // A structural pattern (`Array<T>`, …) matched against a *nullable*
    // receiver sees through the wrapper — `first(arr?)` still yields the
    // element type. `Var`/`Nullable` patterns handle the wrapper
    // themselves.
    let concrete = match (pat, concrete) {
        (GType::Var(_) | GType::Nullable(_), _) => concrete,
        (_, Type::Nullable(inner)) => inner.as_ref(),
        _ => concrete,
    };
    match pat {
        GType::Var(name) => {
            let joined = match b.get(name) {
                Some(existing) => unify_types(existing, concrete),
                None => concrete.clone(),
            };
            b.insert(name.clone(), joined);
        }
        GType::Concrete(_) => {}
        GType::Array(inner) => {
            if let Type::Array(t) = concrete {
                unify(inner, t, b);
            }
        }
        GType::Map(k, v) => {
            if let Type::Map(ck, cv) = concrete {
                unify(k, ck, b);
                unify(v, cv, b);
            }
        }
        GType::Set(inner) => {
            if let Type::Set(t) = concrete {
                unify(inner, t, b);
            }
        }
        GType::Nullable(inner) => match concrete {
            Type::Nullable(t) => unify(inner, t, b),
            other => unify(inner, other, b),
        },
    }
}

/// Turn a pattern into a concrete type using solved `bindings`.
/// Unbound variables become `Any`.
fn substitute(pat: &GType, bindings: &Bindings) -> Type {
    match pat {
        GType::Var(name) => bindings.get(name).cloned().unwrap_or(Type::Any),
        GType::Concrete(t) => t.clone(),
        GType::Array(inner) => Type::Array(Box::new(substitute(inner, bindings))),
        GType::Map(k, v) => Type::Map(
            Box::new(substitute(k, bindings)),
            Box::new(substitute(v, bindings)),
        ),
        GType::Set(inner) => Type::Set(Box::new(substitute(inner, bindings))),
        GType::Nullable(inner) => Type::Nullable(Box::new(substitute(inner, bindings))),
    }
}

/// The experimental generic signature for a builtin, if one is
/// defined. Currently a small, hand-written seed set of array
/// element-returning operations; a future signature parser will
/// generate the full table.
pub fn generic_builtin(name: &str) -> Option<GenericSig> {
    // `f<T>(Array<T>) -> T` — pop an element out of an array.
    let array_to_elem = || GenericSig {
        params: vec![GType::array(GType::var("T"))],
        ret: GType::var("T"),
    };
    match name {
        "first" | "last" | "pop" | "shift" | "arrayFirst" | "arrayLast" | "arrayPop"
        | "arrayShift" | "arrayRandom" | "arrayMax" | "arrayMin" => Some(array_to_elem()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn instantiates_array_element_return() {
        let sig = generic_builtin("first").unwrap();
        let ret = sig.instantiate(&[Type::Array(Box::new(Type::Integer))]);
        assert_eq!(ret, Type::Integer);
    }

    #[test]
    fn unbound_variable_widens_to_any() {
        let sig = generic_builtin("pop").unwrap();
        // Receiver isn't an array → `T` never binds → `Any`.
        assert_eq!(sig.instantiate(&[Type::String]), Type::Any);
        // No args at all → `Any`.
        assert_eq!(sig.instantiate(&[]), Type::Any);
    }

    #[test]
    fn variable_seen_twice_is_joined() {
        // push<T>(Array<T>, T) -> Array<T>, called with Array<int> + real
        // joins T to `real`.
        let sig = GenericSig {
            params: vec![GType::array(GType::var("T")), GType::var("T")],
            ret: GType::array(GType::var("T")),
        };
        let ret = sig.instantiate(&[Type::Array(Box::new(Type::Integer)), Type::Real]);
        assert_eq!(ret, Type::Array(Box::new(Type::Real)));
    }

    #[test]
    fn nullable_receiver_unwraps_for_binding() {
        let sig = generic_builtin("first").unwrap();
        let arg = Type::Nullable(Box::new(Type::Array(Box::new(Type::String))));
        assert_eq!(sig.instantiate(&[arg]), Type::String);
    }
}
