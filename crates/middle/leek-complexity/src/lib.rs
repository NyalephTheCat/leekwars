//! Static complexity / big-O analysis for Leekscript.
//!
//! Builds, for each user function in an [`HirFile`], a symbolic
//! [`CostExpr`] giving the ops formula in terms of parameter
//! sizes, then reduces it to a [`BigO`] class.
//!
//! This is the static counterpart of [`leek_charge`]: the latter
//! returns scalar `u64` ops counts (the same numbers
//! `getOperations()` reports at runtime), this crate generalises
//! those into a symbolic formula in size variables.
//!
//! ## Slice 1+2 scope
//!
//! - Constant-cost functions: exact formula and `O(1)`.
//! - Loops with recognisable bounds: `for (i = 0; i < N; i++)`,
//!   `foreach (x in arr)`, and `while (i < N) { ... i += k }`.
//! - A small builtin growth table covering `sort`, `reverse`,
//!   `concat`, `arrayIntersect`, etc.
//! - User-function calls inside a function body are treated as
//!   [`CostExpr::Unknown`] — slice 4 will substitute the callee's
//!   own formula. Recursive calls always stay `Unknown`.
//!
//! Returns one [`Complexity`] per function, plus one entry for
//! `<main>` (the top-level statements).
//!
//! [`HirFile`]: leek_hir::HirFile
//! [`leek_charge`]: ../../leek-charge/index.html

pub mod analyze;
pub mod big_o;
pub mod call_graph;
pub mod cost_expr;
pub mod loop_bound;

pub use analyze::{Complexity, ParamInfo, analyze_file, analyze_function};
pub use big_o::BigO;
pub use cost_expr::{CostExpr, SizeVar};
pub use loop_bound::LoopBound;
