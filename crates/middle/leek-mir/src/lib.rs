//! Mid-level intermediate representation for Leekscript.
//!
//! MIR is the IR consumed by the native (Cranelift) backend. The Java
//! and LeekScript emitters read HIR directly, so MIR is intentionally
//! lower-level: a per-function control-flow graph of basic blocks,
//! with explicit temporaries and explicit short-circuit lowering for
//! `&&`, `||`, and `??`.
//!
//! Two entry points:
//! - [`ir`] defines the MIR data types ([`MirProgram`],
//!   [`MirFunction`], [`BasicBlock`], etc.).
//! - [`lower`] turns a [`leek_hir::HirFile`] into a [`MirProgram`] —
//!   [`lower_and_optimize`] is the entry point drivers want: lower,
//!   optimize at the requested level, and report malformed IR.
//!
//! Classes, lambdas (with captures), intervals and `super` dispatch
//! are fully lowered. `Rvalue::Unsupported` survives only as a marker
//! for shapes that are *errors* rather than gaps — an unbound local,
//! `super` outside a method or in a static, a `this(...)` call — each
//! paired with a diagnostic.

pub mod cfg;
pub mod ir;
pub mod lower;
pub mod opt;
pub mod query;
pub mod verify;

pub use ir::*;
pub use lower::{lower_and_optimize, lower_file};
pub use opt::{optimize_function, optimize_program};
pub use verify::{MirError, verify_function, verify_program};
