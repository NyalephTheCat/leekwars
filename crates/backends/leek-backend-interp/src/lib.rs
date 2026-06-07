//! MIR-walking interpreter.
//!
//! Entry point: [`run`]. The public surface (`run`, `run_with_limit`,
//! `run_with_limit_version`, [`Interpreter`]) still accepts a
//! [`HirFile`] so callers don't need to know about MIR — internally,
//! every entry lowers HIR to MIR via [`leek_mir::lower_file`] and
//! drives the interpreter from the resulting [`leek_mir::MirProgram`].
//!
//! See [`interp`] for the per-statement / per-terminator dispatch
//! and the regression note about class / lambda / foreach lowering.

mod interp;
pub mod profiler;

/// Re-export runtime value types for backward compatibility.
pub mod value {
    pub use leek_runtime::*;
}

pub use profiler::Profiler;

pub use interp::{Interpreter, Outcome, RunResult};
pub use value::Value;

use leek_hir::HirFile;
use leek_mir::lower_file;

/// Build the `DefId → builtin name` map for bodiless signature-file
/// functions. A bodiless function has no body to execute, so the
/// interpreter dispatches the call as a runtime builtin by the
/// function's name (the signature-file migration of builtins).
fn bodiless_builtins(hir: &HirFile) -> std::collections::HashMap<leek_hir::DefId, String> {
    hir.defs
        .iter()
        .enumerate()
        .filter_map(|(i, d)| match d {
            leek_hir::Def::Function(f) if f.body.is_none() => {
                let id = u32::try_from(i).expect("more than u32::MAX defs");
                Some((leek_hir::DefId(id), f.name.clone()))
            }
            _ => None,
        })
        .collect()
}

/// One-shot helper: lower the HIR file to MIR, build an interpreter,
/// run main, and return the result. Lowering errors are surfaced as
/// runtime errors so callers don't need a separate path.
pub fn run(hir: &HirFile) -> RunResult {
    let (program, errs) = lower_file(hir);
    if let Some(first) = errs.first() {
        return RunResult {
            value: Value::Null,
            error: Some(format!("MIR lowering failed: {}", first.message)),
        };
    }
    let mut interp = Interpreter::new(&program);
    interp.set_bodiless_builtins(bodiless_builtins(hir));
    interp.run()
}

/// Run with an operation budget — a runaway loop returns a
/// `TOO_MUCH_OPERATIONS` error instead of hanging.
pub fn run_with_limit(hir: &HirFile, limit: u64) -> RunResult {
    let (program, errs) = lower_file(hir);
    if let Some(first) = errs.first() {
        return RunResult {
            value: Value::Null,
            error: Some(format!("MIR lowering failed: {}", first.message)),
        };
    }
    let mut interp = Interpreter::with_op_limit(&program, limit);
    interp.set_bodiless_builtins(bodiless_builtins(hir));
    interp.run()
}

/// Run with both an op budget and a language version. The version
/// selects between v1-3 and v4 runtime semantics (mostly around
/// strict bounds-checking on array writes).
pub fn run_with_limit_version(hir: &HirFile, limit: u64, version: u8) -> RunResult {
    run_with_limit_version_strict(hir, limit, version, false)
}

/// Same as [`run_with_limit_version`] but also lets the caller
/// toggle strict mode (typed-assignment coercion).
pub fn run_with_limit_version_strict(
    hir: &HirFile,
    limit: u64,
    version: u8,
    strict: bool,
) -> RunResult {
    let (program, errs) = lower_file(hir);
    if let Some(first) = errs.first() {
        return RunResult {
            value: Value::Null,
            error: Some(format!("MIR lowering failed: {}", first.message)),
        };
    }
    let mut interp = Interpreter::with_op_limit(&program, limit);
    interp.set_version(version);
    interp.set_strict(strict);
    interp.set_bodiless_builtins(bodiless_builtins(hir));
    interp.run()
}

/// Variant that exposes the op counter alongside the result. Used
/// by the corpus runner to verify `.ops(N)` expectations.
pub fn run_with_ops_used(hir: &HirFile, limit: u64, version: u8) -> (RunResult, u64) {
    let (program, errs) = lower_file(hir);
    if let Some(first) = errs.first() {
        return (
            RunResult {
                value: Value::Null,
                error: Some(format!("MIR lowering failed: {}", first.message)),
            },
            0,
        );
    }
    let mut interp = Interpreter::with_op_limit(&program, limit);
    interp.set_version(version);
    interp.set_bodiless_builtins(bodiless_builtins(hir));
    let result = interp.run();
    let used = interp.ops_used();
    (result, used)
}
