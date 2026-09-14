//! Pipeline integration: resolver as a [`Step`].

use std::path::PathBuf;
use std::sync::Arc;

use leek_diagnostics::Diagnostic;
use leek_parser::ast::{AstNode, SourceFile};
use leek_parser::pipeline::{AstArtifact, KnownClassesArtifact};
use leek_pipeline::{Artifact, Context, Step, StepError};
use leek_pipeline::{RecipeArtifact, RecipeParams, RecipeStep};
use leek_syntax::pipeline::PragmasArtifact;
use leek_syntax::version::version_from_byte;
use leek_syntax::{SyntaxNode, Version};

use crate::closure::{ClosureFile, IncludeClosure, resolve_include_closure};
use crate::folder::Folder;
use crate::index::ResolveTable;
use crate::interner::SourceInterner;
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
    pub text: Arc<str>,
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

/// The red-tree view of an [`IncludeClosure`].
///
/// The closure is the value — green trees, safe to memoize and to move
/// between threads — and this artifact is the cursor over it that the
/// AST-walking passes (HIR lowering, resolution, type checking) need.
/// Casting here rather than inside the closure keeps every `SyntaxNode`
/// on the side of the boundary a tracked query can never cross.
impl From<IncludeClosure> for IncludeGraphArtifact {
    fn from(closure: IncludeClosure) -> Self {
        let IncludeClosure {
            entry_path,
            files,
            resolved,
            forward,
            ..
        } = closure;
        Self {
            includes: files
                .into_iter()
                .map(|file| {
                    let ClosureFile {
                        source,
                        path,
                        text,
                        version,
                        green,
                    } = file;
                    ParsedIncludedFile {
                        source,
                        path,
                        text,
                        version,
                        ast: SourceFile::cast(SyntaxNode::new_root(green))
                            .expect("grammar::source_file always opens a SourceFile root"),
                    }
                })
                .collect(),
            resolved,
            forward,
            entry_path,
        }
    }
}

/// Pipeline step that walks `include("…")` calls transitively
/// using the provided [`Folder`].
///
/// Add this before [`leek_hir::pipeline::LowerHir`] (or its
/// future multi-file variant) so the lowerer has every file's
/// parsed AST in the [`IncludeGraphArtifact`]. Without this step
/// the existing single-file flow runs unchanged.
///
/// `interner` issues the `SourceId`s: the entry file first (the
/// walker seeds it), then each newly-discovered include. Callers that
/// compile several entry files in one run — the LSP over a workspace,
/// `miku test` over a tests directory — hand every pipeline the *same*
/// interner, so a helper included by two entries keeps one id instead
/// of colliding with the next entry's. One-shot callers build a fresh
/// [`PathInterner`](crate::interner::PathInterner) per run.
pub struct ResolveIncludes {
    pub folder: Arc<dyn Folder>,
    pub entry_path: PathBuf,
    pub interner: Arc<dyn SourceInterner>,
}

impl ResolveIncludes {
    /// The step for `entry_path`, numbering it and its includes out of
    /// `interner`.
    #[must_use]
    pub fn new(
        folder: Arc<dyn Folder>,
        entry_path: PathBuf,
        interner: Arc<dyn SourceInterner>,
    ) -> Self {
        Self {
            folder,
            entry_path,
            interner,
        }
    }
}

impl Step for ResolveIncludes {
    fn name(&self) -> &'static str {
        "resolve_includes"
    }
    fn run(&self, cx: &mut Context<'_>) -> Result<(), StepError> {
        let (closure, diagnostics) = resolve_include_closure(
            &self.entry_path,
            cx.text(),
            version_from_byte(cx.version_byte()),
            &*self.folder,
            &*self.interner,
            cx.flags(),
        );
        cx.emit_all(diagnostics);
        // Published for the `Parse` step, which runs *after* this one
        // when the pipeline is include-aware: upstream resolves potential
        // type words against the program-wide defined-class set, so the
        // entry must parse `lowercaseClassFromInclude x = …` as a typed
        // declaration.
        cx.insert(KnownClassesArtifact(closure.class_names.clone()));
        cx.insert(IncludeGraphArtifact::from(closure));
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
    // The dynamically-registered builtins are still a process-global, and
    // this reads it — untracked — from inside a tracked query. What changed
    // is the *frequency*: one read here, at the top of the query body,
    // instead of one lock per unresolved name during the walk. A later slice
    // takes the registry off the salsa input instead, so registering a
    // builtin invalidates the memo rather than being silently missed by it.
    let builtins = crate::builtins::snapshot_dynamic_builtins();
    let opts = Options::from_settings(
        Some(&pragmas),
        leek_pipeline::FeatureFlags::from_bits(file.flags_bits(db)),
        file.strict(db),
    )
    .with_builtins(builtins);
    let ResolveResult { diagnostics, table } = resolve_collecting(
        &ast,
        file.source(db),
        version_from_byte(file.version_byte(db)),
        opts,
    );
    ResolveArtifact { diagnostics, table }
}
