//! Per-function CFG lowering (`FnLowerer`).

mod cfg;
mod expr;
mod stmt;

pub(super) use super::util;
pub(super) use super::{FnLowerer, LoopCtx, PendingLambda};
