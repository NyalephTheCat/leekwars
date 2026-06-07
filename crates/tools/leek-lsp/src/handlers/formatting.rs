//! `textDocument/formatting` — full-document formatting via
//! [`leek_fmt`].

use leek_fmt::pipeline::FormattedArtifact;
use tower_lsp::lsp_types as lsp;

use crate::workspace::Workspace;

/// Run the formatter and return a single full-document [`TextEdit`]
/// covering `(0,0)..end-of-doc`.
///
/// Returns `Some(empty Vec)` when the formatted output already
/// matches the buffer (no edit needed). Returns `None` only if the
/// document is unknown to the workspace.
pub fn handle(ws: &Workspace, uri: &lsp::Url) -> Option<Vec<lsp::TextEdit>> {
    let doc = ws.doc(uri)?;
    let run = crate::pipeline::run_formatted(ws, uri, ws.settings.format.clone())?;

    let formatted = run.get::<FormattedArtifact>()?.0.as_ref().clone();
    let original = doc.text.as_ref();

    if formatted == original {
        return Some(Vec::new());
    }

    // End position of the original document. `LineTable::line_col`
    // returns 1-indexed line/col; LSP wants 0-indexed.
    let end_offset = leek_span::offset(original.len());
    let end = doc.line_table.line_col(end_offset);
    let end_pos = lsp::Position {
        line: end.line.saturating_sub(1),
        character: end.col.saturating_sub(1),
    };

    Some(vec![lsp::TextEdit {
        range: lsp::Range {
            start: lsp::Position {
                line: 0,
                character: 0,
            },
            end: end_pos,
        },
        new_text: formatted,
    }])
}
