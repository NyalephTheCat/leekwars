//! The published diagnostic set, and its conversion to `lsp_types`.

use leek_diagnostics::{Diagnostic as LeekDiagnostic, Severity};
use leek_pipeline::salsa::SourceFile;
use leek_recipes::Target;
use tower_lsp::lsp_types as lsp;

use crate::util::position::{PosMap, span_to_range};
use crate::workspace::Workspace;

/// The `source` field we stamp on every diagnostic we publish. Handlers
/// use it to tell our own diagnostics apart from other servers' when a
/// client hands some back (see `code_action`).
pub const SOURCE: &str = "leek";

/// The diagnostic set for one file — the single source of truth behind
/// push (`publishDiagnostics`), pull (`textDocument/diagnostic`) and
/// `textDocument/codeAction`.
///
/// Analysis covers the whole include closure (`Linted`, so lint findings
/// come with it) and the result is filtered to spans belonging to
/// `source_file` itself: a diagnostic raised inside an include belongs to
/// that include's own document.
///
/// Handlers must not re-derive this set. A per-file run reports
/// undefined-symbol and type errors for everything an include provides,
/// so code actions built on one offer quick fixes — and let
/// `source.fixAll` apply them on save — for problems the editor never
/// showed.
///
/// The pipeline underneath is memoized by salsa, so recomputation across
/// the handlers serving one edit is cheap; only the returned `Vec` is
/// freshly allocated.
pub fn file_diagnostics(
    ws: &Workspace,
    uri: &lsp::Url,
    source_file: SourceFile,
) -> Vec<LeekDiagnostic> {
    let source = source_file.source(&ws.db);
    // Recipe planning can fail; degrade to "no diagnostics" rather than crash.
    let Some(run) = crate::pipeline::run_on_file_with_includes(ws, source_file, Target::Linted)
    else {
        if crate::trace_enabled() {
            eprintln!("leek-lsp: recipe planning failed for {uri}; no diagnostics");
        }
        return Vec::new();
    };
    run.diagnostics()
        .iter()
        .filter(|d| d.span.source == source)
        .cloned()
        .collect()
}

/// Map a Leek diagnostic to the LSP wire shape, including catalog
/// metadata and secondary labels as `relatedInformation`.
pub fn to_lsp(diag: &LeekDiagnostic, pm: PosMap<'_>, uri: Option<&lsp::Url>) -> lsp::Diagnostic {
    // Link the code to its extended write-up when one exists — the
    // `explain/<ID>.md` file the build embeds (and `miku explain`
    // prints). Codes without a write-up get no link.
    let code_description = diag
        .code
        .explain()
        .and_then(|_| {
            lsp::Url::parse(&format!(
                "https://github.com/chloe/leekscript-rs/blob/main/crates/core/leek-diagnostics/explain/{}.md",
                diag.code.id()
            ))
            .ok()
        })
        .map(|href| lsp::CodeDescription { href });

    let related_information = uri.and_then(|doc_uri| {
        if diag.labels.is_empty() {
            return None;
        }
        Some(
            diag.labels
                .iter()
                .map(|label| lsp::DiagnosticRelatedInformation {
                    location: lsp::Location {
                        uri: doc_uri.clone(),
                        range: span_to_range(pm, label.span),
                    },
                    message: label.message.clone(),
                })
                .collect(),
        )
    });

    lsp::Diagnostic {
        range: span_to_range(pm, diag.span),
        severity: Some(match diag.severity {
            Severity::Error => lsp::DiagnosticSeverity::ERROR,
            Severity::Warning => lsp::DiagnosticSeverity::WARNING,
            Severity::Info => lsp::DiagnosticSeverity::INFORMATION,
            Severity::Hint => lsp::DiagnosticSeverity::HINT,
        }),
        code: Some(lsp::NumberOrString::String(diag.code.id().to_string())),
        code_description,
        source: Some(SOURCE.into()),
        message: diag.message.clone(),
        related_information,
        tags: None,
        data: None,
    }
}
