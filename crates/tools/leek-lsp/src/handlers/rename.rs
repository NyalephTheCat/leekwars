//! `textDocument/rename` — rename a symbol everywhere it's used.
//!
//! Class members (fields and methods) are **refused** rather than
//! renamed: the resolver records no references for member names, so the
//! occurrence search behind the edit is unsound in both directions. See
//! [`refusal`](super::refusal) and leekwars#46.

use std::collections::HashMap;

use leek_span::Span;
use leek_syntax::SyntaxNode;
use tower_lsp::lsp_types as lsp;

use crate::handlers::refusal::Refusable;
use crate::workspace::Workspace;

pub fn handle(
    ws: &Workspace,
    uri: &lsp::Url,
    pos: lsp::Position,
    new_name: &str,
) -> Refusable<lsp::WorkspaceEdit> {
    let Some(doc) = ws.doc(uri) else {
        return Ok(None);
    };
    let Some(offset) = doc.pos_map().to_offset(pos) else {
        return Ok(None);
    };

    let Some(run) = crate::pipeline::run(ws, uri, leek_session::Target::Resolved) else {
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
        // rename spans every includer. No member check is needed here:
        // `cross_file_use_target` rejects dotted member accesses and
        // only matches top-level declarations.
        let Some(target) = crate::handlers::cross_file_use_target(ws, uri, &root, offset) else {
            return Ok(None);
        };
        return Ok(workspace_rename(
            &target.home_uri,
            &target.name,
            target.kind,
        ));
    };

    // A class member: refuse rather than produce a wrong edit. Both the
    // workspace path below (a method is a `Function`, so it would fan
    // out over same-named free functions) and the single-file path (a
    // field's `this.x` uses are not references) are unsound for members.
    if let Some(sym) = table.symbol(target_id)
        && crate::handlers::symbol_is_class_member(&root, sym)
    {
        let class = crate::handlers::enclosing_class_name(&root, sym.def_span.start)
            .unwrap_or_else(|| "<anonymous>".to_string());
        return Err(crate::handlers::refusal::member_rename(&sym.name, &class));
    }

    // Locals/params/fields are file-scoped and stay single-file
    // (renaming them across files would be wrong).
    if let Some(sym) = table.symbol(target_id)
        && crate::handlers::is_workspace_global(sym.kind)
    {
        return Ok(workspace_rename(uri, &sym.name, sym.kind));
    }

    let mut edits: Vec<lsp::TextEdit> = Vec::new();
    if let Some(sym) = table.symbol(target_id) {
        edits.push(lsp::TextEdit {
            range: doc.pos_map().span_range(sym.def_span),
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
                range: doc.pos_map().span_range(span),
                new_text: new_name.to_string(),
            });
        }
    }

    let mut changes = HashMap::new();
    changes.insert(uri.clone(), edits);
    Ok(Some(lsp::WorkspaceEdit {
        changes: Some(changes),
        document_changes: None,
        change_annotations: None,
    }))
}
