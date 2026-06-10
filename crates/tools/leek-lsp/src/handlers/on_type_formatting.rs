//! `textDocument/onTypeFormatting` — auto-format on certain
//! keystrokes.
//!
//! Triggered when the user types one of the configured characters
//! (`;`, `}`, `\n`). For each trigger we:
//!
//! - `;` → format the enclosing statement.
//! - `}` → format the block that just closed (from the matching
//!   `{` to this `}`).
//! - `\n` → re-indent the line that was just terminated.
//!
//! Resolution strategy uses the smallest enclosing CST node of
//! the right kind, then runs `leek_fmt::format_range` against
//! that range — the same machinery `rangeFormatting` uses.

use leek_span::Span;
use leek_syntax::{SyntaxKind, SyntaxNode};
use tower_lsp::lsp_types as lsp;

use crate::util::position::{position_to_offset, span_to_range};
use crate::workspace::Workspace;

pub fn handle(
    ws: &Workspace,
    uri: &lsp::Url,
    pos: lsp::Position,
    trigger: &str,
) -> Option<Vec<lsp::TextEdit>> {
    let doc = ws.doc(uri)?;
    let offset = position_to_offset(doc.pos_map(), pos)?;

    let run = crate::pipeline::run(ws, uri, leek_recipes::Target::Parsed)?;
    let green = &run.get::<leek_parser::pipeline::GreenTreeArtifact>()?.0;
    let root = SyntaxNode::new_root(green.clone());

    let range = match trigger {
        ";" => enclosing_kinds(&root, offset, STMT_KINDS),
        "}" => enclosing_kinds(&root, offset, BLOCK_KINDS),
        "\n" => Some(line_range(&doc.text, offset)),
        _ => None,
    }?;

    let (target_range, replacement) = leek_fmt::format_range(green, &ws.settings.format, range)?;

    let span = Span::new(
        doc.source_file_source_id(&ws.db),
        target_range.start,
        target_range.end,
    );
    Some(vec![lsp::TextEdit {
        range: span_to_range(doc.pos_map(), span),
        new_text: replacement,
    }])
}

/// `;`-triggered: smallest enclosing statement-shaped node.
const STMT_KINDS: &[SyntaxKind] = &[
    SyntaxKind::VarDeclStmt,
    SyntaxKind::ReturnStmt,
    SyntaxKind::IfStmt,
    SyntaxKind::WhileStmt,
    SyntaxKind::DoWhileStmt,
    SyntaxKind::ForStmt,
    SyntaxKind::ForeachStmt,
    SyntaxKind::SwitchStmt,
    SyntaxKind::BreakStmt,
    SyntaxKind::ContinueStmt,
];

/// `}`-triggered: smallest enclosing block-shaped node.
const BLOCK_KINDS: &[SyntaxKind] = &[
    SyntaxKind::Block,
    SyntaxKind::FnDecl,
    SyntaxKind::ClassDecl,
    SyntaxKind::ClassMethod,
    SyntaxKind::ClassConstructor,
    SyntaxKind::IfStmt,
    SyntaxKind::WhileStmt,
    SyntaxKind::ForStmt,
    SyntaxKind::ForeachStmt,
];

fn enclosing_kinds(
    root: &SyntaxNode,
    offset: u32,
    kinds: &[SyntaxKind],
) -> Option<std::ops::Range<u32>> {
    let token = root.token_at_offset(offset.into()).left_biased()?;
    let mut node: Option<SyntaxNode> = token.parent();
    while let Some(n) = node {
        if kinds.contains(&n.kind()) {
            let r = n.text_range();
            return Some(u32::from(r.start())..u32::from(r.end()));
        }
        node = n.parent();
    }
    None
}

/// Byte range of the line containing `offset` (excluding the
/// trailing newline). For a `\n` trigger we want to re-format the
/// line that was just completed, not the new empty one.
fn line_range(text: &str, offset: u32) -> std::ops::Range<u32> {
    let off = (offset as usize).min(text.len());
    // For `\n`, the cursor sits just past the newline. We want to
    // format the PREVIOUS line.
    let probe = if off > 0 { off - 1 } else { off };
    let bytes = text.as_bytes();
    let mut start = probe;
    while start > 0 && bytes[start - 1] != b'\n' {
        start -= 1;
    }
    let mut end = probe;
    while end < bytes.len() && bytes[end] != b'\n' {
        end += 1;
    }
    leek_span::offset(start)..leek_span::offset(end)
}
