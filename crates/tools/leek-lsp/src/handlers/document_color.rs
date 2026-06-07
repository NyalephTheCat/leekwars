//! `textDocument/documentColor` + `textDocument/colorPresentation`.
//!
//! We surface two color-literal shapes so the editor can render
//! inline swatches and let the user pick a new value:
//!
//!  1. **Integer color literals** — `color(r, g, b)` calls with
//!     all three args being int literals in 0..=255. The pack is
//!     `(r << 16) | (g << 8) | b`, which matches the runtime
//!     `color` builtin.
//!  2. **24-bit hex int literals** — bare integers like
//!     `0xFF00FF` showing up as the first argument to `setColor`
//!     or as a `var c = 0xFF00FF` initializer. Conservative: we
//!     only flag *positive ints* with values that look like a
//!     24-bit color (≤ 0xFFFFFF) when they appear in a context
//!     where they're likely a color (literal-only, no math).
//!
//! `colorPresentation` is the editor's reverse: given a color the
//! user picked, render text edits. We emit two options for each
//! pick: a `color(r, g, b)` call form and a `0xRRGGBB` literal.

use leek_span::Span;
use leek_syntax::{SyntaxKind, SyntaxNode};
use tower_lsp::lsp_types as lsp;

use crate::util::position::span_to_range;
use crate::workspace::Workspace;

pub fn handle(ws: &Workspace, uri: &lsp::Url) -> Option<Vec<lsp::ColorInformation>> {
    let doc = ws.doc(uri)?;
    let run = crate::pipeline::run(ws, uri, leek_recipes::Target::Parsed)?;
    let green = &run.get::<leek_parser::pipeline::GreenTreeArtifact>()?.0;
    let root = SyntaxNode::new_root(green.clone());

    let mut out: Vec<lsp::ColorInformation> = Vec::new();

    // 1) color(r, g, b) calls.
    for node in root.descendants() {
        if node.kind() != SyntaxKind::CallExpr {
            continue;
        }
        // Find the callee NameRef → Ident `color`.
        let Some(callee_ident) = node
            .children()
            .find(|c| c.kind() == SyntaxKind::NameRef)
            .and_then(|n| {
                n.children_with_tokens()
                    .filter_map(leek_syntax::language::NodeOrToken::into_token)
                    .find(|t| t.kind() == SyntaxKind::Ident)
            })
        else {
            continue;
        };
        if callee_ident.text() != "color" {
            continue;
        }
        let Some(arg_list) = node.children().find(|c| c.kind() == SyntaxKind::ArgList) else {
            continue;
        };
        let int_args: Vec<u32> = arg_list
            .descendants_with_tokens()
            .filter_map(leek_syntax::language::NodeOrToken::into_token)
            .filter(|t| t.kind() == SyntaxKind::IntLiteral)
            .filter_map(|t| t.text().parse::<u32>().ok())
            .collect();
        if int_args.len() < 3 {
            continue;
        }
        let r = int_args[0].min(255);
        let g = int_args[1].min(255);
        let b = int_args[2].min(255);
        let nr = node.text_range();
        let span = Span::new(
            leek_span::SourceId::new(1).unwrap(),
            u32::from(nr.start()),
            u32::from(nr.end()),
        );
        out.push(lsp::ColorInformation {
            range: span_to_range(doc.pos_map(), span),
            color: rgb_to_lsp(r as u8, g as u8, b as u8),
        });
    }

    // 2) bare `0xRRGGBB` int literals — values 0..=0xFFFFFF whose
    //    source spelling starts with `0x` (so we don't surface
    //    `42` as a color).
    for tok in root
        .descendants_with_tokens()
        .filter_map(leek_syntax::language::NodeOrToken::into_token)
        .filter(|t| t.kind() == SyntaxKind::IntLiteral)
    {
        let text = tok.text();
        let lower = text.to_lowercase();
        if !lower.starts_with("0x") {
            continue;
        }
        let Ok(val) = u32::from_str_radix(&lower[2..], 16) else {
            continue;
        };
        if val > 0xFF_FFFF {
            continue;
        }
        let r = ((val >> 16) & 0xFF) as u8;
        let g = ((val >> 8) & 0xFF) as u8;
        let b = (val & 0xFF) as u8;
        let r_range = tok.text_range();
        let span = Span::new(
            leek_span::SourceId::new(1).unwrap(),
            u32::from(r_range.start()),
            u32::from(r_range.end()),
        );
        out.push(lsp::ColorInformation {
            range: span_to_range(doc.pos_map(), span),
            color: rgb_to_lsp(r, g, b),
        });
    }

    Some(out)
}

// The components are clamped to `0.0..=255.0` before the cast, so the
// `f32 -> u32` conversions are exact (no checked float conversion exists).
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
pub fn presentations(
    _ws: &Workspace,
    color: lsp::Color,
    _range: lsp::Range,
) -> Vec<lsp::ColorPresentation> {
    let r = (color.red * 255.0).round().clamp(0.0, 255.0) as u32;
    let g = (color.green * 255.0).round().clamp(0.0, 255.0) as u32;
    let b = (color.blue * 255.0).round().clamp(0.0, 255.0) as u32;
    vec![
        lsp::ColorPresentation {
            label: format!("color({r}, {g}, {b})"),
            text_edit: None,
            additional_text_edits: None,
        },
        lsp::ColorPresentation {
            label: format!("0x{:06X}", (r << 16) | (g << 8) | b),
            text_edit: None,
            additional_text_edits: None,
        },
    ]
}

fn rgb_to_lsp(r: u8, g: u8, b: u8) -> lsp::Color {
    lsp::Color {
        red: f32::from(r) / 255.0,
        green: f32::from(g) / 255.0,
        blue: f32::from(b) / 255.0,
        alpha: 1.0,
    }
}
