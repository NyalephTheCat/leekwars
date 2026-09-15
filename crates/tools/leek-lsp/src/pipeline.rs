//! What is left of the LSP's pipeline drivers.
//!
//! No handler comes through here any more: every frontend artifact a
//! handler reads is a [`crate::analysis`] accessor, and the diagnostic
//! stream is a whole-program query. The two `run*` functions below exist
//! only so `analysis`'s tests can compare each accessor against the
//! artifact the equivalent recipe produces — the check that makes those
//! rewrites a refactor rather than a second opinion. They go when
//! `Step` does (#99).
//!
//! [`include_folder`] is not part of that and does not go with it: the
//! include walker behind `Workspace::resync` still resolves names
//! through it.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use leek_pipeline::Run;
use leek_pipeline::salsa::SourceFile;
use leek_resolver::folder::{Folder, LoadError, LoadedFile, MemFolder};
use leek_session::{self, Target};
use tower_lsp::lsp_types as lsp;

use crate::workspace::{AnalysisTarget, Workspace};

pub fn run_on_file(ws: &Workspace, source_file: SourceFile, target: Target) -> Option<Run<'_>> {
    let pipeline = leek_session::pipeline(target, &leek_session::lsp_params()).ok()?;
    Some(pipeline.run_memoized(&ws.db, source_file))
}

/// Run a memoized pipeline recipe for an open document.
pub fn run<'db>(ws: &'db Workspace, uri: &lsp::Url, target: Target) -> Option<Run<'db>> {
    let doc = ws.doc(uri)?;
    run_on_file(ws, doc.source_file, target)
}

/// Include resolver for the LSP. Open buffers shadow indexed/disk contents;
/// disk remains a fallback for a file that has not been opened or indexed.
struct WorkspaceFolder {
    memory: MemFolder,
    disk: leek_resolver::folder::DiskFolder,
}

/// The include folder over `targets`.
///
/// Built once per workspace revision by
/// [`Workspace::resync`](crate::workspace::Workspace) rather than once
/// per pipeline run: a diagnostics pass over a project of N files used
/// to build N copies of this map, each one a full copy of every file's
/// text. Each entry now costs a refcount bump, and the canonical path
/// comes off the file's own salsa input instead of being re-derived
/// (and re-canonicalized, a syscall per file) from its URI.
///
/// Files the include walker reaches that are neither open nor indexed
/// are interned into the same [`PathInterner`](leek_resolver::interner::PathInterner)
/// the workspace mints its ids from, so the walker and the workspace
/// agree on every id without either having to re-assign the other's.
pub(crate) fn include_folder(targets: &[AnalysisTarget]) -> Arc<dyn Folder> {
    let mut memory = MemFolder::new();
    for target in targets {
        if target.canonical_path.is_empty() {
            continue;
        }
        memory.insert(
            PathBuf::from(&target.canonical_path),
            Arc::clone(&target.text),
        );
    }
    Arc::new(WorkspaceFolder {
        memory,
        disk: leek_resolver::folder::DiskFolder,
    })
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
