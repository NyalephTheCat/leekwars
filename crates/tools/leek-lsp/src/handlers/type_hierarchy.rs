//! `textDocument/prepareTypeHierarchy` + `typeHierarchy/supertypes`
//! + `typeHierarchy/subtypes`.
//!
//! Walks the class-inheritance graph (`class C extends P { ... }`)
//! across the whole *program* the cursor's class belongs to — a parent
//! or a subclass may live in an `include`d file. `prepare` resolves the
//! cursor to a class (falling back to a cross-file lookup); `supertypes`
//! reports its parent; `subtypes` finds every class whose parent is it.

use leek_hir::pipeline::HirArtifact;
use leek_hir::Def;
use leek_pipeline::salsa::SourceFile;
use leek_resolver::SymbolKind;
use leek_span::{LineTable, Span};
use tower_lsp::lsp_types as lsp;

use crate::util::position::{PosMap, position_to_offset, span_to_range};
use crate::workspace::Workspace;

pub fn prepare(
    ws: &Workspace,
    uri: &lsp::Url,
    pos: lsp::Position,
) -> Option<Vec<lsp::TypeHierarchyItem>> {
    let doc = ws.doc(uri)?;
    let offset = position_to_offset(doc.pos_map(), pos)?;
    let run = crate::pipeline::run(ws, uri, leek_recipes::Target::Resolved)?;
    let table = &run.get::<leek_resolver::pipeline::ResolveArtifact>()?.table;

    // 1. Resolve locally — the cursor is on a class declared in this file
    //    (or a reference to one).
    if let Some(sym) = crate::handlers::resolve_symbol(table, offset)
        && sym.kind == SymbolKind::Class
    {
        return Some(vec![item_for(ws, uri, doc.source_file, &sym.name, sym.def_span, sym.full_span)]);
    }

    // 2. Cross-file: the cursor is on a use of a class declared in an
    //    `include`d file (e.g. `extends Animal`, `new Cat()`).
    let green = &run.get::<leek_parser::pipeline::GreenTreeArtifact>()?.0;
    let root = leek_syntax::SyntaxNode::new_root(green.clone());
    let name = crate::handlers::ident_name_at(&root, offset)?;
    let (file, sym) = crate::handlers::find_top_level_decl(ws, uri, &name)?;
    if sym.kind != SymbolKind::Class {
        return None;
    }
    Some(vec![item_for(
        ws,
        &file.uri,
        file.source_file,
        &sym.name,
        sym.def_span,
        sym.full_span,
    )])
}

pub fn supertypes(
    ws: &Workspace,
    _uri: &lsp::Url,
    item: &lsp::TypeHierarchyItem,
) -> Option<Vec<lsp::TypeHierarchyItem>> {
    let classes = program_classes(ws, &item.uri);
    let me = classes.iter().find(|c| c.name == item.name)?;
    let mut out = Vec::new();
    if let Some(parent) = &me.parent
        && let Some(pc) = classes.iter().find(|c| &c.name == parent)
    {
        out.push(item_for_class(ws, pc));
    }
    Some(out)
}

pub fn subtypes(
    ws: &Workspace,
    _uri: &lsp::Url,
    item: &lsp::TypeHierarchyItem,
) -> Option<Vec<lsp::TypeHierarchyItem>> {
    let classes = program_classes(ws, &item.uri);
    let out = classes
        .iter()
        .filter(|c| c.parent.as_deref() == Some(item.name.as_str()))
        .map(|c| item_for_class(ws, c))
        .collect();
    Some(out)
}

/// One class in the program: its declaring file, name, parent, and the
/// spans needed to build a hierarchy item.
struct ClassInfo {
    uri: lsp::Url,
    source_file: SourceFile,
    name: String,
    parent: Option<String>,
    def_span: Span,
    full_span: Span,
}

/// Every class across the program `home_uri` belongs to, joined from
/// each file's HIR (for the `extends` parent) and resolve table (for the
/// declaration spans). Class names are unique within a program, so the
/// list has no duplicates.
fn program_classes(ws: &Workspace, home_uri: &lsp::Url) -> Vec<ClassInfo> {
    let mut out: Vec<ClassInfo> = Vec::new();
    for file in crate::handlers::program_scope::program_scope(ws, home_uri) {
        let Some(run) = crate::pipeline::run_on_file(ws, file.source_file, leek_recipes::Target::Hir)
        else {
            continue;
        };
        let Some(table) = run
            .get::<leek_resolver::pipeline::ResolveArtifact>()
            .map(|a| &a.table)
        else {
            continue;
        };
        let Some(hir) = run.get::<HirArtifact>() else {
            continue;
        };
        for def in &hir.0.defs {
            let Def::Class(c) = def else { continue };
            let Some(sym) = table
                .symbols
                .iter()
                .find(|s| s.kind == SymbolKind::Class && s.name == c.name)
            else {
                continue;
            };
            out.push(ClassInfo {
                uri: file.uri.clone(),
                source_file: file.source_file,
                name: c.name.clone(),
                parent: c.parent.clone(),
                def_span: sym.def_span,
                full_span: sym.full_span,
            });
        }
    }
    out
}

fn item_for_class(ws: &Workspace, ci: &ClassInfo) -> lsp::TypeHierarchyItem {
    item_for(
        ws,
        &ci.uri,
        ci.source_file,
        &ci.name,
        ci.def_span,
        ci.full_span,
    )
}

/// Build a [`TypeHierarchyItem`] for a class, converting its spans
/// against its *own* file's text (which may differ from the request's).
fn item_for(
    ws: &Workspace,
    uri: &lsp::Url,
    source_file: SourceFile,
    name: &str,
    def_span: Span,
    full_span: Span,
) -> lsp::TypeHierarchyItem {
    let text = source_file.text(&ws.db);
    let line_table = LineTable::new(text);
    let pm = PosMap::new(&line_table, text);
    lsp::TypeHierarchyItem {
        name: name.to_string(),
        kind: lsp::SymbolKind::CLASS,
        tags: None,
        detail: None,
        uri: uri.clone(),
        range: span_to_range(pm, full_span),
        selection_range: span_to_range(pm, def_span),
        data: None,
    }
}
