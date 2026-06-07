//! Runtime values and supporting data structures.
//!
//! Shared between the MIR interpreter and future native backends.

mod builtin;
mod builtins;
mod convert;
mod eval;
mod host;
mod rng;
mod value;

pub use builtin::*;
pub use convert::{clamp_index, int_to_real, len_as_int, real_to_int};
pub use builtins::{
    builtin_arity, builtin_op_cost, call_builtin, deep_clone_for_v1, is_known_builtin,
    lookup_constant, needs_at_least_one_arg, take_pending_promotion,
};
pub use eval::*;
pub use host::{BuiltinFlow, BuiltinHost};
pub use rng::Rng;
pub use value::*;
