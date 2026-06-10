//! Shared pipeline drivers for LSP handlers.

use leek_fmt::FormatOptions;
use leek_pipeline::Run;
use leek_pipeline::salsa::SourceFile;
use leek_recipes::{self, Target};
use tower_lsp::lsp_types as lsp;

use crate::workspace::Workspace;

pub fn run_on_file(ws: &Workspace, source_file: SourceFile, target: Target) -> Option<Run<'_>> {
    let pipeline = leek_recipes::pipeline(target, &leek_recipes::lsp_params()).ok()?;
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
    let pipeline = leek_recipes::pipeline_formatted(opts, &leek_recipes::lsp_params()).ok()?;
    Some(pipeline.run_memoized(&ws.db, doc.source_file))
}
