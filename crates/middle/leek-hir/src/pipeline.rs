//! Pipeline integration: HIR lowering as a [`Step`].
//!
//! The composition this step drives — parse the active headers, lower the
//! file against them, fold and optimize — lives in [`crate::lower`] and
//! [`crate::fold`] as public functions, so a driver that is not a pipeline
//! can assemble the same stages itself.
//!
//! A note on what lowering does and does not share with the type checker:
//! lowering parses the **concatenation** of `PRELUDE_SRC` and the active
//! libraries under one cache key, while the checker parses `STDLIB_SRC` and
//! `LEEKWARS_SRC` as two separate keys (the `seed_header` calls in
//! `leek-types`' `checker/file.rs`). Both go through
//! [`leek_parser::parse_signature_header`], so they share its cache — but
//! never an entry in it. Each pass parses its own text once per language
//! version; neither re-parses per compile.

use std::sync::Arc;

use leek_diagnostics::Diagnostic;
use leek_parser::pipeline::AstArtifact;
use leek_pipeline::{Artifact, Context, OptLevel, Step, StepError};
use leek_pipeline::{RecipeArtifact, RecipeParams, RecipeStep};
use leek_resolver::pipeline::IncludeGraphArtifact;
use leek_syntax::Version;

use crate::HirFile;
use crate::fold::fold_map;
use crate::lower::{LowerUnit, PRELUDE_UNIT_PATH, finish, lower_files, lower_one, prelude_tree};

/// Lowered HIR.
///
/// The inner `HirFile` is held by [`Arc`] so the salsa cache path
/// stays pointer-cheap on hits — without the `Arc`, every cached
/// read would deep-clone the entire tree.
#[derive(Debug, Clone)]
pub struct HirArtifact(pub Arc<HirFile>);
impl Artifact for HirArtifact {}

/// AST → HIR lowering. Skipped silently if no AST is in the context
/// (catastrophic parse error).
///
/// `opt` controls whether the backend-agnostic [`fold_expressions`] pass runs
/// after lowering. It is taken from the recipe's [`OptLevel`] so codegen
/// drivers (`miku run`, `miku build --clean`, native) optimize while analysis
/// drivers and Java *exact* mode keep the IR source-faithful.
///
/// [`fold_expressions`]: crate::transform::fold_expressions
pub struct LowerHir {
    opt: OptLevel,
}

impl LowerHir {
    /// A lowering step at the given [`OptLevel`]. Recipes build this via
    /// [`RecipeStep::build`] from [`RecipeParams::opt`]; this constructor is
    /// for manual `.with(...)` pipeline composition.
    #[must_use]
    pub fn new(opt: OptLevel) -> Self {
        Self { opt }
    }
}

impl Default for LowerHir {
    fn default() -> Self {
        Self { opt: OptLevel::O0 }
    }
}

impl Step for LowerHir {
    fn name(&self) -> &'static str {
        "lower-hir"
    }
    fn run(&self, cx: &mut Context<'_>) -> Result<(), StepError> {
        if cx.get::<AstArtifact>().is_none() {
            // Some pipelines wire LowerHir without Parse for the
            // salsa path (where parse_query is dispatched internally
            // by lower_hir_query). Fall through in that case too.
            #[cfg(feature = "salsa")]
            if cx.salsa().is_none() {
                return Ok(());
            }
            #[cfg(not(feature = "salsa"))]
            return Ok(());
        }
        let (hir, diagnostics) = run_lower(cx, self.opt);
        cx.emit_all(diagnostics);
        cx.insert(HirArtifact(hir));
        Ok(())
    }
}

impl RecipeStep for LowerHir {
    fn build(params: &RecipeParams) -> Box<dyn leek_pipeline::Step> {
        Box::new(LowerHir { opt: params.opt })
    }
}

impl RecipeArtifact for HirArtifact {
    type Producer = LowerHir;
    type Requires = (leek_types::pipeline::TypeCheckArtifact,);
    type Produces = (HirArtifact,);
}

/// Salsa-aware lower driver. Returns the lowered HIR (wrapped in an
/// `Arc`) plus the diagnostics it produced.
///
/// When an [`IncludeGraphArtifact`] is present (i.e. a
/// `ResolveIncludes` step ran earlier), dispatches to the
/// multi-file path so cross-file functions / classes / globals
/// merge into the entry's HIR. Otherwise stays on the single-file
/// path — existing pipelines without `ResolveIncludes` are
/// unchanged.
fn run_lower(cx: &Context<'_>, opt: OptLevel) -> (Arc<HirFile>, Vec<Diagnostic>) {
    // Entry boundary for the compilation configuration (#98, #226). Which
    // libraries and constant catalogs are active is still a process-global,
    // so it is sampled once here, at the edge, and threaded down as plain
    // values — the lowering below reads no global. A later slice of epic #346
    // replaces these two reads with the configuration the driver hands in.
    let libraries = leek_prelude::active_library_set();
    let fold = leek_prelude::active_fold_set();

    // An include graph is assembled outside salsa from the workspace's
    // open-buffer/disk snapshot. Lower it directly so the graph is not lost
    // when the ordinary single-file salsa query is available.
    if let Some(graph) = cx.get::<IncludeGraphArtifact>()
        && !graph.includes.is_empty()
        && let Some(ast) = cx.get::<AstArtifact>().map(|a| a.0.clone())
    {
        let version = Version::from_byte(cx.version_byte());
        let flags = cx.flags();
        // Active library headers (e.g. leekwars) merge in as a
        // synthetic front unit: their bodiless signatures are
        // pre-declared before every user file's, mirroring the
        // single-file prelude path. No `include` statement resolves
        // to the synthetic path, so it contributes no main block.
        let prelude = prelude_tree(libraries, flags.prelude, version);
        let mut units: Vec<LowerUnit<'_>> = Vec::with_capacity(graph.includes.len() + 1);
        if let Some((prelude_ast, prelude_src)) = &prelude {
            units.push(LowerUnit {
                ast: prelude_ast,
                source: *prelude_src,
                path: std::path::Path::new(PRELUDE_UNIT_PATH),
                version,
            });
        }
        // Each included file keeps the version the include walker lexed and
        // parsed it at (its own pragma, else the entry's version).
        units.extend(graph.includes.iter().map(|inc| LowerUnit {
            ast: &inc.ast,
            source: inc.source,
            path: &inc.path,
            version: inc.version,
        }));
        let entry = LowerUnit {
            ast: &ast,
            source: cx.source(),
            path: &graph.entry_path,
            version,
        };
        let (hir, diagnostics) = lower_files(entry, &units, Some(&graph.resolved), flags);
        return (finish(hir, &fold_map(fold), opt), diagnostics);
    }

    #[cfg(feature = "salsa")]
    if let Some((db, file)) = cx.salsa() {
        let out = lower_hir_query(db, file);
        // The salsa-tracked query is keyed only on the source file, not on the
        // recipe's opt level, so it always produces unoptimized HIR (the LSP /
        // analysis use case). Apply optimization outside the cache when a
        // codegen driver asked for it.
        if opt.optimizes() {
            let mut hir = (*out.hir).clone();
            crate::transform::optimize_hir(&mut hir);
            return (Arc::new(hir), out.diagnostics);
        }
        return (out.hir, out.diagnostics);
    }
    let ast = cx
        .get::<AstArtifact>()
        .map(|a| a.0.clone())
        .expect("LowerHir::run guards on AstArtifact presence outside the salsa path");
    let flags = cx.flags();
    let prelude = prelude_tree(
        libraries,
        flags.prelude,
        Version::from_byte(cx.version_byte()),
    );
    let (hir, diagnostics) = lower_one(
        &ast,
        cx.source(),
        cx.version_byte(),
        flags,
        prelude.as_ref().map(|(tree, source)| (tree, *source)),
    );
    (finish(hir, &fold_map(fold), opt), diagnostics)
}

/// Tracked return type for [`lower_hir_query`]: the HIR (in an
/// `Arc` for cheap cloning) plus the lowering pass's own diagnostics.
#[cfg_attr(feature = "salsa", derive(salsa::Update))]
#[derive(Debug, Clone, PartialEq)]
pub struct LowerHirResult {
    pub hir: Arc<HirFile>,
    pub diagnostics: Vec<Diagnostic>,
}

/// Salsa-tracked entry point for HIR lowering. Re-runs only when the
/// upstream [`parse_query`](leek_parser::pipeline::parse_query)'s
/// green tree changes.
#[cfg(feature = "salsa")]
#[salsa::tracked]
pub fn lower_hir_query(
    db: &dyn leek_pipeline::salsa::Db,
    file: leek_pipeline::salsa::SourceFile,
) -> LowerHirResult {
    use leek_parser::ast::{AstNode, SourceFile as AstSourceFile};
    use leek_pipeline::salsa::ProgramClasses;
    use leek_syntax::SyntaxNode;

    // Entry boundary for the compilation configuration (#98, #226): sampled
    // here rather than inside `prelude_tree` / `fold_map`, which are now pure
    // functions of these values. The read is still a process-global and so is
    // invisible to salsa — a later slice of epic #346 replaces it with a
    // tracked input on `file`, at which point this query re-runs when the
    // configuration changes.
    let libraries = leek_prelude::active_library_set();
    let fold = leek_prelude::active_fold_set();

    let parse = leek_parser::pipeline::parse_query(db, file, ProgramClasses::none(db));
    let Some(ast) = AstSourceFile::cast(SyntaxNode::new_root(parse.green.clone())) else {
        return LowerHirResult {
            hir: Arc::new(HirFile::default()),
            diagnostics: Vec::new(),
        };
    };
    let flags = leek_span::FeatureFlags::from_bits(file.flags_bits(db));
    let version_byte = file.version_byte(db);
    let prelude = prelude_tree(libraries, flags.prelude, Version::from_byte(version_byte));
    let (hir, diagnostics) = lower_one(
        &ast,
        file.source(db),
        version_byte,
        flags,
        prelude.as_ref().map(|(tree, source)| (tree, *source)),
    );
    LowerHirResult {
        hir: finish(hir, &fold_map(fold), OptLevel::O0),
        diagnostics,
    }
}
