//! The host interface stdlib builtins use to reach backend state.
//!
//! Most builtins are pure functions of their argument [`Value`]s, but a
//! few need backend-provided capabilities: the active language version,
//! the RNG (`randInt`/`randFloat`/…), and the ability to invoke a callback
//! for higher-order builtins (`arrayMap`/`arrayFilter`/`arrayReduce`/…).
//! Rather than couple the builtin catalog to a concrete interpreter, those
//! needs are abstracted behind [`BuiltinHost`]. The interpreter implements
//! it over its own state; the native backend can supply a trivial host
//! (it never reaches higher-order builtins, since lambda creation isn't
//! lowered there).

use crate::Value;

/// Abnormal control flow / error escaping a higher-order builtin's
/// callback — the runtime-level mirror of the interpreter's `Outcome`,
/// kept free of any backend type so it can live in `leek-runtime`.
#[derive(Debug, Clone)]
pub enum BuiltinFlow {
    /// `return v` bubbling out of the callback.
    Return(Value),
    /// Stray `break`.
    Break,
    /// Stray `continue`.
    Continue,
    /// A runtime error (e.g. `TOO_MUCH_OPERATIONS`).
    Error(String),
}

/// Backend capabilities the stdlib builtins draw on.
pub trait BuiltinHost {
    /// The active Leekscript language version (1–4).
    fn version(&self) -> u8;

    /// Uniform random integer in `[lo, hi)`.
    fn rng_int(&mut self, lo: i64, hi: i64) -> i64;

    /// Uniform random real in `[lo, hi)`.
    fn rng_real(&mut self, lo: f64, hi: f64) -> f64;

    /// The number of arguments a callback value expects, if known
    /// (drives the calling convention of higher-order builtins).
    fn callback_arity(&self, callee: &Value) -> Option<usize>;

    /// Per-parameter `@`-by-reference mask for a callback, if known
    /// (so `arrayMap`-style builtins can wrap by-ref arguments in cells).
    fn param_byref_mask(&self, callee: &Value) -> Option<Vec<bool>>;

    /// Invoke a callback value with `args` (for higher-order builtins).
    fn call_value(&mut self, callee: &Value, args: Vec<Value>) -> Result<Value, BuiltinFlow>;
}
