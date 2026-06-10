//! Convert `leek-diagnostics` diagnostics to `lsp_types`.

use leek_diagnostics::{Diagnostic as LeekDiagnostic, Severity};
use tower_lsp::lsp_types as lsp;

use crate::util::position::{PosMap, span_to_range};

/// Map a Leek diagnostic to the LSP wire shape, including catalog
/// metadata and secondary labels as `relatedInformation`.
pub fn to_lsp(diag: &LeekDiagnostic, pm: PosMap<'_>, uri: Option<&lsp::Url>) -> lsp::Diagnostic {
    let code_description = diag
        .code
        .meta()
        .and_then(|_| {
            lsp::Url::parse("https://github.com/chloe/leekscript-rs/blob/main/doc/diagnostics.md")
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
        source: Some("leek".into()),
        message: diag.message.clone(),
        related_information,
        tags: None,
        data: None,
    }
}
