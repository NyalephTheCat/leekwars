//! `textDocument/codeLens` — inline annotations above declarations.
//!
//! For each user function we emit two lenses:
//!  1. `N references` (counts `ResolvedRef`s targeting it).
//!  2. `Complexity: O(...)` (from `leek_complexity::analyze_file`).
//!
//! Both are non-clickable today (`command.command == ""`), so the
//! editor renders them as plain inline text. A future slice can
//! wire them to "show references" / "show complexity formula"
//! commands.

use leek_complexity::analyze_file;
use leek_hir::pipeline::HirArtifact;
use leek_resolver::SymbolKind;
use tower_lsp::lsp_types as lsp;

use crate::util::position::span_to_range;
use crate::workspace::Workspace;

pub fn handle(ws: &Workspace, uri: &lsp::Url) -> Option<Vec<lsp::CodeLens>> {
    let _ = uri;
    let doc = ws.doc(uri)?;
    let run = crate::pipeline::run(ws, uri, leek_recipes::Target::Hir)?;
    let table = &run.get::<leek_resolver::pipeline::ResolveArtifact>()?.table;
    let hir = run.get::<HirArtifact>()?;
    let complexities = analyze_file(&hir.0);

    let mut out: Vec<lsp::CodeLens> = Vec::new();
    for sym in &table.symbols {
        if sym.kind != SymbolKind::Function {
            continue;
        }
        let range = span_to_range(doc.pos_map(), sym.def_span);
        let ref_count = table
            .references
            .iter()
            .filter(|r| r.target == sym.id)
            .count();
        // The "N references" lens dispatches a client-side
        // `editor.action.showReferences`-shaped command via our
        // `leek.showReferences` wrapper. The args are (uri,
        // position) — VS Code's reference peek opens at that
        // location.
        out.push(lsp::CodeLens {
            range,
            command: Some(lsp::Command {
                title: format!(
                    "{} reference{}",
                    ref_count,
                    if ref_count == 1 { "" } else { "s" },
                ),
                command: "leek.showReferences".into(),
                arguments: Some(vec![
                    serde_json::Value::String(uri.to_string()),
                    serde_json::json!({
                        "line": range.start.line,
                        "character": range.start.character,
                    }),
                ]),
            }),
            data: None,
        });
        if let Some(c) = complexities.iter().find(|c| c.name == sym.name) {
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
