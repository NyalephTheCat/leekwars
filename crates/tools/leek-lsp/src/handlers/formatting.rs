//! `textDocument/formatting` — full-document formatting via
//! [`leek_fmt`].

use std::ops::Range;

use leek_fmt::pipeline::FormattedArtifact;
use leek_syntax::Version;
use tower_lsp::lsp_types as lsp;

use crate::documents::DocHandle;
use crate::workspace::Workspace;

/// Run the formatter and return a single full-document [`TextEdit`]
/// covering `(0,0)..end-of-doc`.
///
/// Returns `Some(empty Vec)` when the formatted output already
/// matches the buffer (no edit needed), or when the output fails the
/// [`leek_fmt::check_equivalence`] safety net — a formatter bug must
/// never delete the user's comments or code. Returns `None` only if
/// the document is unknown to the workspace.
pub fn handle(ws: &Workspace, uri: &lsp::Url) -> Option<Vec<lsp::TextEdit>> {
    let doc = ws.doc(uri)?;
    let run = crate::pipeline::run_formatted(ws, uri, ws.settings.format.clone())?;

    let formatted = run.get::<FormattedArtifact>()?.0.as_ref().clone();
    let original = doc.text.as_ref();

    if formatted == original {
        return Some(Vec::new());
    }
    if let Err(err) = leek_fmt::check_equivalence(original, &formatted, doc_version(ws, doc)) {
        eprintln!("leek-lsp: refusing to format {uri}: {err}");
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

/// True when replacing the byte `range` of `doc` with `replacement`
/// keeps every comment and significant token (see
/// [`leek_fmt::check_edit_equivalence`]). Range and on-type formatting
/// drop edits that fail it, logging why.
pub(crate) fn edit_is_safe(
    ws: &Workspace,
    uri: &lsp::Url,
    doc: &DocHandle,
    range: Range<u32>,
    replacement: &str,
) -> bool {
    match leek_fmt::check_edit_equivalence(&doc.text, range, replacement, doc_version(ws, doc)) {
        Ok(()) => true,
        Err(err) => {
            eprintln!("leek-lsp: refusing to format {uri}: {err}");
            false
        }
    }
}

/// The language version `doc` was parsed with.
fn doc_version(ws: &Workspace, doc: &DocHandle) -> Version {
    leek_syntax::pipeline::version_from_byte(doc.source_file.version_byte(&ws.db))
}
