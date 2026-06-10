//! Pipeline integration: HIR lowering as a [`Step`].

use std::sync::Arc;

use leek_diagnostics::Diagnostic;
use leek_parser::ast::AstNode;
use leek_parser::pipeline::AstArtifact;
use leek_pipeline::{Artifact, Context, OptLevel, Step, StepError};
use leek_pipeline::{RecipeArtifact, RecipeParams, RecipeStep};
use leek_resolver::pipeline::IncludeGraphArtifact;
use leek_span::SourceId;

use crate::HirFile;
use crate::lower::{
    lower_file_versioned_with_flags, lower_file_with_prelude_with_flags, lower_files,
};

/// Parse the active library/prelude headers (the implicit prelude when
/// enabled, plus any `--library` headers like leekwars) into a single
/// signature AST to merge ahead of the user file. `None` when nothing
/// is active. Parsed with bodiless signatures + generics enabled so the
/// headers' typed signatures and `@<backend>-dispatch:` directives load.
fn parse_prelude(prelude_enabled: bool) -> Option<(leek_parser::ast::SourceFile, SourceId)> {
    use leek_parser::ast::{AstNode, SourceFile as AstSourceFile};
    use leek_syntax::SyntaxNode;
    let combined = leek_prelude::merged_header_src(prelude_enabled)?;
    let src = leek_prelude::source_id();
    let parsed = leek_parser::parse_with_features(
        &combined,
        src,
        leek_syntax::Version::V4,
        leek_parser::ParseFeatures {
            function_signatures: true,
            generics: true,
            ..Default::default()
        },
    );
    let ast = AstSourceFile::cast(SyntaxNode::new_root(parsed.green))?;
    Some((ast, src))
}

/// Pick the language version for lowering: an explicit `// @version:N`
/// pragma always wins; otherwise fall back to the out-of-band
/// `Input::version_byte` (which the corpus runner and editors set).
/// Previously `lower_file` defaulted pragma-less sources to v4,
/// silently dropping the caller's version (e.g. v1 string-escape rules).
fn effective_version(src_text: &str, source: SourceId, fallback_byte: u8) -> u8 {
    let (pragmas, _) = leek_syntax::parse_pragmas(src_text, source);
    if pragmas.version_explicit {
        match pragmas.version {
            leek_syntax::Version::V1 => 1,
            leek_syntax::Version::V2 => 2,
            leek_syntax::Version::V3 => 3,
            leek_syntax::Version::V4 => 4,
        }
    } else {
        fallback_byte
    }
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
    if let Some(graph) = cx.get::<IncludeGraphArtifact>() {
        let includes: Vec<_> = graph
            .includes
            .iter()
            .map(|inc| (inc.ast.clone(), inc.source, inc.path.clone()))
            .collect();
        let (hir, diagnostics) = lower_files(
            (&ast, cx.source(), &graph.entry_path),
            &includes,
            &graph.resolved,
        );
        return (finish_hir(hir, opt), diagnostics);
    }
    let src_text = ast.syntax().text().to_string();
    let version = effective_version(&src_text, cx.source(), cx.version_byte());
    let flags = cx.flags();
    if let Some((prelude, prelude_src)) = parse_prelude(flags.prelude) {
        let (hir, diagnostics) = lower_file_with_prelude_with_flags(
            &ast,
            cx.source(),
            version,
            &prelude,
            prelude_src,
            flags,
        );
        return (finish_hir(hir, opt), diagnostics);
    }
    let (hir, diagnostics) = lower_file_versioned_with_flags(&ast, cx.source(), version, flags);
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
    let src_text = ast.syntax().text().to_string();
    let version = effective_version(&src_text, file.source(db), file.version_byte(db));
    let flags = leek_pipeline::FeatureFlags::from_bits(file.flags_bits(db));
    if let Some((prelude, prelude_src)) = parse_prelude(flags.prelude) {
        let (hir, diagnostics) = lower_file_with_prelude_with_flags(
            &ast,
            file.source(db),
            version,
            &prelude,
            prelude_src,
            flags,
        );
        return LowerHirResult {
            hir: finish_hir(hir, OptLevel::O0),
            diagnostics,
        };
    }
    let (hir, diagnostics) = lower_file_versioned_with_flags(&ast, file.source(db), version, flags);
    LowerHirResult {
        hir: finish_hir(hir, OptLevel::O0),
        diagnostics,
    }
}
