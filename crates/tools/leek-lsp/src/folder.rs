//! The include folder the workspace resolves names through.
//!
//! Every frontend answer a handler reads is a [`crate::analysis`]
//! accessor and the diagnostic stream is a whole-program query, so this
//! is all the include machinery the server owns: the folder behind
//! `Workspace::resync`'s walker, which has to see open buffers shadow
//! what is on disk.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use leek_resolver::folder::{Folder, LoadError, LoadedFile, MemFolder};

use crate::workspace::AnalysisTarget;

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
/// per include walk: a diagnostics pass over a project of N files used
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
