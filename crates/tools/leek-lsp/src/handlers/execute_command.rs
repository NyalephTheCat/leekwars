//! `workspace/executeCommand` — server-side commands the editor
//! can invoke directly.
//!
//! Currently registered:
//! - `leek.showComplexity` — returns the full ops formula for a
//!   function as a string. Bound to the "Complexity: O(...)" lens.
//! - `leek.analyze` — returns per-function complexity records for
//!   the current document, mirroring `miku analyze`.
//!
//! `leek.showReferences` — the command the "N references" code lens
//! resolves to — is deliberately *not* here. It is a client-side
//! command (the VS Code extension turns it into
//! `editor.action.showReferences`); advertising it would make
//! vscode-languageclient register a proxy under the same id and collide
//! with the extension's own registration.
//!
//! Result type is `serde_json::Value` so the client receives a
//! tagged JSON payload. A plain-string answer is additionally pushed to
//! the user as a `window/showMessage` by the server method — an editor
//! discards an `executeCommand` result it did not ask for, so a lens
//! click would otherwise show nothing.

use serde_json::Value as Json;
use tower_lsp::lsp_types as lsp;

use crate::workspace::Workspace;

/// The set of commands we advertise. Listed in `executeCommandProvider`.
pub const COMMANDS: &[&str] = &["leek.showComplexity", "leek.analyze"];

pub fn handle(ws: &Workspace, command: &str, args: &[Json]) -> Option<Json> {
    match command {
        "leek.showComplexity" => show_complexity(ws, args),
        "leek.analyze" => analyze(ws, args),
        _ => None,
    }
}

/// `leek.showComplexity (uri, function_name)` → returns the ops
/// formula as a string (or "O(?)").
fn show_complexity(ws: &Workspace, args: &[Json]) -> Option<Json> {
    let uri_str = args.first()?.as_str()?;
    let fn_name = args.get(1)?.as_str()?;
    let uri = lsp::Url::parse(uri_str).ok()?;
    let doc = ws.doc(&uri)?;
    let report = crate::analysis::complexity(&ws.db, doc.source_file);
    let c = report.0.iter().find(|c| c.name == fn_name)?;
    Some(Json::String(format!("{} — ops: {}", c.big_o, c.formula)))
}

/// `leek.analyze (uri)` → JSON array of `{ name, params, big_o,
/// formula }` objects per user function.
fn analyze(ws: &Workspace, args: &[Json]) -> Option<Json> {
    let uri_str = args.first()?.as_str()?;
    let uri = lsp::Url::parse(uri_str).ok()?;
    let doc = ws.doc(&uri)?;
    let report = crate::analysis::complexity(&ws.db, doc.source_file);
    let entries: Vec<Json> = report
        .0
        .iter()
        .map(|c| {
            let params: Vec<Json> = c
                .params
                .iter()
                .map(|p| Json::String(p.name.clone()))
                .collect();
            serde_json::json!({
                "name": c.name,
                "params": params,
                "big_o": c.big_o.to_string(),
                "formula": c.formula.to_string(),
            })
        })
        .collect();
    Some(Json::Array(entries))
}
