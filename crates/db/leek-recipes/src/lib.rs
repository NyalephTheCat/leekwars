//! Shared pipeline recipes for `leekc`, `miku`, and the LSP.
//!
//! Call [`pipeline`] with a [`Target`] to get a fully planned [`Pipeline`],
//! then run it with [`Pipeline::run`] or [`Pipeline::run_memoized`].

use std::any::TypeId;

use leek_fmt::FormatOptions;
use leek_fmt::pipeline::{Fmt, FormattedArtifact};
use leek_hir::pipeline::{HirArtifact, LowerHir};
use leek_lexer::pipeline::TokensArtifact;
use leek_lint::pipeline::LintFindings;
use leek_mir::pipeline::MirArtifact;
use leek_parser::pipeline::{AstArtifact, GreenTreeArtifact};
use leek_pipeline::{RecipeStep, plan_for};

pub use leek_pipeline::{OptLevel, Pipeline, RecipeError, RecipeParams, RecipePlan, TimingSink};
use leek_resolver::pipeline::{IncludeGraphArtifact, ResolveArtifact};
use leek_types::pipeline::TypeCheckArtifact;

/// What a tool wants out of the compiler front/middle-end.
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
}

/// Plan/build a pipeline for `target` using recipe metadata + `params`.
pub fn pipeline(target: Target, params: &RecipeParams) -> Result<Pipeline, RecipeError> {
    plan(target, params).map(leek_pipeline::RecipePlan::build)
}

/// Plan a pipeline without building it (e.g. to append custom steps).
pub fn plan(
    target: Target,
    params: &RecipeParams,
) -> Result<leek_pipeline::RecipePlan, RecipeError> {
    match target {
        Target::Tokens => plan_for::<TokensArtifact>(params),
        Target::Parsed => plan_for::<GreenTreeArtifact>(params),
        Target::Resolved => plan_for::<ResolveArtifact>(params),
        Target::TypeChecked => plan_for::<TypeCheckArtifact>(params),
        Target::Hir => plan_for::<HirArtifact>(params),
        Target::Linted => plan_for::<LintFindings>(params),
        Target::Mir => plan_for::<MirArtifact>(params),
    }
}

/// Like [`pipeline`], but records per-step durations into `sink`.
pub fn pipeline_timed(
    target: Target,
    params: &RecipeParams,
    sink: &TimingSink,
) -> Result<Pipeline, RecipeError> {
    Ok(plan(target, params)?.build_timed(sink))
}

/// Parse, then lower HIR without resolve/types (single-file path).
pub fn pipeline_hir_from_parse(params: &RecipeParams) -> Result<Pipeline, RecipeError> {
    let mut plan = leek_pipeline::RecipePlan::new();
    plan.need::<AstArtifact>(params)?;
    plan.push_step(LowerHir::build(params), &[TypeId::of::<HirArtifact>()]);
    Ok(plan.build())
}

/// Parse, resolve includes with `includes`, then lower HIR (multi-file path).
pub fn pipeline_hir_with_includes(
    includes: Box<dyn leek_pipeline::Step>,
    params: &RecipeParams,
) -> Result<Pipeline, RecipeError> {
    let mut plan = leek_pipeline::RecipePlan::new();
    plan.need::<AstArtifact>(params)?;
    plan.push_step(includes, &[TypeId::of::<IncludeGraphArtifact>()]);
    plan.push_step(LowerHir::build(params), &[TypeId::of::<HirArtifact>()]);
    Ok(plan.build())
}

/// Formatting is separate because [`Fmt`] carries per-project options.
pub fn pipeline_formatted(
    opts: FormatOptions,
    params: &RecipeParams,
) -> Result<Pipeline, RecipeError> {
    let mut plan = plan_for::<GreenTreeArtifact>(params)?;
    plan.push_step(
        Box::new(Fmt::with_options(opts)),
        &[TypeId::of::<FormattedArtifact>()],
    );
    Ok(plan.build())
}

/// LSP default recipe parameters.
pub fn lsp_params() -> RecipeParams {
    RecipeParams::lsp()
}

/// One-shot driver parameters (stop on parse/type errors).
pub fn driver_params() -> RecipeParams {
    RecipeParams::default()
}

/// Register a host-environment library's functions with the resolver so the
/// whole pipeline (diagnostics, completion, type-checking) recognizes them
/// as defined functions instead of flagging them as undefined. Shared by
/// `leekc`, `miku`, and the LSP — load a catalog with
/// [`leek_environment::load_all`], then call this once.
///
/// Registration is process-global (the resolver's dynamic-builtin table),
/// matching how the resolver already supports importable builtin libraries.
pub fn register_environment(catalog: &dyn leek_environment::EnvironmentCatalog) {
    for (name, b) in catalog.entries() {
        leek_resolver::builtins::register_builtin_function(
            name,
            b.min_arity,
            b.max_arity,
            1, // available from v1 — host libraries aren't version-gated here
        );
    }
    // Constants (e.g. the fight constants `CELL_EMPTY`, `WEAPON_PISTOL`):
    // register so they're recognized (no "undefined") and offered in
    // completion.
    for (name, _ty) in catalog.constants() {
        leek_resolver::builtins::register_builtin_constant(name);
    }
}

/// Load library specs (built-in names like `"leekwars"`, or file paths) and
/// register them with the resolver in one step, returning the composed
/// catalog for a backend to emit through.
///
/// The built-in `leekwars` library is a *typed signature header*
/// (`leek_prelude::LEEKWARS_SRC`) carrying `@java-dispatch:` directives,
/// not a TSV catalog: its functions are registered by parsing the header,
/// its constants come from [`leek_environment`], and the header is
/// activated for HIR merge so the Java backend dispatches through the
/// directives (fully-qualified, no import needed). It contributes nothing
/// to the returned catalog; file-based `FileCatalog` libraries still use
/// the catalog's env-dispatch path.
pub fn load_and_register_libraries<I, S>(
    specs: I,
) -> Result<leek_environment::CompositeCatalog, String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let mut composite = leek_environment::CompositeCatalog::new();
    for spec in specs {
        let spec = spec.as_ref();
        if matches!(spec, "leekwars" | "fight" | "fight.generator") {
            register_leekwars();
        } else {
            let lib = leek_environment::load(spec)?;
            register_environment(lib.as_ref());
            composite.push(lib);
        }
    }
    Ok(composite)
}

/// Opt in to folding the leek-wars constants (`WEAPON_PISTOL` → `37`) during
/// HIR lowering — what `leekc --fold-constants` does. The official-parity
/// fight runners need this: the goldens are generated from AIs compiled with
/// folding on, and the native backend has no runtime lookup for environment
/// constants.
pub fn activate_leekwars_constant_folding() {
    leek_prelude::activate_fold_constants(
        leek_environment::leekwars_constant_values()
            .into_iter()
            .map(|(n, v)| (n.to_string(), v.to_string())),
    );
}

/// Register the leek-wars game library from its typed signature header:
/// function names + arities (parsed from the header), the fight constants,
/// and activation of the header so HIR lowering merges its signatures +
/// `@java-dispatch:` directives.
fn register_leekwars() {
    for (name, lo, hi) in leekwars_header_arities() {
        leek_resolver::builtins::register_builtin_function(&name, lo, hi, 1);
    }
    for (name, _ty) in leek_environment::leekwars_constants() {
        leek_resolver::builtins::register_builtin_constant(name);
    }
    leek_prelude::activate_library(leek_prelude::LEEKWARS_SRC);
}

/// Parse the leek-wars signature header into `(name, min_arity, max_arity)`
/// rows — the per-name parameter-count range across overloads — so the
/// resolver recognizes the functions (no "undefined function").
fn leekwars_header_arities() -> Vec<(String, u8, u8)> {
    use leek_parser::ast::{AstNode, SourceFile};
    use leek_parser::{ParseFeatures, parse_with_features};
    use leek_syntax::{SyntaxKind, SyntaxNode, Version};
    let parsed = parse_with_features(
        leek_prelude::LEEKWARS_SRC,
        leek_prelude::source_id(),
        Version::V4,
        ParseFeatures {
            function_signatures: true,
            generics: true,
            ..Default::default()
        },
    );
    let Some(file) = SourceFile::cast(SyntaxNode::new_root(parsed.green)) else {
        return Vec::new();
    };
    let mut arities: std::collections::HashMap<String, (u8, u8)> = std::collections::HashMap::new();
    for child in file.syntax().children() {
        if child.kind() != SyntaxKind::FnDecl {
            continue;
        }
        let Some(name) = child
            .children_with_tokens()
            .filter_map(leek_syntax::language::NodeOrToken::into_token)
            .find(|t| t.kind() == SyntaxKind::Ident)
            .map(|t| t.text().to_string())
        else {
            continue;
        };
        let argc = child
            .children()
            .find(|n| n.kind() == SyntaxKind::ParamList)
            .map_or(0, |pl| {
                let n = pl
                    .children()
                    .filter(|n| n.kind() == SyntaxKind::Param)
                    .count();
                u8::try_from(n).unwrap_or(u8::MAX)
            });
        arities
            .entry(name)
            .and_modify(|(lo, hi)| {
                *lo = (*lo).min(argc);
                *hi = (*hi).max(argc);
            })
            .or_insert((argc, argc));
    }
    arities
        .into_iter()
        .map(|(n, (lo, hi))| (n, lo, hi))
        .collect()
}

/// What one library contributed when loaded, for verbose logging.
#[derive(Debug, Clone)]
pub struct LibraryStats {
    /// The spec as requested (`"leekwars"` or a file path).
    pub spec: String,
    /// Resolved import namespace(s) the catalog declares.
    pub imports: Vec<String>,
    /// Number of functions registered.
    pub functions: usize,
    /// Number of constants registered.
    pub constants: usize,
    /// A few function names (sorted) for a confirmation sample.
    pub sample_functions: Vec<String>,
    /// A few constant names (sorted) for a confirmation sample.
    pub sample_constants: Vec<String>,
}

/// Outcome of loading one library spec.
pub type LibraryLoadResult = Result<LibraryStats, String>;

/// Load each spec individually, register it with the resolver, and return a
/// per-spec report (counts + a sorted sample of names, or a load error).
///
/// Unlike [`load_and_register_libraries`], this keeps going past a failing
/// spec (recording the error in that spec's slot) and reports each library's
/// individual contribution — for surfacing in the LSP / CLI logs so users can
/// confirm their library's functions *and constants* actually loaded.
pub fn load_register_and_report<I, S>(specs: I) -> Vec<LibraryLoadResult>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let mut out = Vec::new();
    for spec in specs {
        let spec = spec.as_ref();
        match leek_environment::load(spec) {
            Ok(cat) => {
                register_environment(cat.as_ref());
                let mut fn_names: Vec<String> =
                    cat.entries().iter().map(|(n, _)| n.to_string()).collect();
                let mut const_names: Vec<String> =
                    cat.constants().iter().map(|(n, _)| n.to_string()).collect();
                fn_names.sort();
                const_names.sort();
                let functions = fn_names.len();
                let constants = const_names.len();
                fn_names.truncate(5);
                const_names.truncate(5);
                out.push(Ok(LibraryStats {
                    spec: spec.to_string(),
                    imports: cat.imports(),
                    functions,
                    constants,
                    sample_functions: fn_names,
                    sample_constants: const_names,
                }));
            }
            Err(e) => out.push(Err(format!("{spec}: {e}"))),
        }
    }
    out
}
