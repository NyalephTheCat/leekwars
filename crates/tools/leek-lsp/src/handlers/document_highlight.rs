//! `textDocument/documentHighlight` — when the cursor sits on a
//! symbol, highlight the declaration and every in-file reference
//! to that symbol.
//!
//! Same lookup pipeline as
//! [`references`](super::references): find the cursor's resolved
//! reference (or, failing that, a declaration whose `def_span`
//! covers the cursor) and produce a [`lsp::DocumentHighlight`] for
//! the def plus each `ResolvedRef` that targets it. The def is
//! tagged `Write` (mutated by the declaration) and reference sites
//! are `Read`.

use leek_span::Span;
use tower_lsp::lsp_types as lsp;

use crate::util::position::{position_to_offset, span_to_range};
use crate::workspace::Workspace;

pub fn handle(
    ws: &Workspace,
    uri: &lsp::Url,
    pos: lsp::Position,
) -> Option<Vec<lsp::DocumentHighlight>> {
    let doc = ws.doc(uri)?;
    let offset = position_to_offset(doc.pos_map(), pos)?;

    let run = crate::pipeline::run(ws, uri, leek_recipes::Target::Resolved)?;
    let table = &run.get::<leek_resolver::pipeline::ResolveArtifact>()?.table;

    // Resolved locally: highlight the declaration (Write) and every
    // in-file reference (Read) — the complete set for this document.
    if let Some(target_id) = crate::handlers::resolve_symbol_id(table, offset) {
        let mut out: Vec<lsp::DocumentHighlight> = Vec::new();
        if let Some(sym) = table.symbol(target_id) {
            out.push(lsp::DocumentHighlight {
                range: span_to_range(doc.pos_map(), sym.def_span),
                kind: Some(lsp::DocumentHighlightKind::WRITE),
            });
        }
        for r in &table.references {
            if r.target == target_id {
                let span = Span::new(
                    doc.source_file_source_id(&ws.db),
                    r.name_offset,
                    r.name_offset + r.name_len,
                );
                out.push(lsp::DocumentHighlight {
                    range: span_to_range(doc.pos_map(), span),
                    kind: Some(lsp::DocumentHighlightKind::READ),
                });
            }
        }
        return Some(out);
    }

    // Unresolved: the cursor may be on a *use* of a top-level symbol
    // declared in an `include`d file. `documentHighlight` is
    // document-local, so scan only this file for the symbol's uses
    // (highlighting nothing if the name doesn't name a known symbol).
    let green = &run.get::<leek_parser::pipeline::GreenTreeArtifact>()?.0;
    let root = leek_syntax::SyntaxNode::new_root(green.clone());
    let name = crate::handlers::ident_name_at(&root, offset)?;
    let (_decl, sym) = crate::handlers::find_top_level_decl(ws, uri, &name)?;
    let out = crate::handlers::occurrences_in_file(ws, uri, doc.source_file, &sym.name, sym.kind)
        .into_iter()
        .map(|o| lsp::DocumentHighlight {
            range: o.range,
            kind: Some(if o.is_declaration {
                lsp::DocumentHighlightKind::WRITE
            } else {
                lsp::DocumentHighlightKind::READ
            }),
        })
        .collect();
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workspace::Workspace;
    use tower_lsp::lsp_types as lsp;

    fn ws_with(src: &str) -> (Workspace, lsp::Url) {
        let mut ws = Workspace::default();
        let uri = lsp::Url::parse("file:///t.leek").unwrap();
        ws.open(uri.clone(), src.to_string());
        (ws, uri)
    }

    fn pos(l: u32, c: u32) -> lsp::Position {
        lsp::Position {
            line: l,
            character: c,
        }
    }

    #[test]
    fn highlights_var_and_all_uses() {
        let src = "var counter = 0\ncounter = counter + 1\nreturn counter\n";
        let (ws, uri) = ws_with(src);
        // Cursor on `counter` in line 1 (the first read site).
        let hits = handle(&ws, &uri, pos(1, 11)).expect("hits");
        // Declaration + write-target + two reads + one final return ref =
        // at least 4. Specifically: def + ref-on-lhs + ref-on-rhs + final.
        assert!(hits.len() >= 4, "expected ≥4 hits, got {hits:?}");
        let write_count = hits
            .iter()
            .filter(|h| h.kind == Some(lsp::DocumentHighlightKind::WRITE))
            .count();
        let read_count = hits
            .iter()
            .filter(|h| h.kind == Some(lsp::DocumentHighlightKind::READ))
            .count();
        // Exactly one declaration (Write) and the rest reads.
        assert_eq!(write_count, 1, "writes: {hits:?}");
        assert!(read_count >= 3, "reads: {hits:?}");
    }

    #[test]
    fn highlights_function_definition_and_call_sites() {
        let src = "function go() { return 1 }\ngo()\ngo()\n";
        let (ws, uri) = ws_with(src);
        // Cursor on the call site `go` (line 1, col 0).
        let hits = handle(&ws, &uri, pos(1, 0)).expect("hits");
        assert!(hits.len() >= 3, "expected def + 2 calls, got {hits:?}");
    }

    #[test]
    fn cursor_on_keyword_returns_none() {
        let src = "var x = 1\n";
        let (ws, uri) = ws_with(src);
        // Position on `var` — no symbol/ref here.
        assert!(handle(&ws, &uri, pos(0, 1)).is_none());
    }
}
