//! Mid-level intermediate representation for Leekscript.
//!
//! MIR is the IR consumed by the execution backends — the bytecode
//! VM interpreter and the native (Cranelift) codegen. The Java
//! emitter reads HIR directly, so MIR is intentionally lower-level:
//! a per-function control-flow graph of basic blocks, with explicit
//! temporaries and explicit short-circuit lowering for `&&`, `||`,
//! and `??`. See `doc/pipeline.md` §8.
//!
//! Two entry points:
//! - [`ir`] defines the MIR data types ([`MirProgram`],
//!   [`MirFunction`], [`BasicBlock`], etc.).
//! - [`lower`] turns a [`leek_hir::HirFile`] into a [`MirProgram`].
//!
//! This is a first-slice implementation. Classes, lambdas, and
//! intervals are recognised but lowered to a `Rvalue::Unsupported`
//! marker rather than fully modelled — they have their own design
//! pass coming.

pub mod cfg;
pub mod ir;
pub mod lower;
pub mod opt;
pub mod pipeline;
pub mod verify;

pub use ir::*;
pub use lower::lower_file;
pub use opt::{optimize_function, optimize_program};
pub use verify::{verify_function, verify_program, MirError};
