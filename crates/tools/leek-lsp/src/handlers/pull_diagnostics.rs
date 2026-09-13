//! `textDocument/diagnostic` and `workspace/diagnostic` — modern
//! pull-style diagnostic API.
//!
//! In pull mode the client asks "what diagnostics does this
//! document have right now?" rather than waiting for the server
//! to push them. We respond with a full report containing the
//! same diagnostic set our `publishDiagnostics` push would emit.
//!
//! For `workspace/diagnostic` we report on every open document.
//!
//! Diagnostics are the heaviest and most frequent analysis the server runs
//! (parse, resolve, types, HIR, lint, over the whole include closure — on
//! every keystroke for the push path), and they run over buffers that are
//! half-typed by definition. Every collection here therefore goes through
//! [`guard`], so a panic in analysis blanks a single document's report
//! instead of unwinding out of the tower-lsp service future.

use leek_pipeline::salsa::SourceFile;
use tower_lsp::lsp_types as lsp;

use crate::diagnostics::{file_diagnostics, to_lsp};
use crate::util::guard::guard;
use crate::util::position::PosMap;
use crate::workspace::{AnalysisTarget, Workspace};

pub fn handle_textdoc(ws: &Workspace, uri: &lsp::Url) -> lsp::DocumentDiagnosticReportResult {
    document_report("textDocument/diagnostic", || collect(ws, uri))
}

pub fn handle_workspace(ws: &Workspace) -> lsp::WorkspaceDiagnosticReportResult {
    workspace_report(&ws.analysis_targets(), |target| {
        collect_target(ws, target.uri, target.pos_map(), target.source_file)
    })
}

/// Collect one document's diagnostics with analysis panics contained.
///
/// Shared with the push path (`server::publish_diagnostics`), which runs the
/// very same analysis on every edit and must be guarded the same way.
pub fn collect_guarded(ws: &Workspace, uri: &lsp::Url, label: &str) -> Vec<lsp::Diagnostic> {
    guard(label, || collect(ws, uri))
}

/// Collect one document's diagnostics under [`guard`] and shape them as a
/// full document report.
fn document_report(
    label: &str,
    collect: impl FnOnce() -> Vec<lsp::Diagnostic>,
) -> lsp::DocumentDiagnosticReportResult {
    let items = guard(label, collect);
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

/// Shape a workspace report, guarding each file **separately** so one buffer
/// that panics analysis loses only its own entry instead of blanking the
/// report for the whole workspace.
fn workspace_report<'a>(
    targets: &[AnalysisTarget<'a>],
    mut collect: impl FnMut(&AnalysisTarget<'a>) -> Vec<lsp::Diagnostic>,
) -> lsp::WorkspaceDiagnosticReportResult {
    let mut entries: Vec<lsp::WorkspaceDocumentDiagnosticReport> =
        Vec::with_capacity(targets.len());
    for target in targets {
        let items = guard("workspace/diagnostic", || collect(target));
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

/// Convert each `leek_diagnostics::Diagnostic` in the file's published
/// set to its LSP shape.
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
    file_diagnostics(ws, uri, source_file)
        .iter()
        .map(|d| to_lsp(d, pm, Some(uri)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{document_report, workspace_report};
    use crate::workspace::Workspace;
    use tower_lsp::lsp_types as lsp;

    fn uri(name: &str) -> lsp::Url {
        let path = std::env::temp_dir()
            .join("leek-lsp-pull-diagnostics-tests")
            .join(name);
        lsp::Url::from_file_path(path).expect("test path should be a valid file URI")
    }

    fn marker() -> lsp::Diagnostic {
        lsp::Diagnostic {
            message: "marker".into(),
            ..lsp::Diagnostic::default()
        }
    }

    /// Run `f` with the panic hook silenced, so the panics these tests
    /// deliberately provoke don't spam the test output.
    fn quietly<T>(f: impl FnOnce() -> T) -> T {
        let prev = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| {}));
        let out = f();
        std::panic::set_hook(prev);
        out
    }

    #[test]
    fn document_report_survives_a_panic_in_analysis() {
        // Analysis over a half-typed buffer may panic; the request must
        // degrade to an empty report instead of unwinding into tower-lsp.
        let report = quietly(|| document_report("test", || panic!("kaboom")));
        let lsp::DocumentDiagnosticReportResult::Report(lsp::DocumentDiagnosticReport::Full(full)) =
            report
        else {
            panic!("expected a full report");
        };
        assert!(full.full_document_diagnostic_report.items.is_empty());
    }

    #[test]
    fn workspace_report_isolates_a_panicking_file() {
        let mut ws = Workspace::default();
        ws.open(uri("good.leek"), "var x = 1;".to_string());
        ws.open(uri("bad.leek"), "var y = 2;".to_string());
        let targets = ws.analysis_targets();

        let report = quietly(|| {
            workspace_report(&targets, |target| {
                assert!(
                    target.uri != &uri("bad.leek"),
                    "analysis blew up on this file"
                );
                vec![marker()]
            })
        });

        let lsp::WorkspaceDiagnosticReportResult::Report(report) = report else {
            panic!("expected a full workspace report");
        };
        // Every file is still reported on, and only the panicking one is
        // blank — one bad buffer must not blank the whole workspace.
        assert_eq!(report.items.len(), 2);
        for entry in &report.items {
            let lsp::WorkspaceDocumentDiagnosticReport::Full(full) = entry else {
                panic!("expected a full entry");
            };
            let items = &full.full_document_diagnostic_report.items;
            if full.uri == uri("bad.leek") {
                assert!(items.is_empty(), "panicking file should report nothing");
            } else {
                assert_eq!(items.len(), 1, "healthy file should still report");
            }
        }
    }
}
