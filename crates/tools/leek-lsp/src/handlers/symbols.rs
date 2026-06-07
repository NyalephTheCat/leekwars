//! `textDocument/documentSymbol` — outline view.
//!
//! Implementation walks the green tree directly via rowan so we
//! don't need a typed-AST roundtrip. Each top-level FnDecl /
//! ClassDecl / VarDeclStmt becomes a [`lsp::DocumentSymbol`];
//! ClassDecl recurses into ClassField / ClassMethod /
//! ClassConstructor.

use leek_syntax::{SyntaxKind, SyntaxNode};
use tower_lsp::lsp_types as lsp;

use crate::util::position::{offset_to_position, PosMap};
use crate::workspace::Workspace;

pub fn handle(ws: &Workspace, uri: &lsp::Url) -> Option<lsp::DocumentSymbolResponse> {
    let doc = ws.doc(uri)?;
    let run = crate::pipeline::run(ws, uri, leek_recipes::Target::Parsed)?;
    let green = &run.get::<leek_parser::pipeline::GreenTreeArtifact>()?.0;
    let root = SyntaxNode::new_root(green.clone());

    let symbols: Vec<lsp::DocumentSymbol> = root
        .children()
        .filter_map(|n| symbol_for(&n, doc.pos_map()))
        .collect();
    Some(lsp::DocumentSymbolResponse::Nested(symbols))
}

fn symbol_for(node: &SyntaxNode, pm: PosMap<'_>) -> Option<lsp::DocumentSymbol> {
    match node.kind() {
        SyntaxKind::FnDecl => Some(build_symbol(
            node,
            pm,
            lsp::SymbolKind::FUNCTION,
            /* children */ &[],
        )),
        SyntaxKind::ClassDecl => {
            // Recurse into the ClassBody for members.
            let body = node.children().find(|c| c.kind() == SyntaxKind::ClassBody);
            let mut children = Vec::new();
            if let Some(body) = body {
                for member in body.children() {
                    if let Some(sym) = symbol_for_member(&member, pm) {
                        children.push(sym);
                    }
                }
            }
            Some(build_symbol(
                node,
                pm,
                lsp::SymbolKind::CLASS,
                &children,
            ))
        }
        SyntaxKind::VarDeclStmt => {
            // Top-level var/global as a variable symbol.
            Some(build_symbol(
                node,
                pm,
                lsp::SymbolKind::VARIABLE,
                &[],
            ))
        }
        _ => None,
    }
}

fn symbol_for_member(node: &SyntaxNode, pm: PosMap<'_>) -> Option<lsp::DocumentSymbol> {
    let kind = match node.kind() {
        SyntaxKind::ClassMethod => lsp::SymbolKind::METHOD,
        SyntaxKind::ClassConstructor => lsp::SymbolKind::CONSTRUCTOR,
        SyntaxKind::ClassField => lsp::SymbolKind::FIELD,
        _ => return None,
    };
    Some(build_symbol(node, pm, kind, &[]))
}

/// Construct a `DocumentSymbol`: name from the first Ident token,
/// range covering the whole node, selection_range covering just the
/// identifier.
fn build_symbol(
    node: &SyntaxNode,
    pm: PosMap<'_>,
    kind: lsp::SymbolKind,
    children: &[lsp::DocumentSymbol],
) -> lsp::DocumentSymbol {
    let (name, name_range) = ident_token(node).map_or_else(
        || ("<anon>".to_string(), node_range(node, pm)),
        |t| {
            let r = t.text_range();
            (
                t.text().to_string(),
                lsp::Range {
                    start: offset_to_position(pm, u32::from(r.start())),
                    end: offset_to_position(pm, u32::from(r.end())),
                },
            )
        },
    );

    let full_range = node_range(node, pm);
    #[allow(deprecated)] // tags/deprecated are deprecated but the struct still needs them
    lsp::DocumentSymbol {
        name,
        detail: None,
        kind,
        tags: None,
        deprecated: None,
        range: full_range,
        selection_range: name_range,
        children: if children.is_empty() {
            None
        } else {
            Some(children.to_vec())
        },
    }
}

fn node_range(node: &SyntaxNode, pm: PosMap<'_>) -> lsp::Range {
    let r = node.text_range();
    lsp::Range {
        start: offset_to_position(pm, u32::from(r.start())),
        end: offset_to_position(pm, u32::from(r.end())),
    }
}

fn ident_token(node: &SyntaxNode) -> Option<leek_syntax::SyntaxToken> {
    node.children_with_tokens()
        .filter_map(leek_syntax::language::NodeOrToken::into_token)
        .find(|t| t.kind() == SyntaxKind::Ident)
}
