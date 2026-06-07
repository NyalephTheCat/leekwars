//! `textDocument/definition` — jump to a name's declaration.
//!
//! Resolves in two tiers: first the cursor file's own symbol table
//! (locals, params, fields, and any top-level symbol declared in this
//! file), then — when that misses — the wider *program* the file
//! belongs to, so a call to a function/class/global defined in an
//! `include`d file jumps to the right file. The LSP resolves each file
//! in isolation, so cross-file references never bind locally; the
//! program-scope search closes that gap.

use leek_syntax::SyntaxNode;
use tower_lsp::lsp_types as lsp;

use crate::util::position::{PosMap, position_to_offset, span_to_range};
use crate::workspace::Workspace;

pub fn handle(
    ws: &Workspace,
    uri: &lsp::Url,
    pos: lsp::Position,
) -> Option<lsp::GotoDefinitionResponse> {
    let doc = ws.doc(uri)?;
    let offset = position_to_offset(doc.pos_map(), pos)?;

    let run = crate::pipeline::run(ws, uri, leek_recipes::Target::Resolved)?;
    let table = &run.get::<leek_resolver::pipeline::ResolveArtifact>()?.table;

    // 1. Same-file: the cursor may be on a use OR on a declaration
    //    itself. Try the references list first, then fall back to a
    //    symbol whose `def_span` covers the cursor.
    if let Some(sym) = crate::handlers::resolve_symbol(table, offset) {
        let range = span_to_range(doc.pos_map(), sym.def_span);
        return Some(lsp::GotoDefinitionResponse::Scalar(lsp::Location {
            uri: uri.clone(),
            range,
        }));
    }

    // 2. Cross-file: an unresolved top-level identifier may be declared
    //    in an included file. Recover the name and search the program.
    let green = &run.get::<leek_parser::pipeline::GreenTreeArtifact>()?.0;
    let root = SyntaxNode::new_root(green.clone());
    let name = crate::handlers::ident_name_at(&root, offset)?;
    let (file, sym) = crate::handlers::find_top_level_decl(ws, uri, &name)?;

    let text = file.source_file.text(&ws.db);
    let line_table = leek_span::LineTable::new(text);
    let range = span_to_range(PosMap::new(&line_table, text), sym.def_span);
    Some(lsp::GotoDefinitionResponse::Scalar(lsp::Location {
        uri: file.uri,
        range,
    }))
}
