//! Shared pipeline recipes for `leekc`, `miku`, and the LSP.
//!
//! Call [`pipeline`] with a [`Target`] to get a fully planned [`Pipeline`],
//! then run it with [`Pipeline::run`] or [`Pipeline::run_memoized`].

use std::any::TypeId;

use leek_complexity::pipeline::ComplexityArtifact;
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
    /// HIR + per-function / per-method complexity report
    /// (`miku analyze`, `miku doc`).
    Complexity,
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
        Target::Complexity => plan_for::<ComplexityArtifact>(params),
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
///
/// The includes step is sequenced *before* `Parse` (it only needs the
/// entry text) so the entry parse can consume the include closure's
/// class names ([`leek_parser::pipeline::KnownClassesArtifact`]) —
/// `lowercaseClassFromInclude x = …` must parse as a typed declaration.
pub fn pipeline_hir_with_includes(
    includes: Box<dyn leek_pipeline::Step>,
    params: &RecipeParams,
) -> Result<Pipeline, RecipeError> {
    let mut plan = leek_pipeline::RecipePlan::new();
    plan.need::<TokensArtifact>(params)?;
    plan.push_step(includes, &[TypeId::of::<IncludeGraphArtifact>()]);
    plan.need::<AstArtifact>(params)?;
    plan.push_step(LowerHir::build(params), &[TypeId::of::<HirArtifact>()]);
    Ok(plan.build())
}

/// Like [`plan`], but with an include-resolution step sequenced after
/// lexing and *before* parsing — the multi-file (project) front-end.
/// See [`pipeline_hir_with_includes`] for why the ordering matters.
pub fn plan_with_includes(
    target: Target,
    includes: Box<dyn leek_pipeline::Step>,
    params: &RecipeParams,
) -> Result<leek_pipeline::RecipePlan, RecipeError> {
    let mut plan = leek_pipeline::RecipePlan::new();
    plan.need::<TokensArtifact>(params)?;
    plan.push_step(includes, &[TypeId::of::<IncludeGraphArtifact>()]);
    match target {
        Target::Tokens => {}
        Target::Parsed => plan.need::<GreenTreeArtifact>(params)?,
        Target::Resolved => plan.need::<ResolveArtifact>(params)?,
        Target::TypeChecked => plan.need::<TypeCheckArtifact>(params)?,
        Target::Hir => plan.need::<HirArtifact>(params)?,
        Target::Linted => plan.need::<LintFindings>(params)?,
        Target::Mir => plan.need::<MirArtifact>(params)?,
        Target::Complexity => plan.need::<ComplexityArtifact>(params)?,
    }
    Ok(plan)
}

/// Build the [`plan_with_includes`] pipeline.
pub fn pipeline_with_includes(
    target: Target,
    includes: Box<dyn leek_pipeline::Step>,
    params: &RecipeParams,
) -> Result<Pipeline, RecipeError> {
    plan_with_includes(target, includes, params).map(leek_pipeline::RecipePlan::build)
}

/// Like [`pipeline_with_includes`], but records per-step durations into `sink`.
pub fn pipeline_with_includes_timed(
    target: Target,
    includes: Box<dyn leek_pipeline::Step>,
    params: &RecipeParams,
    sink: &TimingSink,
) -> Result<Pipeline, RecipeError> {
    Ok(plan_with_includes(target, includes, params)?.build_timed(sink))
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
) -> Result<leek_environment::CompositeCatalog, LibraryLoadError>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let mut composite = leek_environment::CompositeCatalog::new();
    for spec in specs {
        let spec = spec.as_ref();
        if is_builtin_leekwars_spec(spec) {
            register_leekwars();
        } else {
            let lib = leek_environment::load(spec).map_err(|cause| LibraryLoadError {
                spec: spec.to_string(),
                cause,
            })?;
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

/// Whether `spec` names the built-in leek-wars library rather than a
/// library-definition file. These specs go through the typed signature
/// header (see [`register_leekwars`]); [`leek_environment::load`] answers
/// them with a constants-only catalog that exposes no functions at all, so
/// every loader path has to branch on this *before* falling back to it.
fn is_builtin_leekwars_spec(spec: &str) -> bool {
    matches!(spec, "leekwars" | "fight" | "fight.generator")
}

/// Register the leek-wars game library from its typed signature header:
/// function names + arities (parsed from the header), the fight constants,
/// and activation of the header so HIR lowering merges its signatures +
/// `@java-dispatch:` directives.
///
/// Returns the `(name, min_arity, max_arity)` rows it registered so the
/// reporting loader can describe the contribution without re-parsing the
/// header.
fn register_leekwars() -> Vec<(String, u8, u8)> {
    let arities = leekwars_header_arities();
    for (name, lo, hi) in &arities {
        leek_resolver::builtins::register_builtin_function(name, *lo, *hi, 1);
    }
    for (name, _ty) in leek_environment::leekwars_constants() {
        leek_resolver::builtins::register_builtin_constant(name);
    }
    leek_prelude::activate_library(leek_prelude::LEEKWARS_SRC);
    arities
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

/// One library spec that failed to load, and why.
///
/// Keeps the spec alongside the underlying
/// [`CatalogError`](leek_environment::CatalogError) so a caller reporting
/// several specs at once can say which one broke without parsing the message
/// back apart.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LibraryLoadError {
    /// The spec as the caller wrote it — a path, or a built-in name.
    pub spec: String,
    pub cause: leek_environment::CatalogError,
}

impl std::fmt::Display for LibraryLoadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.spec, self.cause)
    }
}

impl std::error::Error for LibraryLoadError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.cause)
    }
}

/// Outcome of loading one library spec.
pub type LibraryLoadResult = Result<LibraryStats, LibraryLoadError>;

/// Describe what the built-in leek-wars library contributed, given the
/// arity rows [`register_leekwars`] registered.
fn leekwars_stats(spec: &str, arities: &[(String, u8, u8)]) -> LibraryStats {
    let mut fn_names: Vec<String> = arities.iter().map(|(n, _, _)| n.clone()).collect();
    let mut const_names: Vec<String> = leek_environment::leekwars_constants()
        .iter()
        .map(|(n, _)| (*n).to_string())
        .collect();
    fn_names.sort();
    const_names.sort();
    let functions = fn_names.len();
    let constants = const_names.len();
    fn_names.truncate(5);
    const_names.truncate(5);
    LibraryStats {
        spec: spec.to_string(),
        // The header dispatches through `@java-dispatch:` directives, so
        // there is no import namespace to report.
        imports: Vec::new(),
        functions,
        constants,
        sample_functions: fn_names,
        sample_constants: const_names,
    }
}

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
        if is_builtin_leekwars_spec(spec) {
            // The header path, same as `load_and_register_libraries`. Going
            // through `leek_environment::load` here registered nothing (its
            // leekwars catalog has no functions) and left the prelude header
            // inactive, so the LSP — the only caller — logged "leekwars — 0
            // functions" and offered no signature, hover or completion for
            // `getCell` & co. while the CLIs had them all along.
            out.push(Ok(leekwars_stats(spec, &register_leekwars())));
            continue;
        }
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
            Err(cause) => out.push(Err(LibraryLoadError {
                spec: spec.to_string(),
                cause,
            })),
        }
    }
    out
}

#[cfg(test)]
mod recipe_shape_tests {
    //! The three recipes that are *not* derived from the artifact graph.
    //!
    //! `plan` gets its ordering checked target-by-target in
    //! leek-pipeline/tests/real_recipes.rs. These three build their plans
    //! by hand, so nothing else catches a step added, dropped or
    //! resequenced in them.

    use super::{
        Target, driver_params, pipeline_formatted, pipeline_hir_from_parse,
        pipeline_hir_with_includes, pipeline_timed,
    };
    use leek_pipeline::{Context, Step, Tap, TimingSink};

    fn fake_includes() -> Box<dyn Step> {
        Box::new(Tap::new("resolve-includes", |_: &mut Context<'_>| {}))
    }

    #[test]
    fn the_single_file_hir_recipe_skips_resolve_and_type_check() {
        // leek-resolver's multi-file tests depend on this chain staying
        // parse-only: adding `resolve` here would make every caller of
        // `pipeline_hir_from_parse` resolve names it deliberately doesn't
        // have yet.
        assert_eq!(
            pipeline_hir_from_parse(&driver_params())
                .expect("plan")
                .step_names(),
            ["pragma", "lex", "parse", "lower-hir"]
        );
    }

    #[test]
    fn the_include_aware_hir_recipe_resolves_includes_before_parsing() {
        assert_eq!(
            pipeline_hir_with_includes(fake_includes(), &driver_params())
                .expect("plan")
                .step_names(),
            ["pragma", "lex", "resolve-includes", "parse", "lower-hir"]
        );
    }

    #[test]
    fn formatting_runs_last_on_top_of_the_parsed_chain() {
        // `leekc --emit fmt` and the LSP's formatting request both read the
        // `FormattedArtifact` this appends; `fmt` needs the green tree, so
        // it can only ever be the final step.
        assert_eq!(
            pipeline_formatted(leek_fmt::FormatOptions::default(), &driver_params())
                .expect("plan")
                .step_names(),
            ["pragma", "lex", "parse", "fmt"]
        );
    }

    #[test]
    fn timing_wraps_every_step_and_changes_none_of_them() {
        // `miku dev --verbose` and leek-bench read the sink. The wrapper
        // must be transparent: same steps, same names, one entry each.
        for target in [
            Target::Tokens,
            Target::Parsed,
            Target::Resolved,
            Target::TypeChecked,
            Target::Hir,
            Target::Linted,
            Target::Mir,
            Target::Complexity,
        ] {
            let plain = super::pipeline(target, &driver_params())
                .unwrap_or_else(|e| panic!("{target:?}: {e}"))
                .step_names();
            let sink = TimingSink::new();
            let timed = pipeline_timed(target, &driver_params(), &sink)
                .unwrap_or_else(|e| panic!("{target:?}: {e}"))
                .step_names();
            assert_eq!(plain, timed, "{target:?}: timing changed the plan");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Registration writes a process-global table shared by every test in
    /// this binary, so each assertion below is *monotone*: it checks that a
    /// name is registered with a particular arity, never that something is
    /// absent. Anything order-dependent would be flaky under `cargo test`'s
    /// thread-per-test model.
    fn arity_of(rows: &[(String, u8, u8)], name: &str) -> (u8, u8) {
        let (_, lo, hi) = rows
            .iter()
            .find(|(n, _, _)| n == name)
            .unwrap_or_else(|| panic!("`{name}` missing from the leekwars header rows"));
        (*lo, *hi)
    }

    #[test]
    fn leekwars_header_yields_the_documented_arity_ranges() {
        // Guards both the silent `Vec::new()` on a parse failure and the
        // per-name min/max fold across overloads. A header/parser regression
        // that disables all 305 functions shows up here rather than as
        // "undefined function" in an editor.
        // 305 signatures in the header fold into 195 distinct names.
        let rows = leekwars_header_arities();
        assert!(
            rows.len() >= 190,
            "only {} functions parsed from the header",
            rows.len()
        );
        // `getLife(entity)` / `getLife()`; `getCell(entity)` / `getCell()`.
        assert_eq!(arity_of(&rows, "getLife"), (0, 1));
        assert_eq!(arity_of(&rows, "getCell"), (0, 1));
        // Single-signature functions collapse to a point range.
        assert_eq!(arity_of(&rows, "say"), (1, 1));
        assert_eq!(arity_of(&rows, "useWeapon"), (1, 1));
        // `moveToward(entity, mp)` / `moveToward(entity)`.
        assert_eq!(arity_of(&rows, "moveToward"), (1, 2));
        // `markText` ranges over 0..=3 parameters across its overloads.
        assert_eq!(arity_of(&rows, "markText"), (0, 3));
        // Every row is a real range.
        for (name, lo, hi) in &rows {
            assert!(lo <= hi, "{name}: {lo} > {hi}");
        }
    }

    #[test]
    fn header_names_are_unique_per_function() {
        let rows = leekwars_header_arities();
        let mut names: Vec<&str> = rows.iter().map(|(n, _, _)| n.as_str()).collect();
        names.sort_unstable();
        let before = names.len();
        names.dedup();
        assert_eq!(before, names.len(), "overloads must fold into one row");
    }

    #[test]
    fn reporting_loader_registers_the_leekwars_header() {
        // Regression: `load_register_and_report` (the LSP's loader) went
        // straight to `leek_environment::load`, whose leekwars catalog
        // exposes no functions — so the editor reported "0 functions" and
        // flagged every `getCell()` as undefined while `leekc --library
        // leekwars` accepted it.
        let reports = load_register_and_report(["leekwars"]);
        assert_eq!(reports.len(), 1);
        let stats = reports[0].as_ref().expect("leekwars loads");
        assert_eq!(stats.spec, "leekwars");
        assert!(
            stats.functions >= 190,
            "reported {} functions",
            stats.functions
        );
        assert!(stats.constants > 0);
        assert_eq!(stats.sample_functions.len(), 5);
        assert!(stats.sample_functions.windows(2).all(|w| w[0] <= w[1]));

        // …and the registration actually reached the resolver.
        assert_eq!(
            leek_resolver::builtins::builtin_fn_meta("getCell"),
            Some((0, 1, 1))
        );
        assert_eq!(
            leek_resolver::builtins::builtin_fn_meta("moveToward"),
            Some((1, 2, 1))
        );
        assert!(leek_resolver::builtins::is_builtin_constant(
            "WEAPON_PISTOL"
        ));
    }

    #[test]
    fn the_leekwars_aliases_take_the_same_header_path() {
        for spec in ["fight", "fight.generator"] {
            assert!(is_builtin_leekwars_spec(spec), "{spec}");
            let reports = load_register_and_report([spec]);
            let stats = reports[0]
                .as_ref()
                .unwrap_or_else(|e| panic!("{spec}: {e}"));
            assert!(stats.functions >= 190, "{spec}: {}", stats.functions);
        }
        assert!(!is_builtin_leekwars_spec("leekwars.lib"));
        assert!(!is_builtin_leekwars_spec("/tmp/leekwars"));
    }

    #[test]
    fn leekwars_contributes_no_catalog_entries_but_still_registers() {
        // The documented contract: the header is not a TSV catalog, so the
        // composed catalog stays empty while the resolver learns the names.
        let catalog = load_and_register_libraries(["leekwars"]).expect("load");
        assert!(leek_environment::EnvironmentCatalog::entries(&catalog).is_empty());
        assert_eq!(
            leek_resolver::builtins::builtin_fn_meta("useWeapon"),
            Some((1, 1, 1))
        );
    }

    #[test]
    fn a_missing_library_file_is_an_error_in_both_loaders() {
        let missing = "/definitely/not/a/library/file.lib";
        let err = load_and_register_libraries([missing]).expect_err("missing file");
        assert_eq!(err.spec, missing);
        assert!(
            matches!(err.cause, leek_environment::CatalogError::Io { .. }),
            "{err:?}"
        );

        // The reporting loader keeps going past a failure and records it in
        // that spec's slot — the documented difference between the two.
        let reports = load_register_and_report(["leekwars", missing]);
        assert_eq!(reports.len(), 2);
        assert!(reports[0].is_ok());
        let err = reports[1].as_ref().expect_err("missing file");
        assert_eq!(err.spec, missing);
    }

    #[test]
    fn file_catalogs_register_their_declared_arities_verbatim() {
        let dir = std::env::temp_dir().join(format!(
            "leek-session-recipes-lib-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::create_dir_all(&dir).expect("scratch dir");
        let path = dir.join("demo.lib");
        std::fs::write(
            &path,
            "namespace = com.example.demo.*\n\
             # name\tclass\tkind\tmin\tmax\tops\n\
             demoZeroArg\tDemoClass\tstatic\t0\t0\t1\n\
             demoRange\tDemoClass\tstatic\t1\t3\t2\n\
             const DEMO_CONSTANT\tinteger\n",
        )
        .expect("write library");

        let catalog = load_and_register_libraries([path.to_str().expect("utf-8 path")])
            .expect("load file catalog");
        assert_eq!(
            leek_environment::EnvironmentCatalog::imports(&catalog),
            ["com.example.demo.*"]
        );
        assert_eq!(
            leek_resolver::builtins::builtin_fn_meta("demoZeroArg"),
            Some((0, 0, 1))
        );
        assert_eq!(
            leek_resolver::builtins::builtin_fn_meta("demoRange"),
            Some((1, 3, 1))
        );
        assert!(leek_resolver::builtins::is_builtin_constant(
            "DEMO_CONSTANT"
        ));

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_malformed_library_file_reports_the_offending_line() {
        let dir = std::env::temp_dir().join(format!(
            "leek-session-recipes-bad-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::create_dir_all(&dir).expect("scratch dir");
        let path = dir.join("bad.lib");
        // A non-numeric arity field must fail loudly rather than register a
        // 0-arity entry that rejects every call.
        std::fs::write(&path, "brokenFn\tDemoClass\tstatic\tnope\n").expect("write");
        let err = load_and_register_libraries([path.to_str().expect("utf-8 path")])
            .expect_err("malformed");
        assert_eq!(
            err.cause,
            leek_environment::CatalogError::BadField {
                line: 1,
                field: 3,
                raw: "nope".to_string(),
            }
        );

        std::fs::remove_dir_all(&dir).ok();
    }
}
