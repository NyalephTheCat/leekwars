//! `textDocument/prepareRename` — validate the cursor is on a
//! renameable symbol and return its name range, before the
//! editor pops up the rename UI.
//!
//! Returns:
//!  - `Some(range)` when the cursor is on either a `ResolvedRef`
//!    or a `Symbol::def_span`.
//!  - `None` when the cursor is on whitespace, a literal, or a
//!    keyword. Editors show "can't rename here" in that case.

use leek_resolver::SymbolKind;
use leek_span::Span;
use leek_syntax::SyntaxNode;
use tower_lsp::lsp_types as lsp;

use crate::util::position::{offset_to_position, position_to_offset, span_to_range};
use crate::workspace::Workspace;

pub fn handle(
    ws: &Workspace,
    uri: &lsp::Url,
    pos: lsp::Position,
) -> Option<lsp::PrepareRenameResponse> {
    let doc = ws.doc(uri)?;
    let offset = position_to_offset(doc.pos_map(), pos)?;

    let run = crate::pipeline::run(ws, uri, leek_recipes::Target::Resolved)?;
    let table = &run.get::<leek_resolver::pipeline::ResolveArtifact>()?.table;

    // 1. Cursor on a reference: report the reference's own range.
    if let Some(r) = table.reference_at(offset) {
        let span = Span::new(
            doc.source_file_source_id(&ws.db),
            r.name_offset,
            r.name_offset + r.name_len,
        );
        // Refuse to rename through a Builtin target — those are
        // language-defined names, renaming makes no sense.
        if let Some(target) = table.symbol(r.target)
            && target.kind == SymbolKind::Builtin {
                return None;
            }
        return Some(lsp::PrepareRenameResponse::Range(span_to_range(
            doc.pos_map(),
            span,
        )));
    }

    // 2. Cursor on a declaration: report the def_span.
    if let Some(sym) = table
        .symbols
        .iter()
        .find(|s| s.def_span.start <= offset && offset < s.def_span.end)
    {
        if sym.kind == SymbolKind::Builtin {
            return None;
        }
        return Some(lsp::PrepareRenameResponse::Range(span_to_range(
            doc.pos_map(),
            sym.def_span,
        )));
    }

    // 3. Cross-file use site: the cursor is on a use of a top-level
    //    symbol declared in an `include`d file (which doesn't resolve
    //    locally). Validate it resolves to a declaration and report the
    //    use-site identifier's own range so the editor allows the rename.
    let green = &run.get::<leek_parser::pipeline::GreenTreeArtifact>()?.0;
    let root = SyntaxNode::new_root(green.clone());
    let target = crate::handlers::cross_file_use_target(ws, uri, &root, offset)?;
    Some(lsp::PrepareRenameResponse::Range(lsp::Range {
        start: offset_to_position(doc.pos_map(), target.use_start),
        end: offset_to_position(doc.pos_map(), target.use_end),
    }))
}
