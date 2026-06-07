//! `textDocument/rename` — rename a symbol everywhere it's used.

use std::collections::HashMap;

use leek_span::Span;
use tower_lsp::lsp_types as lsp;

use crate::util::position::{position_to_offset, span_to_range};
use crate::workspace::Workspace;

pub fn handle(
    ws: &Workspace,
    uri: &lsp::Url,
    pos: lsp::Position,
    new_name: &str,
) -> Option<lsp::WorkspaceEdit> {
    let doc = ws.doc(uri)?;
    let offset = position_to_offset(doc.pos_map(), pos)?;

    let run = crate::pipeline::run(ws, uri, leek_recipes::Target::Resolved)?;
    let table = &run.get::<leek_resolver::pipeline::ResolveArtifact>()?.table;

    // Rename a top-level symbol everywhere it's used across the program.
    // Top-level functions/classes/globals share one flat namespace
    // across `include`d files, so the edit must reach every includer.
    let workspace_rename = |home: &lsp::Url, name: &str, kind| {
        let mut changes: HashMap<lsp::Url, Vec<lsp::TextEdit>> = HashMap::new();
        for occ in crate::handlers::workspace_occurrences(ws, home, name, kind) {
            changes.entry(occ.uri).or_default().push(lsp::TextEdit {
                range: occ.range,
                new_text: new_name.to_string(),
            });
        }
        if changes.is_empty() {
            None
        } else {
            Some(lsp::WorkspaceEdit {
                changes: Some(changes),
                document_changes: None,
                change_annotations: None,
            })
        }
    };

    let local_target = table.reference_at(offset).map(|r| r.target).or_else(|| {
        table
            .symbols
            .iter()
            .find(|s| s.def_span.start <= offset && offset < s.def_span.end)
            .map(|s| s.id)
    });

    let Some(target_id) = local_target else {
        // Not resolved locally — a *use* of a top-level symbol declared
        // in an `include`d file. Anchor on the declaration's file so the
        // rename spans every includer.
        let green = &run.get::<leek_parser::pipeline::GreenTreeArtifact>()?.0;
        let root = leek_syntax::SyntaxNode::new_root(green.clone());
        let target = crate::handlers::cross_file_use_target(ws, uri, &root, offset)?;
        return workspace_rename(&target.home_uri, &target.name, target.kind);
    };

    // Locals/params/fields are file-scoped and stay single-file
    // (renaming them across files would be wrong).
    if let Some(sym) = table.symbol(target_id)
        && crate::handlers::is_workspace_global(sym.kind)
    {
        return workspace_rename(uri, &sym.name, sym.kind);
    }

    let mut edits: Vec<lsp::TextEdit> = Vec::new();
    if let Some(sym) = table.symbol(target_id) {
        edits.push(lsp::TextEdit {
            range: span_to_range(doc.pos_map(), sym.def_span),
            new_text: new_name.to_string(),
        });
    }
    for r in &table.references {
        if r.target == target_id {
            let span = Span::new(
                doc.source_file_source_id(&ws.db),
                r.name_offset,
                r.name_offset + r.name_len,
            );
            edits.push(lsp::TextEdit {
                range: span_to_range(doc.pos_map(), span),
                new_text: new_name.to_string(),
            });
        }
    }

    let mut changes = HashMap::new();
    changes.insert(uri.clone(), edits);
    Some(lsp::WorkspaceEdit {
        changes: Some(changes),
        document_changes: None,
        change_annotations: None,
    })
}
