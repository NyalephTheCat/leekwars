//! Java backend for Leekscript.
//!
//! Reads typed/resolved HIR and produces a single `AI_<id>.java`
//! file plus a `.lines` sidecar mapping Java lines to source lines.
//!
//! Two emission modes — see [`options::Mode`]:
//! - [`Mode::Exact`] mirrors the upstream Java reference's emission
//!   shape (`u_x` / `g_x` mangling, per-statement `ai.ops(1)` ticks,
//!   runtime-comparison switch lowering, `add(...)`/`sub(...)` etc.
//!   helpers for non-primitive arithmetic). Byte-for-byte parity is
//!   pending the golden-output capture pipeline.
//! - [`Mode::Clean`] is the readable / optimized variant. Folds per-
//!   block static op cost into a single `charge(n)` (via the
//!   `leek-charge` pass), drops unreachable code after `return`,
//!   uses native Java `switch` for constant cases, and drops the
//!   `u_`/`f_` prefix when the bare name doesn't collide with a
//!   Java keyword.
//!
//! Entry point: [`emit`]. Convenience wrappers [`emit_exact`] and
//! [`emit_clean`] preset the option struct.
//!
//! See `doc/java-backend.md` for the full spec.

mod builtins;
mod emit;
mod mangle;
mod options;
mod writer;

pub use emit::{EmittedJava, emit};
pub use options::{Mode, Options};

use leek_hir::HirFile;
use leek_syntax::Version;

/// Convenience: exact-mode emission with sensible defaults.
pub fn emit_exact(hir: &HirFile, version: Version, ai_id: u64) -> EmittedJava {
    let opts = Options::exact(version, ai_id);
    emit(hir, &opts)
}

/// Convenience: clean-mode emission with sensible defaults.
pub fn emit_clean(hir: &HirFile, version: Version, ai_id: u64) -> EmittedJava {
    let opts = Options::clean(version, ai_id);
    emit(hir, &opts)
}
