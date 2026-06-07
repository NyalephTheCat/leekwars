//! Leekscript linter.
//!
//! Operates on the [`HirFile`] produced by `leek-hir` and emits
//! [`Diagnostic`]s in the `L0xxx` numeric range (see
//! `doc/diagnostics.md` §3.6). Findings flow through the standard
//! pipeline diagnostic stream, so they show up in `leekc` output
//! and the LSP without any extra wiring.
//!
//! ## Adding a rule
//!
//! 1. Pick the next free `L0xxx` code and register it in
//!    `leek-diagnostics`' catalog.
//! 2. Implement [`LintRule`] in a new module under [`rules`].
//! 3. Add the rule to [`default_rules`].
//!
//! Rules should be cheap, side-effect-free, and idempotent — the
//! linter runs every time the HIR changes.

pub mod allow;
pub mod pipeline;
pub mod rules;

pub use allow::{AllowMap, collect_allows};
pub use pipeline::{Lint, LintFindings};

use leek_diagnostics::Diagnostic;
use leek_hir::HirFile;

/// A single lint rule. Implementors walk the HIR and append any
/// findings to `out`. Conventional naming is `lowercase-with-hyphens`
/// matching the `CodeMeta::name`'s kebab form.
pub trait LintRule {
    /// Stable kebab-case identifier — also the value users put in
    /// `--allow=<name>` and (eventually) `@allow(<name>)`.
    fn name(&self) -> &'static str;

    /// The diagnostic code this rule emits. Used by the runner to
    /// honor `--allow` / `--deny` filters even before a rule runs.
    fn code(&self) -> leek_diagnostics::Code;

    /// Walk `file` and append findings to `out`.
    fn check(&self, file: &HirFile, out: &mut Vec<Diagnostic>);
}

/// Run every rule in [`default_rules`] against `file` and return all
/// findings. Order is rule-then-source-order.
pub fn lint(file: &HirFile) -> Vec<Diagnostic> {
    let mut out = Vec::new();
    for rule in default_rules() {
        rule.check(file, &mut out);
    }
    out
}

/// The default rule set. Add new rules here as they land.
pub fn default_rules() -> Vec<Box<dyn LintRule>> {
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
        Box::new(rules::duplicate_condition::DuplicateCondition),
        Box::new(rules::unused_parameter::UnusedParameter),
        Box::new(rules::redundant_ternary::RedundantTernary),
        Box::new(rules::duplicate_include::DuplicateInclude),
        Box::new(rules::negated_comparison::NegatedComparison),
        Box::new(rules::unused_expression::UnusedExpression),
        Box::new(rules::duplicate_case::DuplicateCase),
        Box::new(rules::unnecessary_else::UnnecessaryElse),
    ]
}
