//! Per-open-document state.

use std::sync::Arc;

use leek_pipeline::salsa::{Db, SourceFile};
use leek_span::{LineTable, SourceId};

/// Everything we need per open document: the salsa input handle
/// (which carries the source text and lives in the workspace's
/// [`LeekDb`](leek_pipeline::salsa::LeekDb)), plus the cached
/// `LineTable` we use to translate LSP positions to byte offsets.
pub struct DocHandle {
    pub source_file: SourceFile,
    pub line_table: LineTable,
    /// In-memory snapshot of the current text. Cheaper than going
    /// through `source_file.text(db)` when the workspace mutex would
    /// otherwise need to be held.
    pub text: Arc<str>,
    /// The client's version number for this buffer, echoed back on
    /// `publishDiagnostics` so the editor can discard a diagnostic set
    /// computed against a stale revision (publishes can race when edits
    /// arrive faster than analysis completes).
    pub version: i32,
}

impl DocHandle {
    /// Convenience: fetch this document's [`SourceId`] from the
    /// salsa input. Needed when constructing a [`Span`] from a
    /// `(start, end)` pair off the resolver's reference table.
    pub fn source_file_source_id(&self, db: &dyn Db) -> SourceId {
        self.source_file.source(db)
    }

    /// UTF-16-aware position map (line table + source text) for converting
    /// between LSP positions and byte offsets.
    pub fn pos_map(&self) -> crate::util::position::PosMap<'_> {
        crate::util::position::PosMap::new(&self.line_table, &self.text)
    }
}
