//! `textDocument/foldingRange` — fold blocks `{…}`.

use leek_syntax::SyntaxKind;
use tower_lsp::lsp_types as lsp;

use crate::workspace::Workspace;

pub fn handle(ws: &Workspace, uri: &lsp::Url) -> Option<Vec<lsp::FoldingRange>> {
    let doc = ws.doc(uri)?;
    let root = crate::analysis::syntax_root(&ws.db, doc.source_file);

    let mut out: Vec<lsp::FoldingRange> = Vec::new();
    for node in root.descendants() {
        // Block-like nodes whose span crosses a line — fold them.
        let kind = node.kind();
        let foldable = matches!(
            kind,
            SyntaxKind::Block
                | SyntaxKind::ClassBody
                | SyntaxKind::ArrayExpr
                | SyntaxKind::MapExpr
                | SyntaxKind::ObjectExpr
                | SyntaxKind::SetExpr
        );
        if !foldable {
            continue;
        }
        let r = node.text_range();
        let start = doc.line_table.line_col(u32::from(r.start()));
        let end = doc.line_table.line_col(u32::from(r.end()));
        if start.line == end.line {
            continue;
        }
        out.push(lsp::FoldingRange {
            start_line: start.line.saturating_sub(1),
            start_character: None,
            end_line: end.line.saturating_sub(1),
            end_character: None,
            kind: Some(lsp::FoldingRangeKind::Region),
            collapsed_text: None,
        });
    }
    Some(out)
}
