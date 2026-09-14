//! Generic IR-traversal framework shared across the Leekscript crates.
//!
//! Two independent layers, each IR-agnostic:
//!
//! - [`tree`] — recursive traversal of *tree-shaped* IRs (a block /
//!   statement / expression trinity). HIR implements it today; the
//!   rowan CST can adopt it later. Two modes: read-only [`tree::Visit`]
//!   and in-place [`tree::VisitMut`], both of which control descent with
//!   [`tree::Flow`].
//!
//!   Two modes this crate does *not* provide, and that consumers work
//!   around today: node **replacement** (`leek-charge` swaps a `Box<Stmt>`
//!   branch for a synthesized block, so it keeps its own walker) and a
//!   **leave** hook (`leek-lint`'s shadowed-binding rule pops a scope on
//!   the way out of a block, which a pre-order-only `visit` cannot
//!   express). Adding either is a framework change, not a consumer one.
//! - [`mod@cfg`] — traversal of *control-flow graphs*. MIR implements the
//!   [`cfg::Cfg`] trait; the crate then provides the standard graph
//!   algorithms ([`cfg::postorder`], [`cfg::reverse_postorder`],
//!   [`cfg::predecessors`]) once, for any CFG.
//!
//! The framework deliberately holds **no knowledge of any concrete IR**:
//! the per-variant child enumeration (which only the IR can know) is
//! supplied by the IR through these traits, while the recursion drivers
//! and graph algorithms live here exactly once. This replaces the
//! several hand-rolled walkers that had drifted apart across the
//! workspace.

pub mod cfg;
pub mod tree;
