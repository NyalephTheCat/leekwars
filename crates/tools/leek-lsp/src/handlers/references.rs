//! `textDocument/references` — find all references to the symbol at
//! the cursor.

use leek_span::Span;
use tower_lsp::lsp_types as lsp;

use crate::util::position::{position_to_offset, span_to_range};
use crate::workspace::Workspace;

pub fn handle(
    ws: &Workspace,
    uri: &lsp::Url,
    pos: lsp::Position,
    include_declaration: bool,
) -> Option<Vec<lsp::Location>> {
    let doc = ws.doc(uri)?;
    let offset = position_to_offset(doc.pos_map(), pos)?;

    let run = crate::pipeline::run(ws, uri, leek_recipes::Target::Resolved)?;
    let table = &run.get::<leek_resolver::pipeline::ResolveArtifact>()?.table;

    // Program-wide occurrences of a top-level symbol → Locations,
    // honouring the `includeDeclaration` flag.
    let to_locs = |home: &lsp::Url, name: &str, kind| {
        crate::handlers::workspace_occurrences(ws, home, name, kind)
            .into_iter()
            .filter(|o| include_declaration || !o.is_declaration)
            .map(|o| lsp::Location {
                uri: o.uri,
                range: o.range,
            })
            .collect::<Vec<_>>()
    };

    // The cursor resolves locally: either a ref's target or a symbol
    // whose def_span covers it.
    if let Some(target_id) = crate::handlers::resolve_symbol_id(table, offset) {
        // Top-level functions/classes/globals share one flat namespace
        // across `include`d files, so their references span the program.
        // Locals/params/fields are file-scoped — single-file path below.
        if let Some(sym) = table.symbol(target_id)
            && crate::handlers::is_workspace_global(sym.kind)
        {
            return Some(to_locs(uri, &sym.name, sym.kind));
        }

        let mut out: Vec<lsp::Location> = Vec::new();
        if include_declaration
            && let Some(sym) = table.symbol(target_id)
        {
            out.push(loc(uri, doc.pos_map(), sym.def_span));
        }
        for r in &table.references {
            if r.target == target_id {
                let span = Span::new(
                    doc.source_file_source_id(&ws.db),
                    r.name_offset,
                    r.name_offset + r.name_len,
                );
                out.push(loc(uri, doc.pos_map(), span));
            }
        }
        return Some(out);
    }

    // The cursor didn't resolve locally — it may be a *use* of a
    // top-level symbol declared in an `include`d file. Anchor the search
    // on the declaration's file so it spans every includer.
    let green = &run.get::<leek_parser::pipeline::GreenTreeArtifact>()?.0;
    let root = leek_syntax::SyntaxNode::new_root(green.clone());
    let target = crate::handlers::cross_file_use_target(ws, uri, &root, offset)?;
    Some(to_locs(&target.home_uri, &target.name, target.kind))
}

fn loc(uri: &lsp::Url, pm: crate::util::position::PosMap<'_>, span: Span) -> lsp::Location {
    lsp::Location {
        uri: uri.clone(),
        range: span_to_range(pm, span),
    }
}
