//! `textDocument/prepareCallHierarchy` + `callHierarchy/incomingCalls`
//! + `callHierarchy/outgoingCalls`, resolved across the whole *program*.
//!
//! Because the LSP resolves each file in isolation, a call that crosses
//! an `include` boundary doesn't bind locally. We close that the same
//! way references/rename do: enumerate the files in the symbol's program
//! ([`program_scope`](super::program_scope)) and, in each, find call
//! sites by identifier (a top-level function name is unique per program,
//! so an unbound `f(...)` can only mean that function). Each call site is
//! attributed to the HIR function whose body span contains it.
//!
//! - `incoming` — for every file in the program, every call site of the
//!   target maps to its enclosing function (the caller).
//! - `outgoing` — within the target's own body, every call to a
//!   top-level function (in any file) is a callee; the callee item points
//!   at its declaring file.

use std::collections::HashMap;

use leek_hir::Def;
use leek_hir::pipeline::HirArtifact;
use leek_pipeline::salsa::SourceFile;
use leek_resolver::{ResolveTable, SymbolKind};
use leek_span::{LineTable, Span};
use leek_syntax::{SyntaxKind, SyntaxNode, language::NodeOrToken};
use tower_lsp::lsp_types as lsp;

use crate::util::position::{PosMap, position_to_offset, span_to_range};
use crate::workspace::Workspace;

/// Resolve the cursor → a function (local or cross-file) → an item.
pub fn prepare(
    ws: &Workspace,
    uri: &lsp::Url,
    pos: lsp::Position,
) -> Option<Vec<lsp::CallHierarchyItem>> {
    let doc = ws.doc(uri)?;
    let offset = position_to_offset(doc.pos_map(), pos)?;
    let run = crate::pipeline::run(ws, uri, leek_recipes::Target::Resolved)?;
    let table = &run.get::<leek_resolver::pipeline::ResolveArtifact>()?.table;

    if let Some(sym) = crate::handlers::resolve_symbol(table, offset)
        && sym.kind == SymbolKind::Function
    {
        return Some(vec![item_for(
            ws,
            uri,
            doc.source_file,
            &sym.name,
            sym.def_span,
            sym.full_span,
        )]);
    }

    // Cross-file: the cursor is on a call to a function declared in an
    // `include`d file.
    let green = &run.get::<leek_parser::pipeline::GreenTreeArtifact>()?.0;
    let root = SyntaxNode::new_root(green.clone());
    let name = crate::handlers::ident_name_at(&root, offset)?;
    let (file, sym) = crate::handlers::find_top_level_decl(ws, uri, &name)?;
    if sym.kind != SymbolKind::Function {
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

/// Callers of `item`, anywhere in its program. Each caller is the HIR
/// function enclosing a call site; the ranges are the call sites in the
/// caller's file.
pub fn incoming(
    ws: &Workspace,
    _uri: &lsp::Url,
    item: &lsp::CallHierarchyItem,
) -> Option<Vec<lsp::CallHierarchyIncomingCall>> {
    let target = item.name.as_str();
    // (caller uri, caller name) → (caller item, call-site ranges).
    let mut buckets: HashMap<(String, String), (lsp::CallHierarchyItem, Vec<lsp::Range>)> =
        HashMap::new();

    for file in crate::handlers::program_scope::program_scope(ws, &item.uri) {
        let Some(run) =
            crate::pipeline::run_on_file(ws, file.source_file, leek_recipes::Target::Hir)
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
        let fns = hir_functions(&hir.0);

        let occs = crate::handlers::occurrences_in_file(
            ws,
            &file.uri,
            file.source_file,
            target,
            SymbolKind::Function,
        );
        for occ in occs.iter().filter(|o| !o.is_declaration) {
            // Which function's body contains this call site?
            let Some((caller_name, caller_span)) = fns
                .iter()
                .find(|(_, span)| span.start <= occ.start && occ.start < span.end)
            else {
                continue; // top-level call (no enclosing function)
            };
            let Some(caller_sym) = top_level_fn_symbol(table, caller_name, *caller_span) else {
                continue;
            };
            let from = item_for(
                ws,
                &file.uri,
                file.source_file,
                caller_name,
                caller_sym.def_span,
                caller_sym.full_span,
            );
            buckets
                .entry((file.uri.to_string(), caller_name.clone()))
                .or_insert_with(|| (from, Vec::new()))
                .1
                .push(occ.range);
        }
    }

    Some(
        buckets
            .into_values()
            .map(|(from, from_ranges)| lsp::CallHierarchyIncomingCall { from, from_ranges })
            .collect(),
    )
}

/// Callees of `item`: every top-level function called from its body,
/// including functions declared in `include`d files.
pub fn outgoing(
    ws: &Workspace,
    _uri: &lsp::Url,
    item: &lsp::CallHierarchyItem,
) -> Option<Vec<lsp::CallHierarchyOutgoingCall>> {
    let scope = crate::handlers::program_scope::program_scope(ws, &item.uri);
    let home = scope.iter().find(|f| f.uri == item.uri)?;
    let run = crate::pipeline::run_on_file(ws, home.source_file, leek_recipes::Target::Hir)?;
    let table = &run.get::<leek_resolver::pipeline::ResolveArtifact>()?.table;
    let hir = run.get::<HirArtifact>()?;
    let green = &run.get::<leek_parser::pipeline::GreenTreeArtifact>()?.0;
    let root = SyntaxNode::new_root(green.clone());

    // The caller's body span (the resolver's full_span is the ident
    // token only, so use the HIR function span).
    let caller_span = hir.0.defs.iter().find_map(|d| match d {
        Def::Function(f) if f.name == item.name => Some(f.span),
        _ => None,
    })?;

    // Every top-level function in the program, for building callee items.
    let funcs = program_functions(ws, &scope);

    let text = home.source_file.text(&ws.db);
    let line_table = LineTable::new(text);
    let pm = PosMap::new(&line_table, text);

    // callee name → call-site ranges within the caller's body.
    let mut buckets: HashMap<String, Vec<lsp::Range>> = HashMap::new();
    for tok in root
        .descendants_with_tokens()
        .filter_map(NodeOrToken::into_token)
    {
        if tok.kind() != SyntaxKind::Ident {
            continue;
        }
        let start = u32::from(tok.text_range().start());
        if start < caller_span.start || start >= caller_span.end {
            continue;
        }
        if crate::handlers::preceded_by_dot(&tok) {
            continue; // member call, not a free function
        }
        // Resolve the callee: a locally-bound function, or an unbound
        // name that the program knows as a top-level function.
        let callee = if let Some(r) = table.reference_at(start) {
            table
                .symbol(r.target)
                .filter(|s| s.kind == SymbolKind::Function)
                .map(|s| s.name.clone())
        } else {
            let name = tok.text();
            funcs.contains_key(name).then(|| name.to_string())
        };
        let Some(callee) = callee else { continue };
        let end = u32::from(tok.text_range().end());
        buckets.entry(callee).or_default().push(span_to_range(
            pm,
            Span::new(home.source_file.source(&ws.db), start, end),
        ));
    }

    let mut out = Vec::new();
    for (callee, from_ranges) in buckets {
        let Some(fi) = funcs.get(&callee) else {
            continue;
        };
        let to = item_for(
            ws,
            &fi.uri,
            fi.source_file,
            &callee,
            fi.def_span,
            fi.full_span,
        );
        out.push(lsp::CallHierarchyOutgoingCall { to, from_ranges });
    }
    Some(out)
}

/// A top-level function's declaring file and spans, for callee items.
struct FuncInfo {
    uri: lsp::Url,
    source_file: SourceFile,
    def_span: Span,
    full_span: Span,
}

/// Index every top-level function across the program by name. HIR
/// `Def::Function`s are top-level only (methods live under classes), so
/// this excludes methods even when a method shares a function's name.
fn program_functions(
    ws: &Workspace,
    scope: &[crate::handlers::program_scope::ScopeFile],
) -> HashMap<String, FuncInfo> {
    let mut out: HashMap<String, FuncInfo> = HashMap::new();
    for file in scope {
        let Some(run) =
            crate::pipeline::run_on_file(ws, file.source_file, leek_recipes::Target::Hir)
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
            let Def::Function(f) = def else { continue };
            if let Some(sym) = top_level_fn_symbol(table, &f.name, f.span) {
                out.entry(f.name.clone()).or_insert(FuncInfo {
                    uri: file.uri.clone(),
                    source_file: file.source_file,
                    def_span: sym.def_span,
                    full_span: sym.full_span,
                });
            }
        }
    }
    out
}

/// (name, body span) for every top-level HIR function in a file.
fn hir_functions(hir: &leek_hir::HirFile) -> Vec<(String, Span)> {
    hir.defs
        .iter()
        .filter_map(|d| match d {
            Def::Function(f) => Some((f.name.clone(), f.span)),
            _ => None,
        })
        .collect()
}

/// The resolver `Symbol` for a top-level function named `name` whose
/// declaration sits inside `fn_span` — disambiguates a top-level
/// function from a same-named method (whose def lives in a class).
fn top_level_fn_symbol<'a>(
    table: &'a ResolveTable,
    name: &str,
    fn_span: Span,
) -> Option<&'a leek_resolver::Symbol> {
    table.symbols.iter().find(|s| {
        s.kind == SymbolKind::Function
            && s.name == name
            && fn_span.start <= s.def_span.start
            && s.def_span.start < fn_span.end
    })
}

fn item_for(
    ws: &Workspace,
    uri: &lsp::Url,
    source_file: SourceFile,
    name: &str,
    def_span: Span,
    full_span: Span,
) -> lsp::CallHierarchyItem {
    let text = source_file.text(&ws.db);
    let line_table = LineTable::new(text);
    let pm = PosMap::new(&line_table, text);
    lsp::CallHierarchyItem {
        name: name.to_string(),
        kind: lsp::SymbolKind::FUNCTION,
        tags: None,
        detail: None,
        uri: uri.clone(),
        range: span_to_range(pm, full_span),
        selection_range: span_to_range(pm, def_span),
        data: None,
    }
}
