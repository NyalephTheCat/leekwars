//! The host interface stdlib builtins use to reach backend state.
//!
//! Most builtins are pure functions of their argument [`Value`]s, but a
//! few need backend-provided capabilities: the active language version,
//! the RNG (`randInt`/`randFloat`/…), and the ability to invoke a callback
//! for higher-order builtins (`arrayMap`/`arrayFilter`/`arrayReduce`/…).
//! Rather than couple the builtin catalog to a concrete backend, those
//! needs are abstracted behind [`BuiltinHost`]. The native backend
//! implements it over its per-run thread-local state: `call_value` enters a
//! JIT-compiled lambda, and reports the run's recorded runtime error as a
//! [`BuiltinError`] so a higher-order builtin stops at the first fault
//! instead of calling back for every remaining element.

use crate::Value;

/// A runtime fault escaping a builtin — in practice one raised inside a
/// higher-order builtin's callback (`TOO_MUCH_OPERATIONS`, `STACKOVERFLOW`,
/// …), which is why the channel exists at all.
///
/// `code` is a bare *fight* error key, not a sentence: the generator logs it
/// verbatim, so it stays a `String` rather than becoming an enum the two
/// sides would have to agree on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuiltinError {
    pub code: String,
}

impl BuiltinError {
    /// A fault reported under the fight error key `code`.
    pub fn new(code: impl Into<String>) -> Self {
        Self { code: code.into() }
    }
}

impl std::fmt::Display for BuiltinError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.code)
    }
}

impl std::error::Error for BuiltinError {}

/// What a builtin returns: its value, or the fault that stopped it. Defaults
/// to `Value` because that is what `call_builtin` and the higher-order
/// helpers produce; the `dispatch_*` tables use `BuiltinResult<Option<Value>>`
/// for "not my name".
pub type BuiltinResult<T = Value> = Result<T, BuiltinError>;

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
    fn call_value(&mut self, callee: &Value, args: Vec<Value>) -> BuiltinResult;
}
