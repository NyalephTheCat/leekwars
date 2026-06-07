//! `textDocument/diagnostic` and `workspace/diagnostic` — modern
//! pull-style diagnostic API.
//!
//! In pull mode the client asks "what diagnostics does this
//! document have right now?" rather than waiting for the server
//! to push them. We respond with a full report containing the
//! same diagnostic set our `publishDiagnostics` push would emit.
//!
//! For `workspace/diagnostic` we report on every open document.

use leek_pipeline::salsa::SourceFile;
use leek_recipes::Target;
use tower_lsp::lsp_types as lsp;

use crate::diagnostics::to_lsp;
use crate::util::position::PosMap;
use crate::workspace::Workspace;

pub fn handle_textdoc(ws: &Workspace, uri: &lsp::Url) -> lsp::DocumentDiagnosticReportResult {
    let items = collect(ws, uri);
    lsp::DocumentDiagnosticReportResult::Report(lsp::DocumentDiagnosticReport::Full(
        lsp::RelatedFullDocumentDiagnosticReport {
            related_documents: None,
            full_document_diagnostic_report: lsp::FullDocumentDiagnosticReport {
                result_id: None,
                items,
            },
        },
    ))
}

pub fn handle_workspace(ws: &Workspace) -> lsp::WorkspaceDiagnosticReportResult {
    let mut entries: Vec<lsp::WorkspaceDocumentDiagnosticReport> = Vec::new();
    for target in ws.analysis_targets() {
        let items = collect_target(ws, target.uri, target.pos_map(), target.source_file);
        entries.push(lsp::WorkspaceDocumentDiagnosticReport::Full(
            lsp::WorkspaceFullDocumentDiagnosticReport {
                uri: target.uri.clone(),
                version: None,
                full_document_diagnostic_report: lsp::FullDocumentDiagnosticReport {
                    result_id: None,
                    items,
                },
            },
        ));
    }
    lsp::WorkspaceDiagnosticReportResult::Report(lsp::WorkspaceDiagnosticReport { items: entries })
}

/// Run the diagnostic-producing pipeline and convert each
/// `leek_diagnostics::Diagnostic` to its LSP shape.
fn collect(ws: &Workspace, uri: &lsp::Url) -> Vec<lsp::Diagnostic> {
    let Some(doc) = ws.docs.get(uri) else {
        return Vec::new();
    };
    collect_target(ws, uri, doc.pos_map(), doc.source_file)
}

fn collect_target(
    ws: &Workspace,
    uri: &lsp::Url,
    pm: PosMap<'_>,
    source_file: SourceFile,
) -> Vec<lsp::Diagnostic> {
    // Recipe planning can fail; degrade to "no diagnostics" rather than crash.
    // `Linted` runs the lint pass on top of type checking so lint findings
    // surface in pull-model diagnostics too.
    let Some(run) = crate::pipeline::run_on_file(ws, source_file, Target::Linted) else {
        return Vec::new();
    };
    run.diagnostics()
        .iter()
        .map(|d| to_lsp(d, pm, Some(uri)))
        .collect()
}
