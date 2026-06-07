//! `textDocument/linkedEditingRange` — when the cursor sits on a
//! renameable symbol, return every in-file occurrence (declaration
//! plus references) so the editor can edit them together: type over
//! one and the rest update live, without invoking a full rename.
//!
//! Same lookup pipeline as [`document_highlight`](super::document_highlight)
//! and [`rename`](super::rename): resolve the symbol under the cursor,
//! then collect its `def_span` and every `ResolvedRef` that targets it.
//! Unlike highlight, we deliberately *refuse* builtins and library
//! names — those have no in-file declaration to keep in sync, and
//! editing them as a group would be wrong.
//!
//! The returned `word_pattern` constrains what the client will accept
//! inside the linked ranges; it matches a Leekscript identifier so the
//! moment the user types a non-identifier character the editor ends the
//! linked edit.

use leek_resolver::SymbolKind;
use leek_span::Span;
use tower_lsp::lsp_types as lsp;

use crate::util::position::{position_to_offset, span_to_range};
use crate::workspace::Workspace;

/// A Leekscript identifier: a letter or `_` followed by word chars.
/// Handed to the client so linked editing only stays active while the
/// edited text remains a valid identifier.
const IDENT_WORD_PATTERN: &str = "[A-Za-z_][A-Za-z0-9_]*";

pub fn handle(
    ws: &Workspace,
    uri: &lsp::Url,
    pos: lsp::Position,
) -> Option<lsp::LinkedEditingRanges> {
    let doc = ws.doc(uri)?;
    let offset = position_to_offset(doc.pos_map(), pos)?;

    let run = crate::pipeline::run(ws, uri, leek_recipes::Target::Resolved)?;
    let table = &run.get::<leek_resolver::pipeline::ResolveArtifact>()?.table;

    let target_id = crate::handlers::resolve_symbol_id(table, offset)?;

    // Only user-declared symbols can be linked-edited: a builtin or a
    // host-library name has no declaration site in this file, so there
    // is nothing to keep in sync. `symbol()` returns `None` for a
    // reference whose target slot is a synthetic builtin.
    let sym = table.symbol(target_id)?;
    if sym.kind == SymbolKind::Builtin {
        return None;
    }

    let mut ranges: Vec<lsp::Range> = vec![span_to_range(doc.pos_map(), sym.def_span)];
    for r in &table.references {
        if r.target == target_id {
            let span = Span::new(
                doc.source_file_source_id(&ws.db),
                r.name_offset,
                r.name_offset + r.name_len,
            );
            ranges.push(span_to_range(doc.pos_map(), span));
        }
    }

    Some(lsp::LinkedEditingRanges {
        ranges,
        word_pattern: Some(IDENT_WORD_PATTERN.to_string()),
    })
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
    fn links_declaration_and_every_use() {
        let src = "var counter = 0\ncounter = counter + 1\nreturn counter\n";
        let (ws, uri) = ws_with(src);
        // Cursor on the declaration `counter` (line 0, after `var `).
        let res = handle(&ws, &uri, pos(0, 4)).expect("linked ranges");
        // decl + lhs + rhs + final return = 4 occurrences.
        assert_eq!(res.ranges.len(), 4, "ranges: {:?}", res.ranges);
        // The first range is the declaration.
        assert_eq!(res.ranges[0].start, pos(0, 4));
        assert!(res.word_pattern.is_some(), "should carry a word pattern");
    }

    #[test]
    fn links_from_a_use_site() {
        let src = "function go() { return 1 }\ngo()\ngo()\n";
        let (ws, uri) = ws_with(src);
        // Cursor on the first call site `go` (line 1, col 0).
        let res = handle(&ws, &uri, pos(1, 0)).expect("linked ranges");
        // decl + 2 calls.
        assert_eq!(res.ranges.len(), 3, "ranges: {:?}", res.ranges);
    }

    #[test]
    fn refuses_builtin_name() {
        // `count` is a builtin — it has no in-file declaration, so there
        // is nothing to link-edit.
        let src = "var a = [1, 2, 3]\nvar n = count(a)\n";
        let (ws, uri) = ws_with(src);
        // Cursor on the `count` call (line 1, col 8).
        assert!(handle(&ws, &uri, pos(1, 8)).is_none());
    }

    #[test]
    fn refuses_on_keyword() {
        let src = "var x = 1\n";
        let (ws, uri) = ws_with(src);
        // Cursor on `var`.
        assert!(handle(&ws, &uri, pos(0, 1)).is_none());
    }
}
