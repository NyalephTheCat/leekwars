//! `textDocument/definition` — jump to a name's declaration.
//!
//! Resolves in three tiers: first the cursor file's own symbol table
//! (locals, params, fields, and any top-level symbol declared in this
//! file), then a member access (`recv.member` — the resolver records
//! no references for member names, so the receiver's class is found
//! via the type table and the member through the `extends` chain),
//! then — when both miss — the wider *program* the file belongs to,
//! so a call to a function/class/global defined in an `include`d
//! file jumps to the right file. The LSP resolves each file in
//! isolation, so cross-file references never bind locally; the
//! program-scope search closes that gap.

use leek_span::Span;
use leek_syntax::SyntaxNode;
use tower_lsp::lsp_types as lsp;

use super::member;
use crate::util::position::{PosMap, position_to_offset, span_to_range};
use crate::workspace::Workspace;

pub fn handle(
    ws: &Workspace,
    uri: &lsp::Url,
    pos: lsp::Position,
) -> Option<lsp::GotoDefinitionResponse> {
    let doc = ws.doc(uri)?;
    let offset = position_to_offset(doc.pos_map(), pos)?;

    // TypeChecked (not just Resolved): a member access needs the type
    // table to resolve its receiver's class.
    let run = crate::pipeline::run(ws, uri, leek_recipes::Target::TypeChecked)?;
    let table = &run.get::<leek_resolver::pipeline::ResolveArtifact>()?.table;

    // 1. Same-file: the cursor may be on a use OR on a declaration
    //    itself. Try the references list first, then fall back to a
    //    symbol whose `def_span` covers the cursor.
    if let Some(sym) = crate::handlers::resolve_symbol(table, offset) {
        let range = span_to_range(doc.pos_map(), sym.def_span);
        return Some(lsp::GotoDefinitionResponse::Scalar(lsp::Location {
            uri: uri.clone(),
            range,
        }));
    }

    let green = &run.get::<leek_parser::pipeline::GreenTreeArtifact>()?.0;
    let root = SyntaxNode::new_root(green.clone());

    // 2. Member access: the resolver records no references for member
    //    names, so resolve `recv.member` through the receiver's class.
    if let Some((start, end)) = member_definition(&run, &root, offset) {
        let span = Span::new(doc.source_file_source_id(&ws.db), start, end);
        return Some(lsp::GotoDefinitionResponse::Scalar(lsp::Location {
            uri: uri.clone(),
            range: span_to_range(doc.pos_map(), span),
        }));
    }

    // 3. Cross-file: an unresolved top-level identifier may be declared
    //    in an included file. Recover the name and search the program.
    let name = crate::handlers::ident_name_at(&root, offset)?;
    let (file, sym) = crate::handlers::find_top_level_decl(ws, uri, &name)?;

    let text = file.source_file.text(&ws.db);
    let line_table = leek_span::LineTable::new(text);
    let range = span_to_range(PosMap::new(&line_table, text), sym.def_span);
    Some(lsp::GotoDefinitionResponse::Scalar(lsp::Location {
        uri: file.uri,
        range,
    }))
}

/// Resolve the member access under the cursor to its declaration's
/// name token range (byte offsets): receiver class via the type
/// table, member via the `extends` chain. Constructors have no name
/// token — their range is the `constructor` keyword (the
/// declaration's first token).
fn member_definition(
    run: &leek_pipeline::Run<'_>,
    root: &SyntaxNode,
    offset: u32,
) -> Option<(u32, u32)> {
    let type_art = run.get::<leek_types::pipeline::TypeCheckArtifact>()?;
    let resolve_art = run.get::<leek_resolver::pipeline::ResolveArtifact>();
    let (field_expr, field_tok) = member::field_access_at(root, offset)?;
    let base = field_expr.base()?;
    let class = member::base_class_name(root, resolve_art, &type_art.table, &base)?;
    let decl = member::find_member_in_chain(root, &class, field_tok.text())?;
    let r = member::member_decl_name_token(&decl)
        .map(|t| t.text_range())
        .or_else(|| decl.first_token().map(|t| t.text_range()))?;
    Some((u32::from(r.start()), u32::from(r.end())))
}
