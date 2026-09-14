//! `textDocument/codeAction` — surface diagnostic suggestions as
//! LSP code actions.
//!
//! Two kinds of action are produced:
//!
//! - **Quick fixes** (`quickfix`): one per [`Suggestion`] attached to a
//!   diagnostic that overlaps the requested range. Applying it runs that
//!   single suggestion's edits.
//! - **Fix all** (`source.fixAll`): a single action that applies *every*
//!   machine-applicable suggestion in the file at once — what editors
//!   invoke for "Fix all auto-fixable problems" and on-save cleanup.
//!
//! Each suggestion is validated through [`leek_rewrite::EditSet`] so
//! internally-conflicting suggestions (overlapping edits, out-of-bounds
//! spans) are dropped silently rather than producing invalid
//! `WorkspaceEdit`s. For the fix-all action, suggestions are merged
//! greedily in diagnostic order; a later suggestion whose edits would
//! collide with one already accepted is skipped (the user can still
//! apply it individually as a quick fix).
//!
//! Both kinds read the *published* diagnostic set — the one
//! [`file_diagnostics`] hands to push and pull — never a private
//! re-analysis. A per-file run without include resolution reports
//! undefined symbols and type errors for everything an include provides;
//! offering quick fixes for those, and letting `source.fixAll` apply them
//! on save, "fixes" problems the user was never shown.
//!
//! Clients may narrow the request via [`CodeActionContext::only`]; we
//! honor that filter using LSP's hierarchical kind matching (a requested
//! `source` matches `source.fixAll`). Quick fixes are additionally
//! matched against [`CodeActionContext::diagnostics`] — the diagnostics
//! the client is actually showing at the range — so a set that has gone
//! stale on our side cannot produce a fix for a squiggle that is no
//! longer there.
//!
//! [`Suggestion`]: leek_diagnostics::Suggestion
//! [`CodeActionContext::only`]: lsp::CodeActionContext
//! [`CodeActionContext::diagnostics`]: lsp::CodeActionContext

use std::collections::HashMap;

use leek_diagnostics::{Applicability, Diagnostic, TextEdit as DiagEdit};
use leek_rewrite::EditSet;
use leek_span::{SourceId, Span};
use tower_lsp::lsp_types as lsp;

use crate::diagnostics::{SOURCE, file_diagnostics, to_lsp};
use crate::util::position::{position_to_offset, span_to_range};
use crate::workspace::Workspace;

pub fn handle(
    ws: &Workspace,
    uri: &lsp::Url,
    range: lsp::Range,
    context: &lsp::CodeActionContext,
) -> Option<Vec<lsp::CodeActionOrCommand>> {
    let doc = ws.doc(uri)?;
    let req_start = position_to_offset(doc.pos_map(), range.start)?;
    let req_end = position_to_offset(doc.pos_map(), range.end)?;

    // The diagnostics the client is showing for this document — exactly
    // the set push and pull publish, so a fix is never offered for a
    // problem the editor does not display.
    let diags = file_diagnostics(ws, uri, doc.source_file);

    let source_len = doc.text.len();
    let source_id = doc.source_file_source_id(&ws.db);
    let mut actions: Vec<lsp::CodeActionOrCommand> = Vec::new();

    let only = context.only.as_ref();
    // Built once: a label's file is looked up per diagnostic, and the
    // workspace does not change inside one request.
    let label_sources = crate::diagnostics::LabelSources::from_workspace(ws);

    // ---- Quick fixes (range-scoped) ----
    if kind_requested(only, &lsp::CodeActionKind::QUICKFIX) {
        let client_diags = our_client_diagnostics(context);
        for diag in &diags {
            if !ranges_overlap(diag.span.start, diag.span.end, req_start, req_end) {
                continue;
            }
            let lsp_diag = to_lsp(diag, doc.pos_map(), Some(uri), &label_sources);
            if !client_shows(&client_diags, &lsp_diag) {
                continue;
            }
            for sug in &diag.suggestions {
                let Some(lsp_edits) =
                    validate_edits(&sug.edits, source_len, source_id, doc.pos_map())
                else {
                    continue;
                };
                actions.push(lsp::CodeActionOrCommand::CodeAction(lsp::CodeAction {
                    title: sug.message.clone(),
                    kind: Some(lsp::CodeActionKind::QUICKFIX),
                    diagnostics: Some(vec![lsp_diag.clone()]),
                    edit: Some(single_file_edit(uri, lsp_edits)),
                    command: None,
                    is_preferred: matches!(sug.applicability, Applicability::MachineApplicable)
                        .then_some(true),
                    disabled: None,
                    data: None,
                }));
            }
        }
    }

    // ---- Fix all (whole-file, machine-applicable only) ----
    // Not narrowed by `context.diagnostics`: fix-all covers the document,
    // while the client only sends the diagnostics at the request range.
    if kind_requested(only, &lsp::CodeActionKind::SOURCE_FIX_ALL) {
        let merged = collect_fix_all_edits(&diags, source_len, source_id);
        if !merged.is_empty()
            && let Some(lsp_edits) = validate_edits(&merged, source_len, source_id, doc.pos_map())
        {
            actions.push(lsp::CodeActionOrCommand::CodeAction(lsp::CodeAction {
                title: format!("Fix all auto-fixable problems ({})", merged.len()),
                kind: Some(lsp::CodeActionKind::SOURCE_FIX_ALL),
                diagnostics: None,
                edit: Some(single_file_edit(uri, lsp_edits)),
                command: None,
                is_preferred: None,
                disabled: None,
                data: None,
            }));
        }
    }

    Some(actions)
}

/// Greedily merge every machine-applicable suggestion across `diags`
/// into a single, conflict-free edit list. Suggestions are considered
/// in diagnostic order; one whose edits would overlap an
/// already-accepted edit is skipped, as is one that would edit a file
/// other than `source` (the whole set is applied to a single document).
/// Pure (no I/O) so it can be unit tested directly.
fn collect_fix_all_edits<'a>(
    diags: impl IntoIterator<Item = &'a Diagnostic>,
    source_len: usize,
    source: SourceId,
) -> Vec<DiagEdit> {
    let mut accepted: Vec<DiagEdit> = Vec::new();
    for diag in diags {
        for sug in &diag.suggestions {
            if !matches!(sug.applicability, Applicability::MachineApplicable) {
                continue;
            }
            if sug.edits.iter().any(|e| e.span.source != source) {
                continue;
            }
            // Validate `accepted + this suggestion` as a whole: if the
            // combined set is conflict-free, commit this suggestion.
            let mut set = EditSet::new(source_len);
            let combined = accepted.iter().chain(sug.edits.iter());
            if combined.map(|e| set.push_diag_edit(e)).all(|r| r.is_ok()) {
                accepted.extend(sug.edits.iter().cloned());
            }
        }
    }
    accepted
}

/// Validate `edits` against a fresh [`EditSet`] and convert the
/// accepted, normalized result to LSP `TextEdit`s. Returns `None` if
/// the set ends up empty, an edit targets a file other than `source_id`
/// (every edit we emit is applied to this one document), or any edit was
/// rejected.
fn validate_edits(
    edits: &[DiagEdit],
    source_len: usize,
    source_id: SourceId,
    pm: crate::util::position::PosMap<'_>,
) -> Option<Vec<lsp::TextEdit>> {
    let mut set = EditSet::new(source_len);
    for e in edits {
        if e.span.source != source_id || set.push_diag_edit(e).is_err() {
            return None;
        }
    }
    if set.is_empty() {
        return None;
    }
    Some(
        set.iter()
            .map(|e| lsp::TextEdit {
                range: span_to_range(pm, Span::new(source_id, e.start, e.end)),
                new_text: e.replacement.clone(),
            })
            .collect(),
    )
}

fn single_file_edit(uri: &lsp::Url, edits: Vec<lsp::TextEdit>) -> lsp::WorkspaceEdit {
    let mut changes = HashMap::new();
    changes.insert(uri.clone(), edits);
    lsp::WorkspaceEdit {
        changes: Some(changes),
        document_changes: None,
        change_annotations: None,
    }
}

/// The client's own view of the diagnostics at the requested range,
/// narrowed to the ones we published. Entries from other language
/// servers say nothing about our quick fixes.
fn our_client_diagnostics(context: &lsp::CodeActionContext) -> Vec<&lsp::Diagnostic> {
    context
        .diagnostics
        .iter()
        .filter(|d| d.source.as_deref() == Some(SOURCE))
        .collect()
}

/// Whether the client is showing `ours`, so a quick fix for it is worth
/// offering. When the client sent none of our diagnostics — it may not
/// populate the field at all — we cannot match and fall back to offering
/// the fix. Matching is by range plus code: the client echoes back the
/// diagnostic object it received, but some drop fields they don't use,
/// so a code missing on either side is not a mismatch.
fn client_shows(client: &[&lsp::Diagnostic], ours: &lsp::Diagnostic) -> bool {
    client.is_empty()
        || client.iter().any(|c| {
            c.range == ours.range
                && match (&c.code, &ours.code) {
                    (Some(theirs), Some(mine)) => theirs == mine,
                    _ => true,
                }
        })
}

/// Honor `CodeActionContext.only`. With no filter, every kind is
/// offered. Matching is hierarchical per the LSP spec: a requested
/// `source` also selects `source.fixAll`, etc.
fn kind_requested(only: Option<&Vec<lsp::CodeActionKind>>, kind: &lsp::CodeActionKind) -> bool {
    match only {
        None => true,
        Some(kinds) => kinds
            .iter()
            .any(|req| kind_matches(req.as_str(), kind.as_str())),
    }
}

/// True when `requested` selects `actual` under LSP's dotted-prefix
/// hierarchy: equal, or `actual` is a sub-kind (`requested` followed by
/// a `.`).
fn kind_matches(requested: &str, actual: &str) -> bool {
    actual == requested
        || (actual.len() > requested.len()
            && actual.starts_with(requested)
            && actual.as_bytes()[requested.len()] == b'.')
}

/// Half-open range overlap. Two ranges overlap iff each starts
/// before the other ends.
fn ranges_overlap(a_start: u32, a_end: u32, b_start: u32, b_end: u32) -> bool {
    a_start < b_end && b_start < a_end
}

#[cfg(test)]
mod tests {
    use super::*;
    use leek_diagnostics::{Suggestion, codes};

    fn source() -> SourceId {
        SourceId::new(1).unwrap()
    }

    fn edit(start: u32, end: u32, repl: &str) -> DiagEdit {
        DiagEdit {
            span: Span::new(source(), start, end),
            replacement: repl.to_string(),
        }
    }

    fn diag_with(sug: Suggestion) -> Diagnostic {
        let mut d = Diagnostic::warning(codes::UNUSED_VARIABLE, Span::new(source(), 0, 1), "x");
        d.suggestions.push(sug);
        d
    }

    fn lsp_diag(line: u32, code: Option<&str>, src: Option<&str>) -> lsp::Diagnostic {
        lsp::Diagnostic {
            range: lsp::Range {
                start: lsp::Position { line, character: 0 },
                end: lsp::Position { line, character: 4 },
            },
            code: code.map(|c| lsp::NumberOrString::String(c.to_string())),
            source: src.map(str::to_string),
            ..lsp::Diagnostic::default()
        }
    }

    #[test]
    fn merges_non_overlapping_machine_applicable() {
        let a = diag_with(Suggestion {
            message: "a".into(),
            edits: vec![edit(0, 2, "")],
            applicability: Applicability::MachineApplicable,
        });
        let b = diag_with(Suggestion {
            message: "b".into(),
            edits: vec![edit(5, 7, "X")],
            applicability: Applicability::MachineApplicable,
        });
        let merged = collect_fix_all_edits([&a, &b], 10, source());
        assert_eq!(merged.len(), 2, "{merged:?}");
    }

    #[test]
    fn skips_overlapping_second_suggestion() {
        let a = diag_with(Suggestion {
            message: "a".into(),
            edits: vec![edit(0, 5, "")],
            applicability: Applicability::MachineApplicable,
        });
        let b = diag_with(Suggestion {
            message: "b".into(),
            edits: vec![edit(3, 8, "X")], // overlaps a
            applicability: Applicability::MachineApplicable,
        });
        let merged = collect_fix_all_edits([&a, &b], 10, source());
        assert_eq!(merged.len(), 1, "second should be skipped: {merged:?}");
        assert_eq!(merged[0].span.end, 5);
    }

    #[test]
    fn excludes_non_machine_applicable() {
        let a = diag_with(Suggestion {
            message: "a".into(),
            edits: vec![edit(0, 2, "")],
            applicability: Applicability::MaybeIncorrect,
        });
        let merged = collect_fix_all_edits([&a], 10, source());
        assert!(merged.is_empty(), "{merged:?}");
    }

    #[test]
    fn excludes_edits_in_another_file() {
        // A suggestion whose edits live in an include must never be
        // folded into this document's fix-all edit.
        let other = SourceId::new(2).unwrap();
        let a = diag_with(Suggestion {
            message: "a".into(),
            edits: vec![DiagEdit {
                span: Span::new(other, 0, 2),
                replacement: String::new(),
            }],
            applicability: Applicability::MachineApplicable,
        });
        let merged = collect_fix_all_edits([&a], 10, source());
        assert!(merged.is_empty(), "{merged:?}");
    }

    #[test]
    fn client_diagnostics_gate_quick_fixes() {
        let ours = lsp_diag(1, Some("L0012"), Some(SOURCE));
        // Same range, same code: the client is showing this one.
        assert!(client_shows(
            &[&lsp_diag(1, Some("L0012"), Some(SOURCE))],
            &ours
        ));
        // Same range, different code — a different problem.
        assert!(!client_shows(
            &[&lsp_diag(1, Some("L0001"), Some(SOURCE))],
            &ours
        ));
        // Different range.
        assert!(!client_shows(
            &[&lsp_diag(2, Some("L0012"), Some(SOURCE))],
            &ours
        ));
        // A client that drops the code still matches on range alone.
        assert!(client_shows(&[&lsp_diag(1, None, Some(SOURCE))], &ours));
        // Nothing to match against: offer the fix.
        assert!(client_shows(&[], &ours));
    }

    #[test]
    fn other_servers_diagnostics_are_not_matched_against() {
        // Only our own diagnostics say anything about our quick fixes;
        // a context carrying just another server's must not gate them.
        let context = lsp::CodeActionContext {
            diagnostics: vec![lsp_diag(1, Some("E123"), Some("rust-analyzer"))],
            ..Default::default()
        };
        let mine = our_client_diagnostics(&context);
        assert!(mine.is_empty());
        assert!(client_shows(
            &mine,
            &lsp_diag(1, Some("L0012"), Some(SOURCE))
        ));
    }

    #[test]
    fn kind_matching_is_hierarchical() {
        assert!(kind_matches("source", "source.fixAll"));
        assert!(kind_matches("source.fixAll", "source.fixAll"));
        assert!(kind_matches("quickfix", "quickfix"));
        assert!(!kind_matches("source.fixAll", "source"));
        assert!(!kind_matches("source", "sourcery")); // prefix but not a sub-kind
        assert!(!kind_matches("quickfix", "source.fixAll"));
    }

    #[test]
    fn no_filter_requests_everything() {
        assert!(kind_requested(None, &lsp::CodeActionKind::QUICKFIX));
        assert!(kind_requested(None, &lsp::CodeActionKind::SOURCE_FIX_ALL));
    }

    #[test]
    fn source_only_excludes_quickfix() {
        let only = vec![lsp::CodeActionKind::SOURCE_FIX_ALL];
        assert!(!kind_requested(Some(&only), &lsp::CodeActionKind::QUICKFIX));
        assert!(kind_requested(
            Some(&only),
            &lsp::CodeActionKind::SOURCE_FIX_ALL
        ));
    }
}
