//! The two settings that key a tracked query.
//!
//! Neither belongs inside a query body: an optimization level and a set
//! of lint groups each produce a *different answer* for the same file, so
//! they are arguments a memo is keyed on rather than configuration a
//! query reads. Both derive `Hash` and `salsa::Update` for that reason.

/// How aggressively backend-agnostic optimization passes rewrite the IR.
///
/// Optimization is opt-in per compilation because some consumers need the
/// IR to mirror the source 1:1 — notably the Java backend's *exact* mode,
/// which reproduces the upstream reference compiler's emission shape, and
/// analysis passes (lint, complexity) that report on the code as written.
/// Codegen drivers (`miku run`, `miku build --clean`, native) request
/// [`OptLevel::O1`] to shrink the program's static op budget.
/// `Hash` (and `salsa::Update`) so it can key a tracked query:
/// `leek_db::lower_program` is memoized per optimization level, which is
/// what lets an `O1` caller read an optimized tree out of the cache
/// instead of cloning the `O0` one and optimizing it on every run.
#[derive(salsa::Update, Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum OptLevel {
    /// No optimization. The IR mirrors the source structure.
    #[default]
    O0,
    /// Backend-agnostic constant folding / dead-code elimination.
    O1,
}

impl OptLevel {
    /// Whether optimization passes should run at this level.
    #[must_use]
    pub fn optimizes(self) -> bool {
        matches!(self, OptLevel::O1)
    }
}

/// Which opt-in lint groups to run, on top of the always-on defaults.
/// Populated from CLI flags (`--pedantic`, `--nursery`) and the project
/// manifest's `[lint]` table.
///
/// `Hash` (and `salsa::Update`) so it can key a tracked query:
/// `leek_lint::lint_query` is memoized per requested group set. The
/// language version is deliberately *not* a field here — it is a
/// property of the file such a query is already keyed on, which is what
/// makes this the right key and `leek_lint::LintOptions`, which does
/// carry a version, the wrong one.
#[derive(salsa::Update, Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct LintGroups {
    /// Strictness lints — verbose-but-fine code worth tightening.
    pub pedantic: bool,
    /// Teaching lints — point newcomers at the idiomatic construct.
    pub nursery: bool,
}
