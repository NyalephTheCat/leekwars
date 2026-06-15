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
//! - Native (builtin) functions are costed by the [`native`]
//!   module: a curated asymptotic table (`sort` → `n·log n`,
//!   `arrayIntersect` → `n·m`, …) backed by the shared
//!   [`leek_builtins`] catalog so *every* native has a defined
//!   complexity, not just the hand-listed ones.
//! - User-function calls are substituted with the callee's own
//!   formula (callees-first via the call graph). Recursive calls
//!   stay [`CostExpr::Unknown`].
//! - **Class methods** are analysed too: each method body gets its
//!   own [`Complexity`] entry (keyed `Class.method`), and method
//!   *call sites* resolve through the enclosing class (`this.m()`),
//!   the receiver's type, a unique-method-name fallback, or the
//!   native catalog (`arr.sort()`).
//! - **Size variables** are resolved uniformly by
//!   [`resolve_size_var`](loop_bound::resolve_size_var): the element
//!   count of any value reachable from a stable root — a parameter
//!   (`arr`), a global (`count(g)`), `this`, or a local that aliases
//!   one of those (`var xs = bag.items`) — through any field access
//!   path (`this.cells`, `bag.items`, `this.grid.rows`). Parameter
//!   roots substitute at a call site (their field path composing onto
//!   the argument's location, so `f(thing)` calling `sumField(obj)`
//!   over `obj.items` reports `O(thing.items)`); `this` and global
//!   roots are shared/instance state and stay in the method's own
//!   big-O. Reassigned locals are excluded so a stale alias can't
//!   mislead. Local `new C()` bindings also feed method-receiver
//!   resolution (`var c = new Cat(); c.m()`).
//!
//! Returns one [`Complexity`] per user function and method, plus
//! one entry for `<main>` (the top-level statements).
//!
//! ## Pipeline integration
//!
//! The analysis is exposed as a [`pipeline`] [`Step`] producing a
//! [`ComplexityArtifact`], so tools plan it through `leek-pipeline`
//! / `leek-recipes` (and reuse salsa-cached HIR) instead of calling
//! [`analyze_file`] by hand.
//!
//! [`leek_builtins`]: ../../leek_builtins/index.html
//! [`Step`]: leek_pipeline::Step
//!
//! [`HirFile`]: leek_hir::HirFile
//! [`leek_charge`]: ../../leek-charge/index.html

pub mod analyze;
pub mod big_o;
pub mod call_graph;
pub mod cost_expr;
pub mod loop_bound;
pub mod native;
pub mod pipeline;

pub use analyze::{Complexity, ParamInfo, analyze_file, analyze_function};
pub use big_o::BigO;
pub use cost_expr::{CostExpr, SizeRoot, SizeSource, SizeVar};
pub use loop_bound::LoopBound;
pub use native::{native_big_o, native_call_cost, native_growth};
pub use pipeline::{Analyze, ComplexityArtifact};
