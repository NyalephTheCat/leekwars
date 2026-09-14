//! `textDocument/prepareRename` — validate the cursor is on a
//! renameable symbol and return its name range, before the
//! editor pops up the rename UI.
//!
//! Returns:
//!  - `Ok(Some(range))` when the cursor is on either a `ResolvedRef`
//!    or a `Symbol::def_span`.
//!  - `Ok(None)` when the cursor is on whitespace, a literal, or a
//!    keyword. Editors show "can't rename here" in that case.
//!  - `Err(Refusal)` when the cursor is on a class member — the rename
//!    would be unsound, and refusing here means the editor says so
//!    before it pops the input box. See [`rename`](super::rename).

use leek_resolver::SymbolKind;
use leek_span::Span;
use leek_syntax::SyntaxNode;
use tower_lsp::lsp_types as lsp;

use crate::handlers::refusal::Refusable;
use crate::util::position::{offset_to_position, position_to_offset, span_to_range};
use crate::workspace::Workspace;

pub fn handle(
    ws: &Workspace,
    uri: &lsp::Url,
    pos: lsp::Position,
) -> Refusable<lsp::PrepareRenameResponse> {
    let Some(doc) = ws.doc(uri) else {
        return Ok(None);
    };
    let Some(offset) = position_to_offset(doc.pos_map(), pos) else {
        return Ok(None);
    };

    let Some(run) = crate::pipeline::run(ws, uri, leek_recipes::Target::Resolved) else {
        return Ok(None);
    };
    let Some(art) = run.get::<leek_resolver::pipeline::ResolveArtifact>() else {
        return Ok(None);
    };
    let table = &art.table;
    let Some(green) = run.get::<leek_parser::pipeline::GreenTreeArtifact>() else {
        return Ok(None);
    };
    let root = SyntaxNode::new_root(green.0.clone());

    // The same member refusal `rename` applies, raised here so clients
    // that honour `prepareRename` never open the input box. Duplicated
    // rather than delegated because some clients skip prepareRename or
    // ignore its error.
    let member_refusal = |sym: &leek_resolver::Symbol| {
        let class = crate::handlers::enclosing_class_name(&root, sym.def_span.start)
            .unwrap_or_else(|| "<anonymous>".to_string());
        crate::handlers::refusal::member_rename(&sym.name, &class)
    };

    // 1. Cursor on a reference: report the reference's own range.
    if let Some(r) = table.reference_at(offset) {
        let span = Span::new(
            doc.source_file_source_id(&ws.db),
            r.name_offset,
            r.name_offset + r.name_len,
        );
        if let Some(target) = table.symbol(r.target) {
            // Refuse to rename through a Builtin target — those are
            // language-defined names, renaming makes no sense.
            if target.kind == SymbolKind::Builtin {
                return Ok(None);
            }
            if crate::handlers::symbol_is_class_member(&root, target) {
                return Err(member_refusal(target));
            }
        }
        return Ok(Some(lsp::PrepareRenameResponse::Range(span_to_range(
            doc.pos_map(),
            span,
        ))));
    }

    // 2. Cursor on a declaration: report the def_span.
    if let Some(sym) = table
        .symbols
        .iter()
        .find(|s| s.def_span.start <= offset && offset < s.def_span.end)
    {
        if sym.kind == SymbolKind::Builtin {
            return Ok(None);
        }
        if crate::handlers::symbol_is_class_member(&root, sym) {
            return Err(member_refusal(sym));
        }
        return Ok(Some(lsp::PrepareRenameResponse::Range(span_to_range(
            doc.pos_map(),
            sym.def_span,
        ))));
    }

    // 3. Cross-file use site: the cursor is on a use of a top-level
    //    symbol declared in an `include`d file (which doesn't resolve
    //    locally). Validate it resolves to a declaration and report the
    //    use-site identifier's own range so the editor allows the rename.
    let Some(target) = crate::handlers::cross_file_use_target(ws, uri, &root, offset) else {
        return Ok(None);
    };
    Ok(Some(lsp::PrepareRenameResponse::Range(lsp::Range {
        start: offset_to_position(doc.pos_map(), target.use_start),
        end: offset_to_position(doc.pos_map(), target.use_end),
    })))
}
