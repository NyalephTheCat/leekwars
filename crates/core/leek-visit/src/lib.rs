//! Generic IR-traversal framework shared across the Leekscript crates.
//!
//! Two independent layers, each IR-agnostic:
//!
//! - [`tree`] — recursive traversal of *tree-shaped* IRs (a block /
//!   statement / expression trinity). HIR implements it today; the
//!   rowan CST can adopt it later. Four modes: read-only [`tree::Visit`],
//!   in-place [`tree::VisitMut`], node-replacing [`tree::Fold`], and the
//!   control-flow-aware [`tree::FlowVisit`].
//! - [`cfg`] — traversal of *control-flow graphs*. MIR implements the
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
