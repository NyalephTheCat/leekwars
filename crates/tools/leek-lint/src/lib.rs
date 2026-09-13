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
//! 1. Pick the next free `L0xxx` code and register it in
//!    `leek-diagnostics`' catalog.
//! 2. Implement [`LintPass`] in a new module under [`rules`], with a
//!    `static META: LintMeta` naming its [`LintGroup`].
//! 3. Add the pass to [`all_passes`].
//!
//! Lints should be cheap, side-effect-free, and idempotent — the
//! linter runs every time the HIR changes.

pub mod allow;
pub mod group;
pub mod pass;
pub mod pipeline;
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
    let mut passes: Vec<Box<dyn LintPass>> = all_passes(opts)
        .into_iter()
        .filter(|p| opts.enabled(p.meta().group))
        .collect();
    let mut out = Vec::new();
    pass::run_passes(file, &mut passes, &mut out);
    // Stable order for consumers and tests: code, then position.
    out.sort_by(|a, b| {
        (a.code.0, a.span.start, a.span.end).cmp(&(b.code.0, b.span.start, b.span.end))
    });
    out
}

/// Every known lint pass, including the opt-in groups. Add new
/// lints here as they land. `opts` parameterizes version-gated
/// passes (e.g. [`rules::interval_loop`] only makes sense on v4+).
pub fn all_passes(opts: &LintOptions) -> Vec<Box<dyn LintPass>> {
    vec![
        Box::new(rules::unused_variable::UnusedVariable),
        Box::new(rules::shadowed_binding::ShadowedBinding),
        Box::new(rules::empty_block::EmptyBlock),
        Box::new(rules::unreachable_code::UnreachableCode),
        Box::new(rules::constant_condition::ConstantCondition),
        Box::new(rules::deprecated_feature::DeprecatedFeature),
        Box::new(rules::duplicate_branches::DuplicateBranches),
        Box::new(rules::self_comparison::SelfComparison),
        Box::new(rules::self_assignment::SelfAssignment),
        Box::new(rules::redundant_boolean::RedundantBoolean),
        Box::new(rules::double_negation::DoubleNegation),
        Box::new(rules::identical_operands::IdenticalOperands),
        Box::new(rules::assignment_in_condition::AssignmentInCondition),
        Box::new(rules::division_by_zero::DivisionByZero),
        Box::new(rules::duplicate_condition::DuplicateCondition::default()),
        Box::new(rules::unused_parameter::UnusedParameter),
        Box::new(rules::redundant_ternary::RedundantTernary),
        Box::new(rules::duplicate_include::DuplicateInclude),
        Box::new(rules::negated_comparison::NegatedComparison),
        Box::new(rules::unused_expression::UnusedExpression),
        Box::new(rules::duplicate_case::DuplicateCase),
        Box::new(rules::unnecessary_else::UnnecessaryElse),
        Box::new(rules::chained_comparison::ChainedComparison),
        // Pedantic (opt-in).
        Box::new(rules::too_many_arguments::TooManyArguments),
        Box::new(rules::long_function::LongFunction),
        Box::new(rules::deep_nesting::DeepNesting),
        Box::new(rules::collapsible_if::CollapsibleIf),
        Box::new(rules::switch_missing_default::SwitchMissingDefault),
        Box::new(rules::manual_min_max::ManualMinMax),
        Box::new(rules::approx_constant::ApproxConstant),
        Box::new(rules::needless_index_loop::NeedlessIndexLoop),
        // Nursery (opt-in).
        Box::new(rules::count_in_loop_condition::CountInLoopCondition),
        Box::new(rules::string_concat_in_loop::StringConcatInLoop::default()),
        Box::new(rules::useless_foreach_write::UselessForeachWrite),
        Box::new(rules::interval_loop::IntervalLoop {
            version: opts.version,
        }),
        Box::new(rules::shadowed_builtin::ShadowedBuiltin),
        Box::new(rules::manual_range_check::ManualRangeCheck {
            version: opts.version,
        }),
        Box::new(rules::array_literal_membership::ArrayLiteralMembership {
            version: opts.version,
        }),
        Box::new(rules::map_as_set::MapAsSet {
            version: opts.version,
        }),
    ]
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
        let mut passes: [Box<dyn LintPass>; 1] = [Box::new(pass)];
        let mut out = Vec::new();
        run_passes(&hir, &mut passes, &mut out);
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
            let fixed = edits.apply(src);

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
