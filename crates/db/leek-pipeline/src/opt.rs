//! Optimization configuration: levels, per-pass toggles, and fuel.
//!
//! [`OptConfig`] is the single value threaded from a recipe ([`RecipeParams`])
//! into the HIR and MIR optimizers. It carries:
//!
//! - an [`OptLevel`] (`O0`–`O3`) that picks a default pass set,
//! - a [`Fuel`] budget bounding how many rewrites the pass loop may apply, and
//! - one boolean per pass so any pass can be toggled independently of the level.
//!
//! The optimizers run their enabled passes in a loop until a full round makes no
//! change (fixpoint) **or** fuel is exhausted. Fuel defaults to [`Fuel::Unlimited`]
//! (fixpoint decides termination); a finite budget makes optimization a
//! deterministic bisection knob — exactly the first *N* rewrites apply.
//!
//! [`RecipeParams`]: crate::RecipeParams

/// How aggressively the backend-agnostic optimization passes rewrite the IR.
///
/// Optimization is opt-in per recipe because some consumers need the IR to
/// mirror the source 1:1 — notably the Java backend's *exact* mode, which
/// reproduces the upstream reference compiler's emission shape, and analysis
/// passes (lint, complexity) that report on the code as written. Codegen
/// recipes (`miku run`, `miku build --clean`, native) request at least
/// [`OptLevel::O1`] to shrink the program's static op budget.
///
/// The levels are cumulative:
/// - **O1** — the conservative, always-safe set: constant propagation +
///   folding + dead-code elimination + inlining (+ the MIR constant-branch and
///   unreachable-block passes). This is byte-for-byte what the optimizer did
///   before levels existed.
/// - **O2** — adds HIR desugaring, algebraic/cosmetic simplification,
///   pure-function detection, and the broader intrinsic table.
/// - **O3** — adds aggressive inlining (single-use pure callees with
///   non-trivial arguments) and MIR jump-threading, with a larger default fuel.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum OptLevel {
    /// No optimization. The IR mirrors the source structure.
    #[default]
    O0,
    /// Conservative, source-version-safe constant folding / propagation /
    /// dead-code elimination / inlining.
    O1,
    /// O1 plus desugaring, algebraic simplification, purity analysis, and
    /// intrinsic recognition.
    O2,
    /// O2 plus aggressive inlining and MIR jump-threading.
    O3,
}

impl OptLevel {
    /// Whether *any* optimization pass should run at this level.
    #[must_use]
    pub fn optimizes(self) -> bool {
        !matches!(self, OptLevel::O0)
    }

    /// Map a small integer (`0..=3`) to a level; values above 3 clamp to O3.
    /// Used by the `-O<n>` CLI flag.
    #[must_use]
    pub fn from_u8(n: u8) -> Self {
        match n {
            0 => OptLevel::O0,
            1 => OptLevel::O1,
            2 => OptLevel::O2,
            _ => OptLevel::O3,
        }
    }
}

/// A budget for optimization rewrites.
///
/// The optimizer's driver loop spends fuel as passes report rewrites: it checks
/// [`Fuel::available`] between passes and deducts each pass's rewrite count via
/// [`Fuel::spend_n`]. Once the budget is exhausted the loop stops, so a finite
/// `Fuel::Limited(n)` bounds the total work and makes a run reproducible — a
/// coarse bisection knob (a pass that has already started runs to completion to
/// preserve its internal invariants, so the bound is honored at pass
/// granularity, not mid-pass). The default [`Fuel::Unlimited`] lets the loop run
/// to fixpoint.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Fuel {
    /// No cap; the pass loop stops only at fixpoint.
    #[default]
    Unlimited,
    /// At most this many rewrites across the whole optimization run.
    Limited(u64),
}

impl Fuel {
    /// Whether at least one more rewrite is permitted.
    #[must_use]
    pub fn available(&self) -> bool {
        match self {
            Fuel::Unlimited => true,
            Fuel::Limited(n) => *n > 0,
        }
    }

    /// Whether the budget is used up (no more rewrites permitted).
    #[must_use]
    pub fn is_exhausted(&self) -> bool {
        !self.available()
    }

    /// Try to consume one unit. Returns `true` if a unit was spent (the rewrite
    /// may proceed) or `false` if the budget is exhausted (skip the rewrite).
    pub fn spend(&mut self) -> bool {
        match self {
            Fuel::Unlimited => true,
            Fuel::Limited(0) => false,
            Fuel::Limited(n) => {
                *n -= 1;
                true
            }
        }
    }

    /// Deduct `n` units (saturating at zero). Used by the driver loop to charge
    /// a pass for the rewrites it reported.
    pub fn spend_n(&mut self, n: usize) {
        if let Fuel::Limited(remaining) = self {
            *remaining = remaining.saturating_sub(n as u64);
        }
    }
}

/// One individually-toggleable optimization pass. Used to wire CLI
/// `--no-<pass>` flags onto an [`OptConfig`] via [`OptConfig::set`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pass {
    /// Propagate immutable constant globals and locals into their uses.
    ConstProp,
    /// Evaluate constant sub-expressions to literals.
    ConstFold,
    /// Inline trivial single-return functions.
    Inline,
    /// Aggressive inlining (single-use pure callees with non-trivial args).
    AggressiveInline,
    /// Eliminate dead statements / constant-condition branches.
    Dce,
    /// Canonicalize compound-assignment / postfix into core HIR forms.
    Desugar,
    /// Algebraic / cosmetic identity simplification.
    Algebraic,
    /// Pure-function detection (feeds folding, DCE, inlining).
    PureFn,
    /// Intrinsic recognition (fold / identity rules for known builtins).
    Intrinsics,
    /// MIR: rewrite constant terminators to unconditional gotos.
    MirConstBranch,
    /// MIR: remove unreachable blocks.
    MirUnreachable,
    /// MIR: jump-threading / empty-block merging.
    MirJumpThread,
}

/// A fully-resolved optimization configuration: a level, a fuel budget, and a
/// per-pass enable flag. Build one with [`OptConfig::for_level`] then override
/// individual passes via [`OptConfig::without`] / [`OptConfig::set`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OptConfig {
    /// The level this config was derived from (informational; passes are the
    /// source of truth once set).
    pub level: OptLevel,
    /// Rewrite budget for the whole optimization run.
    pub fuel: Fuel,
    // --- HIR passes ---
    /// Constant propagation (globals + locals).
    pub const_prop: bool,
    /// Constant folding of sub-expressions.
    pub const_fold: bool,
    /// Trivial function inlining.
    pub inline: bool,
    /// Aggressive inlining of single-use pure callees with non-trivial args.
    pub aggressive_inline: bool,
    /// Dead-code / constant-branch elimination.
    pub dce: bool,
    /// HIR desugaring (compound-assign / postfix → core forms).
    pub desugar: bool,
    /// Algebraic / cosmetic simplification.
    pub algebraic: bool,
    /// Pure-function detection.
    pub pure_fn: bool,
    /// Intrinsic recognition (extended fold / identity table).
    pub intrinsics: bool,
    // --- MIR passes ---
    /// Rewrite constant terminators to unconditional gotos.
    pub mir_const_branch: bool,
    /// Remove unreachable blocks.
    pub mir_unreachable: bool,
    /// Jump-threading / empty-block merging.
    pub mir_jump_thread: bool,
}

impl Default for OptConfig {
    fn default() -> Self {
        Self::for_level(OptLevel::default())
    }
}

impl OptConfig {
    /// The default pass set + fuel for `level` (see [`OptLevel`] for the
    /// cumulative mapping). Fuel defaults to [`Fuel::Unlimited`] at every level
    /// — the pass loop terminates at fixpoint; a finite budget is opt-in via
    /// the `--fuel` flag.
    #[must_use]
    pub fn for_level(level: OptLevel) -> Self {
        let on = level.optimizes();
        let o2 = matches!(level, OptLevel::O2 | OptLevel::O3);
        let o3 = matches!(level, OptLevel::O3);
        OptConfig {
            level,
            fuel: Fuel::Unlimited,
            // O1 set — exactly what the optimizer did before levels existed.
            const_prop: on,
            const_fold: on,
            inline: on,
            dce: on,
            mir_const_branch: on,
            mir_unreachable: on,
            // O2 additions.
            desugar: o2,
            algebraic: o2,
            pure_fn: o2,
            intrinsics: o2,
            // O3 additions.
            aggressive_inline: o3,
            mir_jump_thread: o3,
        }
    }

    /// Whether any pass runs at this config's level.
    #[must_use]
    pub fn optimizes(&self) -> bool {
        self.level.optimizes()
    }

    /// Set this config's fuel budget (builder form).
    #[must_use]
    pub fn with_fuel(mut self, fuel: Fuel) -> Self {
        self.fuel = fuel;
        self
    }

    /// Enable or disable a single pass.
    pub fn set(&mut self, pass: Pass, on: bool) {
        match pass {
            Pass::ConstProp => self.const_prop = on,
            Pass::ConstFold => self.const_fold = on,
            Pass::Inline => self.inline = on,
            Pass::AggressiveInline => self.aggressive_inline = on,
            Pass::Dce => self.dce = on,
            Pass::Desugar => self.desugar = on,
            Pass::Algebraic => self.algebraic = on,
            Pass::PureFn => self.pure_fn = on,
            Pass::Intrinsics => self.intrinsics = on,
            Pass::MirConstBranch => self.mir_const_branch = on,
            Pass::MirUnreachable => self.mir_unreachable = on,
            Pass::MirJumpThread => self.mir_jump_thread = on,
        }
    }

    /// Disable a single pass (builder form).
    #[must_use]
    pub fn without(mut self, pass: Pass) -> Self {
        self.set(pass, false);
        self
    }

    /// Enable a single pass (builder form).
    #[must_use]
    pub fn with(mut self, pass: Pass) -> Self {
        self.set(pass, true);
        self
    }

    /// Whether any MIR pass is enabled (cheap gate for the MIR optimizer).
    #[must_use]
    pub fn mir_optimizes(&self) -> bool {
        self.mir_const_branch || self.mir_unreachable || self.mir_jump_thread
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn o0_runs_nothing() {
        let cfg = OptConfig::for_level(OptLevel::O0);
        assert!(!cfg.optimizes());
        assert!(!cfg.const_fold && !cfg.inline && !cfg.dce);
        assert!(!cfg.mir_optimizes());
    }

    #[test]
    fn o1_is_the_conservative_set() {
        let cfg = OptConfig::for_level(OptLevel::O1);
        // The pre-levels behavior: prop + fold + inline + dce + MIR const/unreachable.
        assert!(cfg.const_prop && cfg.const_fold && cfg.inline && cfg.dce);
        assert!(cfg.mir_const_branch && cfg.mir_unreachable);
        // None of the new passes run at O1.
        assert!(!cfg.desugar && !cfg.algebraic && !cfg.pure_fn && !cfg.intrinsics);
        assert!(!cfg.aggressive_inline && !cfg.mir_jump_thread);
    }

    #[test]
    fn o2_adds_new_hir_passes() {
        let cfg = OptConfig::for_level(OptLevel::O2);
        assert!(cfg.desugar && cfg.algebraic && cfg.pure_fn && cfg.intrinsics);
        assert!(!cfg.aggressive_inline && !cfg.mir_jump_thread);
    }

    #[test]
    fn o3_adds_aggressive_and_jump_threading() {
        let cfg = OptConfig::for_level(OptLevel::O3);
        assert!(cfg.aggressive_inline && cfg.mir_jump_thread);
    }

    #[test]
    fn without_disables_a_single_pass() {
        let cfg = OptConfig::for_level(OptLevel::O2).without(Pass::Inline);
        assert!(!cfg.inline);
        assert!(cfg.const_fold, "other passes untouched");
    }

    #[test]
    fn fuel_limited_spends_exactly_n() {
        let mut f = Fuel::Limited(2);
        assert!(f.spend());
        assert!(f.spend());
        assert!(!f.spend(), "third spend is denied");
        assert!(f.is_exhausted());
    }

    #[test]
    fn fuel_unlimited_never_exhausts() {
        let mut f = Fuel::Unlimited;
        for _ in 0..1000 {
            assert!(f.spend());
        }
        assert!(f.available());
    }

    #[test]
    fn from_u8_clamps() {
        assert_eq!(OptLevel::from_u8(0), OptLevel::O0);
        assert_eq!(OptLevel::from_u8(3), OptLevel::O3);
        assert_eq!(OptLevel::from_u8(9), OptLevel::O3);
    }
}
