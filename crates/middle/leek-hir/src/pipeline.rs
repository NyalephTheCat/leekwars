//! Pipeline integration: HIR lowering as a [`Step`].

use std::sync::Arc;

use leek_diagnostics::Diagnostic;
use leek_parser::ast::AstNode;
use leek_parser::pipeline::AstArtifact;
use leek_pipeline::{Artifact, Context, OptLevel, Step, StepError};
use leek_pipeline::{RecipeArtifact, RecipeParams, RecipeStep};
use leek_resolver::pipeline::IncludeGraphArtifact;
use leek_span::SourceId;
use leek_syntax::Version;

use crate::HirFile;
use crate::lower::{
    LowerUnit, PRELUDE_UNIT_PATH, lower_file_versioned_with_flags,
    lower_file_with_prelude_with_flags, lower_files,
};

/// Parse the active library/prelude headers (the implicit prelude when
/// enabled, plus any `--library` headers like leekwars) into a single
/// signature AST to merge ahead of the user file. `None` when nothing
/// is active.
///
/// Parsed at the **program's** language version through the shared,
/// version-keyed [`leek_parser::parse_signature_header`] cache — the same
/// one the type checker seeds signatures from — so lowering and checking
/// see the same header tree and nothing is re-parsed per compile.
fn parse_prelude(
    prelude_enabled: bool,
    version: Version,
) -> Option<(leek_parser::ast::SourceFile, SourceId)> {
    use leek_parser::ast::SourceFile as AstSourceFile;
    use leek_syntax::SyntaxNode;
    let combined = leek_prelude::merged_header_src(prelude_enabled)?;
    let green = leek_parser::parse_signature_header(&combined, version);
    let ast = AstSourceFile::cast(SyntaxNode::new_root(green))?;
    Some((ast, leek_prelude::source_id()))
}

/// Lower one file at the pipeline's settled language settings: `version`
/// is `Input::version_byte` (already resolved from override > pragma >
/// default by the driver), never re-derived from the file's pragmas.
fn lower_single(
    ast: &leek_parser::ast::SourceFile,
    source: SourceId,
    version_byte: u8,
    flags: leek_pipeline::FeatureFlags,
) -> (HirFile, Vec<Diagnostic>) {
    if let Some((prelude, prelude_src)) =
        parse_prelude(flags.prelude, Version::from_byte(version_byte))
    {
        return lower_file_with_prelude_with_flags(
            ast,
            source,
            version_byte,
            &prelude,
            prelude_src,
            flags,
        );
    }
    lower_file_versioned_with_flags(ast, source, version_byte, flags)
}

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
    // An include graph is assembled outside salsa from the workspace's
    // open-buffer/disk snapshot. Lower it directly so the graph is not lost
    // when the ordinary single-file salsa query is available.
    if let Some(graph) = cx.get::<IncludeGraphArtifact>()
        && !graph.includes.is_empty()
        && let Some(ast) = cx.get::<AstArtifact>().and_then(|a| a.0.clone())
    {
        let version = Version::from_byte(cx.version_byte());
        let flags = cx.flags();
        // Active library headers (e.g. leekwars) merge in as a
        // synthetic front unit: their bodiless signatures are
        // pre-declared before every user file's, mirroring the
        // single-file prelude path. No `include` statement resolves
        // to the synthetic path, so it contributes no main block.
        let prelude = parse_prelude(flags.prelude, version);
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
        return (finish_hir(hir, opt), diagnostics);
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
        .and_then(|a| a.0.clone())
        .expect("LowerHir::run guards on AstArtifact presence outside the salsa path");
    let (hir, diagnostics) = lower_single(&ast, cx.source(), cx.version_byte(), cx.flags());
    (finish_hir(hir, opt), diagnostics)
}

/// Apply the opt-in constant-folding pass (if any constants are active),
/// then wrap the lowered HIR in an `Arc`. Routing every fresh-HIR return
/// site through this means *all* downstream consumers — the Java backend
/// (reads `HirArtifact`) and MIR/native/interp (lower from the same
/// `HirArtifact`) — see folded literals from one hook. A no-op (and
/// allocation-free) when no fold constants are registered, so the default
/// path and the corpus baseline are unchanged.
fn finish_hir(mut hir: HirFile, opt: OptLevel) -> Arc<HirFile> {
    let pairs = leek_prelude::fold_constants();
    if !pairs.is_empty() {
        let map: std::collections::HashMap<String, crate::ir::Literal> = pairs
            .into_iter()
            .filter_map(|(name, value)| {
                let lit = if value.contains('.') {
                    value.parse::<f64>().ok().map(crate::ir::Literal::Real)
                } else {
                    value.parse::<i64>().ok().map(crate::ir::Literal::Int)
                }?;
                Some((name, lit))
            })
            .collect();
        crate::transform::fold_constants(&mut hir, &map);
    }
    // Backend-agnostic optimization — only at O1. Propagation and folding run
    // to a fixpoint so chained constants (`var A = 2; var B = A + 1; …`) fully
    // resolve.
    if opt.optimizes() {
        crate::transform::optimize_hir(&mut hir);
    }
    Arc::new(hir)
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
    use leek_syntax::SyntaxNode;

    let parse = leek_parser::pipeline::parse_query(db, file);
    let Some(ast) = AstSourceFile::cast(SyntaxNode::new_root(parse.green.clone())) else {
        return LowerHirResult {
            hir: Arc::new(HirFile::default()),
            diagnostics: Vec::new(),
        };
    };
    let flags = leek_pipeline::FeatureFlags::from_bits(file.flags_bits(db));
    let (hir, diagnostics) = lower_single(&ast, file.source(db), file.version_byte(db), flags);
    LowerHirResult {
        hir: finish_hir(hir, OptLevel::O0),
        diagnostics,
    }
}
