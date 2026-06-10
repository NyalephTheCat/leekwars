//! `textDocument/rangeFormatting` — partial-document formatting.
//!
//! Delegates to [`leek_fmt::format_range`] to pick the smallest CST
//! subtree containing the requested range and format just that
//! subtree. If no usable subtree exists (the range spans multiple
//! top-level items, for example), returns an empty edit list — the
//! client can fall back to full-document formatting.

use leek_span::Span;
use tower_lsp::lsp_types as lsp;

use crate::util::position::{position_to_offset, span_to_range};
use crate::workspace::Workspace;

pub fn handle(ws: &Workspace, uri: &lsp::Url, range: lsp::Range) -> Option<Vec<lsp::TextEdit>> {
    let doc = ws.doc(uri)?;
    let start = position_to_offset(doc.pos_map(), range.start)?;
    let end = position_to_offset(doc.pos_map(), range.end)?;
    if start > end {
        return Some(Vec::new());
    }

    let run = crate::pipeline::run(ws, uri, leek_recipes::Target::Parsed)?;
    let green = &run.get::<leek_parser::pipeline::GreenTreeArtifact>()?.0;

    let (target_range, replacement) =
        leek_fmt::format_range(green, &ws.settings.format, start..end)?;

    // If the replacement matches the original, no edit needed.
    // `target_range` comes from the green tree, which can desync from
    // `doc.text` (stale cache, mid-edit). Bound-check rather than index
    // blindly: a bad range falls back to no edit instead of panicking
    // and crashing the language server.
    let original = &doc.text;
    let original_slice = original.get(target_range.start as usize..target_range.end as usize)?;
    if original_slice == replacement {
        return Some(Vec::new());
    }

    let span = Span::new(
        doc.source_file_source_id(&ws.db),
        target_range.start,
        target_range.end,
    );
    Some(vec![lsp::TextEdit {
        range: span_to_range(doc.pos_map(), span),
        new_text: replacement,
    }])
}
