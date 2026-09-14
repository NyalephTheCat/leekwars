//! Runtime values and supporting data structures.
//!
//! Shared by every consumer that needs LeekScript's runtime semantics: the
//! native (Cranelift) backend links these as C-ABI symbols from JIT-compiled
//! and AOT-compiled code, `leek-aot-runtime` re-exports them into a standalone
//! binary, and the test/bench tooling calls them directly.

mod builtin;
mod builtins;
mod convert;
mod eval;
mod host;
mod rng;
mod value;

pub use builtin::*;
pub use builtins::{
    builtin_arity, builtin_cost, builtin_op_cost, call_builtin, deep_clone_for_v1,
    is_known_builtin, lookup_constant, needs_at_least_one_arg, take_pending_promotion,
};
pub use convert::{clamp_index, int_to_real, len_as_int, real_to_int};
pub use eval::*;
pub use host::{BuiltinError, BuiltinHost, BuiltinResult};
pub use rng::Rng;
pub use value::*;
