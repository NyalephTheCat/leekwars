//! `textDocument/selectionRange` — smart-select expansion.
//!
//! For each requested cursor position, walks up the CST and
//! returns a chain of progressively larger ranges (the cursor's
//! token → enclosing expression → enclosing statement → block →
//! function → source file). VS Code's "Expand selection" feature
//! steps through these.

use leek_span::Span;
use leek_syntax::{SyntaxNode, SyntaxToken};
use tower_lsp::lsp_types as lsp;

use crate::util::position::{position_to_offset, span_to_range};
use crate::workspace::Workspace;

pub fn handle(
    ws: &Workspace,
    uri: &lsp::Url,
    positions: Vec<lsp::Position>,
) -> Option<Vec<lsp::SelectionRange>> {
    let doc = ws.doc(uri)?;
    let run = crate::pipeline::run(ws, uri, leek_recipes::Target::Parsed)?;
    let green = &run.get::<leek_parser::pipeline::GreenTreeArtifact>()?.0;
    let root = SyntaxNode::new_root(green.clone());

    let out: Vec<lsp::SelectionRange> = positions
        .into_iter()
        .map(|pos| {
            let offset = position_to_offset(doc.pos_map(), pos).unwrap_or(0);
            chain_for_offset(&root, offset, doc.pos_map())
        })
        .collect();
    Some(out)
}

/// Build the linked-list chain of `SelectionRange`s outward from
/// the cursor. Innermost first; each step's parent is the next
/// enclosing node. We collapse adjacent identical ranges (a node
/// and its sole child that share text-range bounds) so each click
/// of "expand selection" produces a visibly different selection.
fn chain_for_offset(
    root: &SyntaxNode,
    offset: u32,
    pm: crate::util::position::PosMap<'_>,
) -> lsp::SelectionRange {
    let token: Option<SyntaxToken> = root.token_at_offset(offset.into()).right_biased();

    // Collect node ancestors from innermost out.
    let mut ranges: Vec<lsp::Range> = Vec::new();
    if let Some(t) = &token {
        ranges.push(token_range(t, pm));
    }
    let mut node = token
        .and_then(|t| t.parent())
        .or_else(|| Some(root.clone()));
    while let Some(n) = node {
        let r = n.text_range();
        let span = Span::new(
            leek_span::SourceId::new(1).unwrap(),
            u32::from(r.start()),
            u32::from(r.end()),
        );
        let lsp_range = span_to_range(pm, span);
        if ranges.last() != Some(&lsp_range) {
            ranges.push(lsp_range);
        }
        node = n.parent();
    }
    if ranges.is_empty() {
        ranges.push(lsp::Range {
            start: lsp::Position {
                line: 0,
                character: 0,
            },
            end: lsp::Position {
                line: 0,
                character: 0,
            },
        });
    }

    // Build the linked list outward-in: outermost is the tail
    // with no `parent`, then wrap inward.
    let mut iter = ranges.into_iter().rev();
    let outer = iter.next().unwrap();
    let mut acc = lsp::SelectionRange {
        range: outer,
        parent: None,
    };
    for r in iter {
        acc = lsp::SelectionRange {
            range: r,
            parent: Some(Box::new(acc)),
        };
    }
    acc
}

fn token_range(t: &SyntaxToken, pm: crate::util::position::PosMap<'_>) -> lsp::Range {
    let r = t.text_range();
    let span = Span::new(
        leek_span::SourceId::new(1).unwrap(),
        u32::from(r.start()),
        u32::from(r.end()),
    );
    span_to_range(pm, span)
}
