//! Source backend: emit valid **official** (non-experimental) LeekScript
//! from a checked [`HirFile`](leek_hir::HirFile).
//!
//! The HIR has already desugared/erased the experimental features —
//! enums are lowered to a class of static fields, type aliases /
//! interfaces / generics never reach this layer, and prelude functions
//! are merged in. This backend walks that IR and prints LeekScript source
//! that runs identically, in two shapes:
//!
//! - [`Mode::Pretty`] — human-readable, with the original comments
//!   carried over (when [`Options::source_text`] is supplied).
//! - [`Mode::Compact`] — minified, comments dropped.
//!
//! With [`Options::optimize`] set, a small set of semantics-preserving
//! HIR→HIR passes (constant folding, dead-code elimination) run first.
//!
//! The remaining experimental-feature handling done here:
//! - **overloads**: same-named functions are renamed uniquely and every
//!   resolved call site follows the rename map;
//! - **prelude**: stdlib signatures merged into the HIR are dropped
//!   (their calls emit the bare builtin name);
//! - **enums**: emitted in their already-lowered class form.

mod comments;
mod emit;
mod optimize;
mod options;
mod prec;
mod rename;
mod writer;

pub use emit::{EmittedLeekScript, emit};
pub use options::{Mode, Options};
