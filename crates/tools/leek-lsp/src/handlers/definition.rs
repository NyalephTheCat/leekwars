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

use leek_db::SourceFile;
use leek_span::Span;
use leek_syntax::SyntaxNode;
use tower_lsp::lsp_types as lsp;

use super::member;
use crate::util::position::PosMap;
use crate::workspace::Workspace;

pub fn handle(
    ws: &Workspace,
    uri: &lsp::Url,
    pos: lsp::Position,
) -> Option<lsp::GotoDefinitionResponse> {
    let doc = ws.doc(uri)?;
    let offset = doc.pos_map().to_offset(pos)?;

    let resolved = crate::analysis::resolved(&ws.db, doc.source_file);
    let table = &resolved.table;

    // 1. Same-file: the cursor may be on a use OR on a declaration
    //    itself. Try the references list first, then fall back to a
    //    symbol whose `def_span` covers the cursor.
    if let Some(sym) = crate::handlers::resolve_symbol(table, offset) {
        let range = doc.pos_map().span_range(sym.def_span);
        return Some(lsp::GotoDefinitionResponse::Scalar(lsp::Location {
            uri: uri.clone(),
            range,
        }));
    }

    let root = crate::analysis::syntax_root(&ws.db, doc.source_file);

    // 2. Member access: the resolver records no references for member
    //    names, so resolve `recv.member` through the receiver's class.
    if let Some((start, end)) = member_definition(ws, doc.source_file, &resolved, &root, offset) {
        let span = Span::new(doc.source_file_source_id(&ws.db), start, end);
        return Some(lsp::GotoDefinitionResponse::Scalar(lsp::Location {
            uri: uri.clone(),
            range: doc.pos_map().span_range(span),
        }));
    }

    // 3. Cross-file: an unresolved top-level identifier may be declared
    //    in an included file. Recover the name and search the program.
    let name = crate::handlers::ident_name_at(&root, offset)?;
    let (file, sym) = crate::handlers::find_top_level_decl(ws, uri, &name)?;

    let range = decl_range(ws, file.source_file, sym.def_span);
    Some(lsp::GotoDefinitionResponse::Scalar(lsp::Location {
        uri: file.uri,
        range,
    }))
}

/// `def_span` as an LSP range in `source_file`'s own text.
///
/// Takes the workspace's already-built line table when it holds the file
/// — the declaring file is normally one of its analysis targets — and
/// only scans the text itself for a file it does not.
fn decl_range(ws: &Workspace, source_file: SourceFile, def_span: Span) -> lsp::Range {
    if let Some(pos_map) = ws.pos_map_for(source_file) {
        return pos_map.span_range(def_span);
    }
    let text = source_file.text(&ws.db);
    let line_table = leek_span::LineTable::new(text);
    PosMap::new(&line_table, text).span_range(def_span)
}

/// Resolve the member access under the cursor to its declaration's
/// name token range (byte offsets): receiver class via the type
/// table, member via the `extends` chain. Constructors have no name
/// token — their range is the `constructor` keyword (the
/// declaration's first token).
fn member_definition(
    ws: &Workspace,
    source_file: SourceFile,
    resolved: &leek_db::queries::ResolveArtifact,
    root: &SyntaxNode,
    offset: u32,
) -> Option<(u32, u32)> {
    let (field_expr, field_tok) = member::field_access_at(root, offset)?;
    let base = field_expr.base()?;
    // The type table is asked for only once the cursor is known to sit
    // on a member access — the handler's first two tiers never need it,
    // and they answer the common case.
    let typed = crate::analysis::typed(&ws.db, source_file);
    let class = member::base_class_name(root, resolved, &typed.table, &base)?;
    let decl = member::find_member_in_chain(root, &class, field_tok.text())?;
    let r = member::member_decl_name_token(&decl)
        .map(|t| t.text_range())
        .or_else(|| decl.first_token().map(|t| t.text_range()))?;
    Some((u32::from(r.start()), u32::from(r.end())))
}
