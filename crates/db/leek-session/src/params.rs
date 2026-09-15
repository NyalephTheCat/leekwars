//! What a driver asks the compiler for, and under which settings.

use leek_diagnostics::Severity;

pub use leek_query::{LintGroups, OptLevel, TimingSink};

/// What a tool wants out of the compiler front/middle-end.
///
/// A [`Session`](crate::Session) maps it onto a
/// [`Stage`](leek_db::queries::Stage) — how far the diagnostic stream
/// reaches — and onto which of [`Compilation`](crate::Compilation)'s
/// accessors can answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    /// `// @version` pragmas + token stream (`--emit tokens`).
    Tokens,
    /// Green CST (parse only).
    Parsed,
    /// Name resolution table + diagnostics.
    Resolved,
    /// Type table + diagnostics.
    TypeChecked,
    /// Lowered HIR.
    Hir,
    /// HIR + lint findings (check / lint drivers).
    Linted,
    /// MIR.
    Mir,
    /// HIR + per-function / per-method complexity report
    /// (`miku analyze`, `miku doc`).
    Complexity,
}

/// The settings one compilation runs under.
///
/// Three knobs, each of which changes what a compilation *reports* or
/// *produces* rather than how it is scheduled — there is nothing left to
/// schedule.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompileParams {
    /// When set, a parse error at or above this severity stops the
    /// compilation there: the stream reports the syntax errors and not
    /// what the later passes made of the wreckage.
    ///
    /// `None` for best-effort tooling. The editor is almost always
    /// looking at code that is mid-edit (a trailing `c.`, an unclosed
    /// brace), so a parse error must not black out hover, completion and
    /// go-to-definition.
    pub stop_on_diagnostics: Option<Severity>,
    /// How aggressively to optimize the IR. Defaults to [`OptLevel::O0`]
    /// so analysis and diagnostics see the code as written; codegen
    /// drivers raise it with [`with_opt`](Self::with_opt).
    pub opt: OptLevel,
    /// Opt-in lint groups, on top of the always-on ones. Only read when
    /// the target is [`Target::Linted`].
    pub lints: LintGroups,
}

impl Default for CompileParams {
    fn default() -> Self {
        Self {
            stop_on_diagnostics: Some(Severity::Error),
            opt: OptLevel::default(),
            lints: LintGroups::default(),
        }
    }
}

impl CompileParams {
    /// Best-effort: no stop-on-error, so every pass answers even after an
    /// earlier one reported.
    #[must_use]
    pub fn permissive() -> Self {
        Self {
            stop_on_diagnostics: None,
            ..Self::default()
        }
    }

    /// LSP defaults, which are [`permissive`](Self::permissive) — see
    /// `stop_on_diagnostics`.
    #[must_use]
    pub fn lsp() -> Self {
        Self::permissive()
    }

    /// Request an [`OptLevel`] (codegen drivers use [`OptLevel::O1`]).
    #[must_use]
    pub fn with_opt(mut self, opt: OptLevel) -> Self {
        self.opt = opt;
        self
    }

    /// Enable opt-in lint groups.
    #[must_use]
    pub fn with_lints(mut self, lints: LintGroups) -> Self {
        self.lints = lints;
        self
    }
}

/// LSP default parameters.
#[must_use]
pub fn lsp_params() -> CompileParams {
    CompileParams::lsp()
}

/// One-shot driver parameters (stop on parse errors).
#[must_use]
pub fn driver_params() -> CompileParams {
    CompileParams::default()
}
