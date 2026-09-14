//! Leekscript linter.
//!
//! Operates on the [`HirFile`] produced by `leek-hir` and emits
//! [`Diagnostic`]s in the `L0xxx` numeric range (the `lint` section of
//! `leek-diagnostics`' `catalog.yaml`). Findings flow through the
//! standard pipeline diagnostic stream, so they show up in `leekc`
//! output and the LSP without any extra wiring.
//!
//! Lints are grouped clippy-style ([`LintGroup`]): `correctness`,
//! `suspicious`, `complexity`, and `style` run by default; `pedantic`
//! and `nursery` are opt-in via [`LintOptions`]. All enabled lints
//! run in a **single traversal** of the HIR (see [`pass`]).
//!
//! ## Adding a lint
//!
//! 1. Pick the next free `L0xxx` code, register it in `leek-diagnostics`'
//!    catalog, and write its `explain/L0xxx.md` page.
//! 2. Implement [`LintPass`] in a new module under [`rules`], describing it
//!    with one `declare_lint!` call (see [`registry`]).
//! 3. Add the module name to the `lint_rules!` list in `rules/mod.rs`.
//!
//! That list is the only registry: [`all_passes`] and [`allow`]'s name lookup
//! are derived from it, and `tests/registry.rs` fails if it and the catalog
//! disagree, so there is nothing left to forget.
//!
//! Lints should be cheap, side-effect-free, and idempotent — the
//! linter runs every time the HIR changes.

pub mod allow;
pub mod group;
pub mod pass;
pub mod pipeline;
pub mod registry;
pub mod rules;

pub use allow::{AllowMap, collect_allows};
pub use group::{LintGroup, LintOptions};
pub use pass::{Body, BodyKind, LintCx, LintMeta, LintPass, run_passes};
pub use pipeline::{Lint, LintFindings};

use leek_diagnostics::Diagnostic;
use leek_hir::HirFile;

/// Run the default lint groups against `file` and return all
/// findings. Equivalent to [`lint_with`] with default options.
pub fn lint(file: &HirFile) -> Vec<Diagnostic> {
    lint_with(file, &LintOptions::default())
}

/// Run every lint enabled by `opts` against `file` and return all
/// findings, ordered by code then source position.
pub fn lint_with(file: &HirFile, opts: &LintOptions) -> Vec<Diagnostic> {
    let mut passes: Vec<Box<dyn LintPass>> = all_passes()
        .into_iter()
        .filter(|p| opts.enabled(p.meta().group))
        .collect();
    let mut out = Vec::new();
    pass::run_passes(file, &mut passes, opts, &mut out);
    // Stable order for consumers and tests: code, then position.
    out.sort_by(|a, b| {
        (a.code.0, a.span.start, a.span.end).cmp(&(b.code.0, b.span.start, b.span.end))
    });
    out
}

/// Every known lint pass, including the opt-in groups — one per entry in
/// [`rules::REGISTRY`], which the `lint_rules!` list generates. Version-gated
/// passes read the version off [`LintCx`](pass::LintCx), so building one
/// needs no arguments.
#[must_use]
pub fn all_passes() -> Vec<Box<dyn LintPass>> {
    rules::REGISTRY.iter().map(|r| (r.make)()).collect()
}

/// Static metadata for every known lint, in registry order.
#[must_use]
pub fn all_metas() -> Vec<&'static LintMeta> {
    rules::REGISTRY.iter().map(|r| r.meta).collect()
}

#[cfg(test)]
pub(crate) mod testing {
    //! Shared scaffolding for per-rule unit tests.

    use leek_diagnostics::Diagnostic;
    use leek_parser::ast::{AstNode, SourceFile};
    use leek_span::SourceId;
    use leek_syntax::{SyntaxNode, Version};

    use crate::pass::{LintPass, run_passes};

    /// Parse + lower `src` (V4) and run exactly one pass over it.
    pub(crate) fn lint_one(pass: impl LintPass + 'static, src: &str) -> Vec<Diagnostic> {
        lint_one_v(pass, src, Version::V4)
    }

    /// [`lint_one`] with an explicit language version.
    pub(crate) fn lint_one_v(
        pass: impl LintPass + 'static,
        src: &str,
        version: Version,
    ) -> Vec<Diagnostic> {
        let source = SourceId::new(1).unwrap();
        let parsed = leek_parser::parse(src, source, version);
        let ast = SourceFile::cast(SyntaxNode::new_root(parsed.green)).expect("source file root");
        let (hir, _) = leek_hir::lower_file(&ast, source);
        let opts = crate::LintOptions {
            version: u8::from(version),
            ..crate::LintOptions::default()
        };
        let mut passes: [Box<dyn LintPass>; 1] = [Box::new(pass)];
        let mut out = Vec::new();
        run_passes(&hir, &mut passes, &opts, &mut out);
        out.sort_by_key(|d| (d.span.start, d.span.end));
        out
    }

    /// Apply every suggestion the pass attaches to `src`, one
    /// suggestion at a time, and assert each one is a *real* fix: the
    /// rewritten source still parses, and the finding it was attached
    /// to is gone.
    ///
    /// This is the guard against span-level plausible but textually
    /// destructive edits — a suggestion that replaces a whole call
    /// expression with a bare function name passes a "the message
    /// mentions the new name" test and fails this one.
    ///
    /// `make` builds a fresh pass per run, since [`LintPass`] hooks
    /// take `&mut self` and a pass may carry state. `src` should
    /// contain exactly one finding so "the finding is gone" is
    /// unambiguous.
    pub(crate) fn assert_suggestions_fix<P, F>(make: F, src: &str)
    where
        P: LintPass + 'static,
        F: Fn() -> P,
    {
        use leek_rewrite::EditSet;

        let before = lint_one(make(), src);
        assert_eq!(
            before.len(),
            1,
            "expected exactly one finding to fix in:\n{src}\ngot {before:?}"
        );
        let diag = &before[0];
        assert!(
            !diag.suggestions.is_empty(),
            "no suggestion attached to {:?} in:\n{src}",
            diag.code
        );
        for sug in &diag.suggestions {
            let mut edits = EditSet::new(src.len());
            edits
                .push_suggestion(sug)
                .unwrap_or_else(|e| panic!("suggestion {:?} has invalid edits: {e}", sug.message));
            let fixed = edits.apply(src).expect("edits apply to their own source");

            let source = SourceId::new(1).unwrap();
            let parsed = leek_parser::parse(&fixed, source, Version::V4);
            assert!(
                parsed.diagnostics.is_empty(),
                "applying {:?} produced unparseable source:\n{fixed}\n{:?}",
                sug.message,
                parsed.diagnostics
            );

            let after = lint_one(make(), &fixed);
            assert!(
                after.iter().all(|d| d.code != diag.code),
                "applying {:?} left the finding in place:\n{fixed}\n{after:?}",
                sug.message
            );
        }
    }
}
