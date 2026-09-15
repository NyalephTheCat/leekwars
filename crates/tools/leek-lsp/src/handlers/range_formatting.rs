//! `textDocument/rangeFormatting` — partial-document formatting.
//!
//! Delegates to [`leek_fmt::format_range`] to pick the smallest CST
//! subtree containing the requested range and format just that
//! subtree. A range spanning several top-level items has no subtree
//! below the root: `format_range` formats the document and narrows
//! the result to the lines that change (#200), so the edit still
//! lands inside the buffer rather than replacing it.
//!
//! An empty edit list means there was nothing to do — no line of the
//! range would change — or that the edit failed `edit_is_safe`.

use leek_span::Span;
use tower_lsp::lsp_types as lsp;

use crate::workspace::Workspace;

pub fn handle(ws: &Workspace, uri: &lsp::Url, range: lsp::Range) -> Option<Vec<lsp::TextEdit>> {
    let doc = ws.doc(uri)?;
    let start = doc.pos_map().to_offset(range.start)?;
    let end = doc.pos_map().to_offset(range.end)?;
    if start > end {
        return Some(Vec::new());
    }

    let green = crate::analysis::green_tree(&ws.db, doc.source_file);

    let (target_range, replacement) = leek_fmt::format_range(
        &green,
        doc.source_file_version(&ws.db),
        &ws.settings.format,
        start..end,
    )?;

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
    if !super::formatting::edit_is_safe(ws, uri, doc, target_range.clone(), &replacement) {
        return Some(Vec::new());
    }

    let span = Span::new(
        doc.source_file_source_id(&ws.db),
        target_range.start,
        target_range.end,
    );
    Some(vec![lsp::TextEdit {
        range: doc.pos_map().span_range(span),
        new_text: replacement,
    }])
}
