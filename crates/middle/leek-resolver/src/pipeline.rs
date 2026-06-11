//! Pipeline integration: resolver as a [`Step`].

use std::path::PathBuf;
use std::sync::Arc;

use leek_diagnostics::Diagnostic;
use leek_parser::ast::SourceFile;
use leek_parser::pipeline::{AstArtifact, KnownClassesArtifact, parse_file_with_classes};
use leek_pipeline::{Artifact, Context, Step, StepError};
use leek_pipeline::{RecipeArtifact, RecipeParams, RecipeStep};
use leek_span::Span;
use leek_syntax::Version;
use leek_syntax::pipeline::PragmasArtifact;
use leek_syntax::pipeline::version_from_byte;

use crate::folder::Folder;
use crate::include_graph::{ResolvedFile, build_include_graph};
use crate::index::ResolveTable;
use crate::{Options, ResolveResult, resolve_collecting};

/// Resolver outcome.
///
/// Carries both the diagnostic list and the LSP-facing
/// [`ResolveTable`] of symbols + references. Direct callers that
/// only need diagnostics ignore `table`.
#[cfg_attr(feature = "salsa", derive(salsa::Update))]
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ResolveArtifact {
    pub diagnostics: Vec<Diagnostic>,
    pub table: ResolveTable,
}
impl Artifact for ResolveArtifact {}

/// Resolver step. Reads the AST from [`leek_parser::pipeline::Parse`].
pub struct Resolve;

impl Step for Resolve {
    fn name(&self) -> &'static str {
        "resolve"
    }
    fn run(&self, cx: &mut Context<'_>) -> Result<(), StepError> {
        let ResolveResult { diagnostics, table } = run_resolve(cx);
        cx.emit_all(diagnostics.iter().cloned());
        cx.insert(ResolveArtifact { diagnostics, table });
        Ok(())
    }
}

impl RecipeStep for Resolve {
    fn build(_: &RecipeParams) -> Box<dyn leek_pipeline::Step> {
        Box::new(Resolve)
    }
}

impl RecipeArtifact for ResolveArtifact {
    type Producer = Resolve;
    type Requires = (AstArtifact,);
    type Produces = (ResolveArtifact,);
}

/// Salsa-aware resolve driver.
fn run_resolve(cx: &Context<'_>) -> ResolveResult {
    #[cfg(feature = "salsa")]
    if let Some((db, file)) = cx.salsa() {
        let art = resolve_query(db, file);
        return ResolveResult {
            diagnostics: art.diagnostics,
            table: art.table,
        };
    }
    let Some(ast) = cx.get::<AstArtifact>().and_then(|a| a.0.clone()) else {
        return ResolveResult::default();
    };
    let experimental_imports = cx
        .get::<PragmasArtifact>()
        .is_some_and(|p| p.0.experimental.iter().any(|f| f == "imports"));
    let experimental_overloads = cx
        .get::<PragmasArtifact>()
        .map(|p| &p.0)
        .is_some_and(pragma_overloads)
        || cx.flags().overloads;
    let opts = Options {
        strict: cx.strict(),
        experimental_imports,
        experimental_overloads,
    };
    resolve_collecting(
        &ast,
        cx.source(),
        version_from_byte(cx.version_byte()),
        opts,
    )
}

/// True when the file opts into experimental function overloads via a
/// `// @experimental: overloads` pragma.
fn pragma_overloads(pragmas: &leek_syntax::Pragmas) -> bool {
    pragmas.experimental.iter().any(|f| f == "overloads")
}

/// One included file's parsed view, ready for the HIR lowerer to
/// consume. Carries the AST, the canonical path the include graph
/// resolved to, and the per-file source/version metadata.
#[derive(Debug, Clone)]
pub struct ParsedIncludedFile {
    pub source: leek_span::SourceId,
    pub path: PathBuf,
    pub text: String,
    pub version: Version,
    pub ast: SourceFile,
}

/// Artifact emitted by [`ResolveIncludes`]. Carries every file the
/// entry transitively includes (in topological order, leaves first)
/// plus the (includer, name) → canonical-path lookup the HIR
/// lowerer uses to splice `Stmt::Include` sites.
#[derive(Debug, Clone, Default)]
pub struct IncludeGraphArtifact {
    /// Included files in dependency order, leaves first. **Excludes
    /// the entry file** — the entry's AST already lives in the
    /// existing [`AstArtifact`].
    pub includes: Vec<ParsedIncludedFile>,
    /// `(includer_canonical, include_name)` → included canonical
    /// path. The HIR lowerer's splice routine resolves names
    /// through this map.
    pub resolved: std::collections::BTreeMap<(PathBuf, String), PathBuf>,
    /// Forward edges keyed by canonical path. Used by callers
    /// (LSP, miku) to invalidate caches when a leaf changes.
    pub forward: std::collections::BTreeMap<PathBuf, std::collections::BTreeSet<PathBuf>>,
    /// Canonical path of the entry file. Needed by the lowerer to
    /// look up resolved-include paths from the entry's own
    /// `Stmt::Include` sites.
    pub entry_path: PathBuf,
}

impl Artifact for IncludeGraphArtifact {}

/// Pipeline step that walks `include("…")` calls transitively
/// using the provided [`Folder`].
///
/// Add this before [`leek_hir::pipeline::LowerHir`] (or its
/// future multi-file variant) so the lowerer has every file's
/// parsed AST in the [`IncludeGraphArtifact`]. Without this step
/// the existing single-file flow runs unchanged.
///
/// `source_allocator` is the per-pipeline strategy for issuing
/// `SourceId`s to newly-discovered include files. Callers that
/// need stable ids across runs (LSP) pass a closure that maps
/// canonical paths to ids they've already allocated; one-shot CLI
/// users (miku) can use a simple monotonic counter.
pub struct ResolveIncludes {
    pub folder: Arc<dyn Folder>,
    pub entry_path: PathBuf,
    pub source_allocator:
        Arc<std::sync::Mutex<dyn FnMut(&std::path::Path) -> leek_span::SourceId + Send>>,
}

impl ResolveIncludes {
    /// Convenience constructor with a monotonic-counter allocator
    /// starting at `start`. Each newly-discovered include file gets
    /// a fresh sequential `SourceId`.
    pub fn with_counter(folder: Arc<dyn Folder>, entry_path: PathBuf, start: u32) -> Self {
        let mut next = start;
        let allocator: Arc<
            std::sync::Mutex<dyn FnMut(&std::path::Path) -> leek_span::SourceId + Send>,
        > = Arc::new(std::sync::Mutex::new(move |_p: &std::path::Path| {
            let id = leek_span::SourceId::new(next).expect("non-zero SourceId");
            next += 1;
            id
        }));
        Self {
            folder,
            entry_path,
            source_allocator: allocator,
        }
    }
}

impl Step for ResolveIncludes {
    fn name(&self) -> &'static str {
        "resolve_includes"
    }
    fn run(&self, cx: &mut Context<'_>) -> Result<(), StepError> {
        let graph = {
            let mut alloc = self.source_allocator.lock().map_err(|e| StepError {
                step: "resolve_includes",
                message: format!("source allocator poisoned: {e}"),
            })?;
            build_include_graph(&self.entry_path, cx.text(), &*self.folder, |p| (alloc)(p))
        };

        cx.emit_all(graph.diagnostics.iter().cloned());

        // Collect every `class IDENT` name across the include closure
        // (entry included) and publish it for the `Parse` step, which
        // runs *after* this one when the pipeline is include-aware.
        // Upstream resolves potential type words against the
        // program-wide defined-class set, so the entry must parse
        // `lowercaseClassFromInclude x = …` as a typed declaration.
        let mut known_classes: Vec<String> = Vec::new();
        for f in &graph.files {
            let lexed = leek_lexer::lex(&f.text, f.source, f.version);
            known_classes.extend(leek_parser::scan_class_names(&f.text, &lexed.tokens));
        }
        known_classes.sort();
        known_classes.dedup();
        cx.insert(KnownClassesArtifact(known_classes.clone()));

        // The walker returns every file in topological order, with
        // the entry last. Re-parse each included file so the lower
        // step has ready-to-use ASTs. The entry's own AST stays
        // owned by the existing `Parse` step's artifact.
        let mut includes: Vec<ParsedIncludedFile> = Vec::new();
        for ResolvedFile {
            source,
            path,
            text,
            version,
        } in graph.files
        {
            if path == self.entry_path
                || path.canonicalize().ok().as_deref() == Some(&self.entry_path)
            {
                continue;
            }
            let parsed = parse_file_with_classes(&text, source, version, &known_classes);
            if let Some(sites) = graph.include_sites.get(&path) {
                if parsed.ast.is_none() {
                    for site in sites {
                        cx.emit(leek_diagnostics::Diagnostic::error(
                            leek_diagnostics::codes::INCLUDE_PARSE_FAILED,
                            site.span,
                            format!("included file `{}` failed to parse", path.display()),
                        ));
                    }
                }
            } else if parsed.ast.is_none() {
                cx.emit(leek_diagnostics::Diagnostic::error(
                    leek_diagnostics::codes::INCLUDE_PARSE_FAILED,
                    Span::new(source, 0, 0),
                    format!("included file `{}` failed to parse", path.display()),
                ));
            }
            let Some(ast) = parsed.ast else {
                cx.emit_all(parsed.diagnostics);
                continue;
            };
            cx.emit_all(parsed.diagnostics);
            includes.push(ParsedIncludedFile {
                source,
                path,
                text,
                version,
                ast,
            });
        }

        cx.insert(IncludeGraphArtifact {
            includes,
            resolved: graph.resolved,
            forward: graph.forward,
            entry_path: self.entry_path.clone(),
        });
        Ok(())
    }
}

/// Salsa-tracked entry point for name resolution. Re-runs only when
/// the upstream [`parse_query`](leek_parser::pipeline::parse_query)'s
/// green tree changes or the `strict` flag flips.
#[cfg(feature = "salsa")]
#[salsa::tracked]
pub fn resolve_query(
    db: &dyn leek_pipeline::salsa::Db,
    file: leek_pipeline::salsa::SourceFile,
) -> ResolveArtifact {
    use leek_parser::ast::{AstNode, SourceFile as AstSourceFile};
    use leek_syntax::SyntaxNode;

    let parse = leek_parser::pipeline::parse_query(db, file);
    let Some(ast) = AstSourceFile::cast(SyntaxNode::new_root(parse.green.clone())) else {
        return ResolveArtifact::default();
    };
    let (pragmas, _) = leek_syntax::parse_pragmas(file.text(db), file.source(db));
    let opts = Options {
        strict: file.strict(db),
        experimental_imports: pragmas.experimental.iter().any(|f| f == "imports"),
        experimental_overloads: pragma_overloads(&pragmas)
            || leek_pipeline::FeatureFlags::from_bits(file.flags_bits(db)).overloads,
    };
    let ResolveResult { diagnostics, table } = resolve_collecting(
        &ast,
        file.source(db),
        version_from_byte(file.version_byte(db)),
        opts,
    );
    ResolveArtifact { diagnostics, table }
}
