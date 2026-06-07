//! `workspace/executeCommand` — server-side commands the editor
//! can invoke directly.
//!
//! Currently registered:
//! - `leek.showReferences` — wraps the editor's
//!   `editor.action.showReferences` with a (uri, position) pair.
//!   Bound to the "N references" code lens.
//! - `leek.showComplexity` — returns the full ops formula for a
//!   function as a string. Bound to the "Complexity: O(...)" lens.
//! - `leek.analyze` — returns per-function complexity records for
//!   the current document, mirroring `miku analyze`.
//!
//! Result type is `serde_json::Value` so the client receives a
//! tagged JSON payload. The bare-server response (this is plain
//! `tower-lsp::Result<Option<Value>>`) is what the editor can
//! display or pass to its UI.

use leek_complexity::analyze_file;
use leek_hir::pipeline::HirArtifact;
use serde_json::Value as Json;
use tower_lsp::lsp_types as lsp;

use crate::workspace::Workspace;

/// The set of commands we advertise. Listed in `executeCommandProvider`.
pub const COMMANDS: &[&str] = &["leek.showReferences", "leek.showComplexity", "leek.analyze"];

pub fn handle(ws: &Workspace, command: &str, args: &[Json]) -> Option<Json> {
    match command {
        "leek.showReferences" => Some(json_args_passthrough(args)),
        "leek.showComplexity" => show_complexity(ws, args),
        "leek.analyze" => analyze(ws, args),
        _ => None,
    }
}

/// `leek.showReferences (uri, position)` is just a forwarder — the
/// client's `editor.action.showReferences` does the heavy lifting.
/// We echo the args back as the response so the client wrapper can
/// pass them straight through.
fn json_args_passthrough(args: &[Json]) -> Json {
    Json::Array(args.to_vec())
}

/// `leek.showComplexity (uri, function_name)` → returns the ops
/// formula as a string (or "O(?)").
fn show_complexity(ws: &Workspace, args: &[Json]) -> Option<Json> {
    let uri_str = args.first()?.as_str()?;
    let fn_name = args.get(1)?.as_str()?;
    let uri = lsp::Url::parse(uri_str).ok()?;
    let _doc = ws.doc(&uri)?;
    let run = crate::pipeline::run(ws, &uri, leek_recipes::Target::Hir)?;
    let hir = run.get::<HirArtifact>()?;
    let report = analyze_file(&hir.0);
    let c = report.iter().find(|c| c.name == fn_name)?;
    Some(Json::String(format!("{} — ops: {}", c.big_o, c.formula,)))
}

/// `leek.analyze (uri)` → JSON array of `{ name, params, big_o,
/// formula }` objects per user function.
fn analyze(ws: &Workspace, args: &[Json]) -> Option<Json> {
    let uri_str = args.first()?.as_str()?;
    let uri = lsp::Url::parse(uri_str).ok()?;
    let _doc = ws.doc(&uri)?;
    let run = crate::pipeline::run(ws, &uri, leek_recipes::Target::Hir)?;
    let hir = run.get::<HirArtifact>()?;
    let report = analyze_file(&hir.0);
    let entries: Vec<Json> = report
        .into_iter()
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
