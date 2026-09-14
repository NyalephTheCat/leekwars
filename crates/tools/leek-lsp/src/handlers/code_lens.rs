//! `textDocument/codeLens` — inline annotations above declarations.
//!
//! For each user function we emit two lenses:
//!  1. `N references`, sent *unresolved*. The count is program-wide, so
//!     computing it means resolving every file that shares a program
//!     with this one — far too much for a request the editor fires on
//!     every scroll. [`resolve`] fills in the title and the command for
//!     the handful of lenses the editor is about to draw.
//!  2. `Complexity: O(...)` / `Cost: N operations` (from
//!     [`leek_complexity::analyze_file`]), matched to the declaration by
//!     span so a method never borrows a same-named free function's
//!     record.
//!
//! The resolved reference lens carries `leek.showReferences` with the
//! `(uri, position, locations)` triple `editor.action.showReferences`
//! needs; the VS Code extension registers that command and opens the
//! peek view. The complexity lens carries `leek.showComplexity`, which
//! the server answers in `workspace/executeCommand`.

use leek_complexity::{Complexity, analyze_file};
use leek_hir::pipeline::HirArtifact;
use leek_resolver::pipeline::ResolveArtifact;
use leek_resolver::{Symbol, SymbolKind};
use leek_span::Span;
use tower_lsp::lsp_types as lsp;

use crate::util::position::span_to_range;
use crate::workspace::Workspace;

pub fn handle(ws: &Workspace, uri: &lsp::Url) -> Option<Vec<lsp::CodeLens>> {
    let doc = ws.doc(uri)?;
    let run = crate::pipeline::run(ws, uri, leek_recipes::Target::Hir)?;
    let table = &run.get::<ResolveArtifact>()?.table;
    let hir = run.get::<HirArtifact>()?;
    let complexities = analyze_file(&hir.0);

    let mut out: Vec<lsp::CodeLens> = Vec::new();
    for sym in &table.symbols {
        if sym.kind != SymbolKind::Function {
            continue;
        }
        let range = span_to_range(doc.pos_map(), sym.def_span);
        // No command and no title yet — `resolve` re-finds the symbol by
        // the offset stashed in `data`.
        out.push(lsp::CodeLens {
            range,
            command: None,
            data: Some(serde_json::json!({
                "uri": uri.to_string(),
                "symbol_offset": sym.def_span.start,
            })),
        });
        if let Some(c) = complexity_for(&complexities, sym) {
            // For a constant-cost function/method the operation count is
            // more useful than `O(1)` — show the cost directly (mirrors
            // the hover row).
            let title = if matches!(c.big_o, leek_complexity::BigO::Constant) {
                format!("Cost: {} operations", c.formula)
            } else {
                format!("Complexity: {}", c.big_o)
            };
            out.push(lsp::CodeLens {
                range,
                command: Some(lsp::Command {
                    title,
                    command: "leek.showComplexity".into(),
                    arguments: Some(vec![
                        serde_json::Value::String(uri.to_string()),
                        serde_json::Value::String(sym.name.clone()),
                    ]),
                }),
                data: None,
            });
        }
    }
    Some(out)
}

/// `codeLens/resolve` — count the symbol's references and attach the
/// command that opens the editor's reference peek.
pub fn resolve(ws: &Workspace, lens: lsp::CodeLens) -> Option<lsp::CodeLens> {
    let data = lens.data.as_ref()?;
    let uri = lsp::Url::parse(data.get("uri")?.as_str()?).ok()?;
    let offset = u32::try_from(data.get("symbol_offset")?.as_u64()?).ok()?;
    let doc = ws.doc(&uri)?;
    let run = crate::pipeline::run(ws, &uri, leek_recipes::Target::Resolved)?;
    let table = &run.get::<ResolveArtifact>()?.table;
    let green = &run.get::<leek_parser::pipeline::GreenTreeArtifact>()?.0;
    let root = leek_syntax::SyntaxNode::new_root(green.clone());
    let sym = table
        .symbols
        .iter()
        .find(|s| s.kind == SymbolKind::Function && s.def_span.start == offset)?;

    let locations: Vec<lsp::Location> = if crate::handlers::is_workspace_global(sym.kind)
        && !crate::handlers::symbol_is_class_member(&root, sym)
    {
        // A top-level function shares one flat namespace with every file
        // that includes it, so a library function called from ten AIs
        // must not read "0 references".
        crate::handlers::workspace_occurrences(ws, &uri, &sym.name, sym.kind)
            .into_iter()
            .filter(|o| !o.is_declaration)
            .map(|o| lsp::Location {
                uri: o.uri,
                range: o.range,
            })
            .collect()
    } else {
        // A method's name is *not* in that namespace: fanning out would
        // report every same-named free function's call sites as
        // references to it. Stay in the file, which is a subset of the
        // truth rather than a wrong superset (leekwars#46).
        table
            .references
            .iter()
            .filter(|r| r.target == sym.id)
            .map(|r| {
                let span = Span::new(
                    doc.source_file_source_id(&ws.db),
                    r.name_offset,
                    r.name_offset + r.name_len,
                );
                lsp::Location {
                    uri: uri.clone(),
                    range: span_to_range(doc.pos_map(), span),
                }
            })
            .collect()
    };

    let count = locations.len();
    Some(lsp::CodeLens {
        range: lens.range,
        command: Some(lsp::Command {
            title: format!("{} reference{}", count, if count == 1 { "" } else { "s" },),
            command: "leek.showReferences".into(),
            arguments: Some(vec![
                serde_json::Value::String(uri.to_string()),
                serde_json::json!({
                    "line": lens.range.start.line,
                    "character": lens.range.start.character,
                }),
                serde_json::to_value(&locations).ok()?,
            ]),
        }),
        data: lens.data,
    })
}

/// The complexity record for the declaration `sym` names.
///
/// Matched by span, not by name: [`analyze_file`] registers a method
/// under `Class.method` while a free function keeps its bare name, so a
/// name lookup misses every method and silently hands it a same-named
/// top-level function's record instead. A declaration's span covers its
/// own name, and no two declarations' spans do.
fn complexity_for<'a>(records: &'a [Complexity], sym: &Symbol) -> Option<&'a Complexity> {
    records.iter().find(|c| {
        c.span.is_some_and(|span| {
            span.source == sym.def_span.source
                && span.start <= sym.def_span.start
                && sym.def_span.start < span.end
        })
    })
}
