//! Pipeline integration: resolver as a [`Step`].

use std::path::PathBuf;
use std::sync::Arc;

use leek_diagnostics::Diagnostic;
use leek_parser::ast::SourceFile;
use leek_parser::pipeline::{AstArtifact, KnownClassesArtifact, parse_file_with_classes};
use leek_pipeline::{Artifact, Context, Step, StepError};
use leek_pipeline::{RecipeArtifact, RecipeParams, RecipeStep};
use leek_span::Span;
use leek_span::paths::canonical_or_normalized;
use leek_syntax::Version;
use leek_syntax::pipeline::PragmasArtifact;
use leek_syntax::version::version_from_byte;

use crate::folder::Folder;
use crate::include_graph::{ResolvedFile, build_include_graph};
use crate::index::ResolveTable;
use crate::{FileUnit, Options, ResolveResult, resolve_collecting, resolve_collecting_files};

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
    // Include-aware runs must bypass the single-file salsa query: the graph
    // is a per-run artifact containing open-buffer/disk snapshots, and the
    // resolver needs to walk all of those ASTs in one shared scope.
    if let Some(graph) = cx.get::<IncludeGraphArtifact>()
        && !graph.includes.is_empty()
        && let Some(entry) = cx.get::<AstArtifact>().map(|a| &a.0)
    {
        let mut files: Vec<FileUnit<'_>> = graph
            .includes
            .iter()
            .map(|file| FileUnit {
                ast: &file.ast,
                source: file.source,
                version: file.version,
                path: &file.path,
            })
            .collect();
        files.push(FileUnit {
            ast: entry,
            source: cx.source(),
            version: version_from_byte(cx.version_byte()),
            path: &graph.entry_path,
        });
        return resolve_collecting_files(&files, Some(&graph.resolved), resolve_options(cx));
    }
    #[cfg(feature = "salsa")]
    if let Some((db, file)) = cx.salsa() {
        let art = resolve_query(db, file);
        return ResolveResult {
            diagnostics: art.diagnostics,
            table: art.table,
        };
    }
    let Some(ast) = cx.get::<AstArtifact>().map(|a| a.0.clone()) else {
        return ResolveResult::default();
    };
    resolve_collecting(
        &ast,
        cx.source(),
        version_from_byte(cx.version_byte()),
        resolve_options(cx),
    )
}

fn resolve_options(cx: &Context<'_>) -> Options {
    Options::from_settings(
        cx.get::<PragmasArtifact>().map(|p| &p.0),
        cx.flags(),
        cx.strict(),
    )
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
        let entry_path = canonical_or_normalized(&self.entry_path);
        let graph = {
            let mut alloc = self.source_allocator.lock().map_err(|e| StepError {
                step: "resolve_includes",
                message: format!("source allocator poisoned: {e}"),
            })?;
            build_include_graph(
                &entry_path,
                cx.text(),
                version_from_byte(cx.version_byte()),
                &*self.folder,
                |p| (alloc)(p),
            )
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
            if path == entry_path {
                continue;
            }
            let parsed = parse_file_with_classes(&text, source, version, &known_classes);
            // The parse always yields a tree — error recovery builds
            // `ErrorNode`s inside the `SourceFile` root rather than failing
            // the root cast — so a broken include is only visible in the
            // diagnostics. Report the chain at the `include(...)` site too,
            // otherwise the entry file's author sees errors pointing only
            // into a file they may not have open. Errors only: a lint in an
            // included file must not mark the include site.
            if parsed
                .diagnostics
                .iter()
                .any(|d| d.severity == leek_diagnostics::Severity::Error)
            {
                match graph.include_sites.get(&path) {
                    Some(sites) => {
                        for site in sites {
                            cx.emit(leek_diagnostics::diag!(
                                leek_diagnostics::codes::INCLUDE_PARSE_FAILED,
                                site.span,
                                "included file `{}` failed to parse",
                                path.display(),
                            ));
                        }
                    }
                    None => cx.emit(leek_diagnostics::diag!(
                        leek_diagnostics::codes::INCLUDE_PARSE_FAILED,
                        Span::new(source, 0, 0),
                        "included file `{}` failed to parse",
                        path.display(),
                    )),
                }
            }
            cx.emit_all(parsed.diagnostics);
            includes.push(ParsedIncludedFile {
                source,
                path,
                text,
                version,
                ast: parsed.ast,
            });
        }

        cx.insert(IncludeGraphArtifact {
            includes,
            resolved: graph.resolved,
            forward: graph.forward,
            entry_path,
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
    // Pragmas only contribute experimental opt-ins here; the version and
    // strict mode come from the salsa input (the settled `Input`). Reuse the
    // memoized pragma query instead of re-scanning the text.
    let pragmas = leek_syntax::pipeline::pragma_query(db, file).pragmas;
    let opts = Options::from_settings(
        Some(&pragmas),
        leek_pipeline::FeatureFlags::from_bits(file.flags_bits(db)),
        file.strict(db),
    );
    let ResolveResult { diagnostics, table } = resolve_collecting(
        &ast,
        file.source(db),
        version_from_byte(file.version_byte(db)),
        opts,
    );
    ResolveArtifact { diagnostics, table }
}
