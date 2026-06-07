//! `textDocument/foldingRange` — fold blocks `{…}`.

use leek_syntax::{SyntaxKind, SyntaxNode};
use tower_lsp::lsp_types as lsp;

use crate::workspace::Workspace;

pub fn handle(ws: &Workspace, uri: &lsp::Url) -> Option<Vec<lsp::FoldingRange>> {
    let doc = ws.doc(uri)?;
    let run = crate::pipeline::run(ws, uri, leek_recipes::Target::Parsed)?;
    let green = &run.get::<leek_parser::pipeline::GreenTreeArtifact>()?.0;
    let root = SyntaxNode::new_root(green.clone());

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
