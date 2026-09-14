//! Shared pipeline drivers for LSP handlers.

use std::path::Path;
use std::sync::Arc;

use leek_fmt::FormatOptions;
use leek_pipeline::Run;
use leek_pipeline::salsa::SourceFile;
use leek_resolver::folder::{Folder, LoadError, LoadedFile, MemFolder};
use leek_resolver::interner::SourceInterner;
use leek_session::{self, Target};
use leek_span::paths::canonical_or_normalized;
use tower_lsp::lsp_types as lsp;

use crate::workspace::Workspace;

pub fn run_on_file(ws: &Workspace, source_file: SourceFile, target: Target) -> Option<Run<'_>> {
    let pipeline = leek_session::pipeline(target, &leek_session::lsp_params()).ok()?;
    Some(pipeline.run_memoized(&ws.db, source_file))
}

/// Run the include-aware pipeline for a workspace-owned source file.
///
/// Navigation handlers still use [`run_on_file`] because they deliberately
/// analyze one document and bridge cross-file results themselves. Diagnostics
/// need the compiler's shared program scope, so they opt into this path.
pub fn run_on_file_with_includes(
    ws: &Workspace,
    source_file: SourceFile,
    target: Target,
) -> Option<Run<'_>> {
    let source = source_file.source(&ws.db);
    let uri = ws
        .analysis_targets()
        .into_iter()
        .find(|t| t.source_file.source(&ws.db) == source)
        .map(|t| t.uri.clone());
    match uri {
        Some(uri) => run_on_uri(ws, &uri, source_file, target),
        None => run_on_file(ws, source_file, target),
    }
}

/// Run an include-aware LSP pipeline for one URI.
fn run_on_uri<'db>(
    ws: &'db Workspace,
    uri: &lsp::Url,
    source_file: SourceFile,
    target: Target,
) -> Option<Run<'db>> {
    let Some(entry_path) = crate::workspace::uri_to_path(uri) else {
        let pipeline = leek_session::pipeline(target, &leek_session::lsp_params()).ok()?;
        return Some(pipeline.run_memoized(&ws.db, source_file));
    };

    let folder = WorkspaceFolder::from_workspace(ws);
    let includes = leek_resolver::pipeline::ResolveIncludes::new(
        folder,
        canonical_or_normalized(&entry_path),
        Arc::clone(&ws.interner) as Arc<dyn SourceInterner>,
    );
    let pipeline = leek_session::pipeline_with_includes(
        target,
        Box::new(includes),
        &leek_session::lsp_params(),
    )
    .ok()?;
    Some(pipeline.run_memoized(&ws.db, source_file))
}

/// Run a memoized pipeline recipe for an open document.
pub fn run<'db>(ws: &'db Workspace, uri: &lsp::Url, target: Target) -> Option<Run<'db>> {
    let doc = ws.doc(uri)?;
    run_on_file(ws, doc.source_file, target)
}

/// Run parse + format with the given options.
pub fn run_formatted<'db>(
    ws: &'db Workspace,
    uri: &lsp::Url,
    opts: FormatOptions,
) -> Option<Run<'db>> {
    let doc = ws.doc(uri)?;
    let pipeline = leek_session::pipeline_formatted(opts, &leek_session::lsp_params()).ok()?;
    Some(pipeline.run_memoized(&ws.db, doc.source_file))
}

/// Include resolver for the LSP. Open buffers shadow indexed/disk contents;
/// disk remains a fallback for a file that has not been opened or indexed.
struct WorkspaceFolder {
    memory: MemFolder,
    disk: leek_resolver::folder::DiskFolder,
}

impl WorkspaceFolder {
    /// The folder for one include-aware run, and — as a side effect —
    /// the workspace interner seeded with every analysis target's id.
    ///
    /// The seeding is what keeps the include graph naming an open or
    /// indexed file by the `SourceId` its salsa input already carries:
    /// [`run_on_file_with_includes`] maps a source back to a URI by
    /// exactly that id. Files the walker reaches that are neither open
    /// nor indexed get fresh ids from the same interner, so they cannot
    /// collide with an id the workspace hands out later.
    fn from_workspace(ws: &Workspace) -> Arc<dyn Folder> {
        let mut memory = MemFolder::new();
        for target in ws.analysis_targets() {
            let Some(path) = crate::workspace::uri_to_path(target.uri) else {
                continue;
            };
            let path = canonical_or_normalized(&path);
            memory.insert(path.clone(), target.text.to_string());
            ws.interner.assign(&path, target.source_file.source(&ws.db));
        }
        Arc::new(Self {
            memory,
            disk: leek_resolver::folder::DiskFolder,
        })
    }
}

impl Folder for WorkspaceFolder {
    fn load(&self, includer: &Path, name: &str) -> Result<LoadedFile, LoadError> {
        match self.memory.load(includer, name) {
            Ok(file) => Ok(file),
            Err(LoadError::NotFound) => self.disk.load(includer, name),
            Err(error) => Err(error),
        }
    }
}
