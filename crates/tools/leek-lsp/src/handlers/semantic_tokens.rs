//! `textDocument/semanticTokens/*` — context-aware coloring.
//!
//! Drives off the resolver's [`ResolveTable`]. TextMate is OK at
//! distinguishing keywords / operators / strings, but it can't tell
//! a local `x` from a function `x` from a class `X`. Semantic
//! tokens close that gap: each declared name and each reference to
//! a name gets a token-type-colored span based on its [`SymbolKind`].
//!
//! Three entry points share one collection + encoding core:
//!
//! - [`handle`] — `…/full`, every token in the file. It also stamps a
//!   `result_id` and caches the encoded tokens so a later delta request
//!   can diff against them.
//! - [`handle_range`] — `…/range`, only the tokens whose source span
//!   falls inside the requested range (cheaper for the visible viewport
//!   of a large file).
//! - [`handle_delta`] — `…/full/delta`, the edits that turn the
//!   previously-returned token set into the current one. Falls back to a
//!   full token set when we have no cached baseline for the given
//!   `previous_result_id`.

use leek_resolver::SymbolKind;
use leek_span::Span;
use tower_lsp::lsp_types as lsp;

use crate::util::position::position_to_offset;
use crate::workspace::Workspace;

/// Legend, in fixed order. The client maps each index to a theme
/// color via its `SemanticTokenTypes` table.
pub const TOKEN_TYPES: &[lsp::SemanticTokenType] = &[
    lsp::SemanticTokenType::VARIABLE,
    lsp::SemanticTokenType::PARAMETER,
    lsp::SemanticTokenType::FUNCTION,
    lsp::SemanticTokenType::METHOD,
    lsp::SemanticTokenType::CLASS,
    lsp::SemanticTokenType::PROPERTY,
];

pub const TOKEN_MODIFIERS: &[lsp::SemanticTokenModifier] =
    &[lsp::SemanticTokenModifier::DECLARATION];

pub fn legend() -> lsp::SemanticTokensLegend {
    lsp::SemanticTokensLegend {
        token_types: TOKEN_TYPES.to_vec(),
        token_modifiers: TOKEN_MODIFIERS.to_vec(),
    }
}

fn type_index(kind: SymbolKind) -> Option<u32> {
    Some(match kind {
        SymbolKind::Local | SymbolKind::Global => 0,
        SymbolKind::Param => 1,
        SymbolKind::Function => 2,
        SymbolKind::Class => 4,
        SymbolKind::Field => 5,
        SymbolKind::Builtin => return None,
    })
}

/// One coloring span before LSP delta-encoding:
/// `(byte_offset, byte_length, type_index, modifier_bits)`.
type Entry = (u32, u32, u32, u32);

/// Collect every colorable span in the file, sorted by start offset.
/// Shared by all three request flavors.
fn collect_entries(ws: &Workspace, uri: &lsp::Url) -> Option<Vec<Entry>> {
    let run = crate::pipeline::run(ws, uri, leek_recipes::Target::Resolved)?;
    let table = &run.get::<leek_resolver::pipeline::ResolveArtifact>()?.table;

    let mut entries: Vec<Entry> = Vec::new();
    for sym in &table.symbols {
        let Some(ti) = type_index(sym.kind) else {
            continue;
        };
        let Span { start, end, .. } = sym.def_span;
        entries.push((start, end - start, ti, 1 /* declaration bit */));
    }
    for r in &table.references {
        let kind = table.symbol(r.target).map(|s| s.kind);
        let Some(ti) = kind.and_then(type_index) else {
            continue;
        };
        entries.push((r.name_offset, r.name_len, ti, 0));
    }
    entries.sort_by_key(|e| e.0);
    Some(entries)
}

/// LSP-encode sorted `entries` as relative (delta) tokens.
///
/// Both the column and the token length are emitted in **UTF-16 code
/// units**, as the protocol requires — so we route byte offsets through
/// [`PosMap`] rather than using raw byte columns. Getting this wrong
/// shifts every token after a non-ASCII character on its line (common
/// in leek-wars' French comments and string literals). Names never span
/// a line, so a token's UTF-16 length is just the column delta between
/// its start and end offsets.
fn encode(pm: crate::util::position::PosMap<'_>, entries: &[Entry]) -> Vec<lsp::SemanticToken> {
    let mut data: Vec<lsp::SemanticToken> = Vec::with_capacity(entries.len());
    let mut prev_line: u32 = 0;
    let mut prev_start: u32 = 0;
    for &(offset, length, type_idx, modifier_bits) in entries {
        let start = pm.to_position(offset);
        let end = pm.to_position(offset + length);
        let line = start.line;
        let character = start.character;
        let utf16_len = end.character.saturating_sub(character);
        let (delta_line, delta_start) = if line == prev_line {
            (0, character - prev_start)
        } else {
            (line - prev_line, character)
        };
        data.push(lsp::SemanticToken {
            delta_line,
            delta_start,
            length: utf16_len,
            token_type: type_idx,
            token_modifiers_bitset: modifier_bits,
        });
        prev_line = line;
        prev_start = character;
    }
    data
}

/// `textDocument/semanticTokens/full`. Stamps a fresh `result_id` and
/// caches the encoded tokens against it so a subsequent delta request
/// can diff. Needs `&mut Workspace` only to update that cache.
pub fn handle(ws: &mut Workspace, uri: &lsp::Url) -> Option<lsp::SemanticTokensResult> {
    let entries = collect_entries(ws, uri)?;
    let doc = ws.doc(uri)?;
    let data = encode(doc.pos_map(), &entries);
    let result_id = ws.cache_semantic_tokens(uri, data.clone());
    Some(lsp::SemanticTokensResult::Tokens(lsp::SemanticTokens {
        result_id: Some(result_id),
        data,
    }))
}

/// `textDocument/semanticTokens/range`. Only the tokens whose source
/// span overlaps `range`. Read-only: range results are not cached for
/// delta (a delta always follows a `full`).
pub fn handle_range(
    ws: &Workspace,
    uri: &lsp::Url,
    range: lsp::Range,
) -> Option<lsp::SemanticTokensRangeResult> {
    let entries = collect_entries(ws, uri)?;
    let doc = ws.doc(uri)?;
    let from = position_to_offset(doc.pos_map(), range.start).unwrap_or(0);
    let to = position_to_offset(doc.pos_map(), range.end).unwrap_or(u32::MAX);
    // Keep a token if any part of it lies within [from, to).
    let in_range: Vec<Entry> = entries
        .into_iter()
        .filter(|&(start, len, _, _)| start < to && start + len > from)
        .collect();
    let data = encode(doc.pos_map(), &in_range);
    Some(lsp::SemanticTokensRangeResult::Tokens(lsp::SemanticTokens {
        result_id: None,
        data,
    }))
}

/// `textDocument/semanticTokens/full/delta`. Diffs the current tokens
/// against the cached baseline named by `previous_result_id`. When the
/// baseline is missing (server restart, evicted entry) we degrade to a
/// full token set, which the protocol explicitly allows.
pub fn handle_delta(
    ws: &mut Workspace,
    uri: &lsp::Url,
    previous_result_id: &str,
) -> Option<lsp::SemanticTokensFullDeltaResult> {
    let entries = collect_entries(ws, uri)?;
    let data = {
        let doc = ws.doc(uri)?;
        encode(doc.pos_map(), &entries)
    };

    let baseline = ws.semantic_tokens_baseline(uri, previous_result_id);
    let result_id = ws.cache_semantic_tokens(uri, data.clone());

    match baseline {
        Some(old) => {
            let edits = diff_tokens(&old, &data);
            Some(lsp::SemanticTokensFullDeltaResult::TokensDelta(
                lsp::SemanticTokensDelta {
                    result_id: Some(result_id),
                    edits,
                },
            ))
        }
        None => Some(lsp::SemanticTokensFullDeltaResult::Tokens(
            lsp::SemanticTokens {
                result_id: Some(result_id),
                data,
            },
        )),
    }
}

/// Produce a minimal single-edit diff between two token arrays.
///
/// LSP semantic-token edits address the *flat* `u32` array (5 ints per
/// token), so all offsets/counts below are in `u32` units. We trim the
/// common prefix and suffix and replace only the differing middle —
/// one [`SemanticTokensEdit`] covers it. Identical inputs yield no
/// edits.
fn diff_tokens(
    old: &[lsp::SemanticToken],
    new: &[lsp::SemanticToken],
) -> Vec<lsp::SemanticTokensEdit> {
    let mut prefix = 0usize;
    while prefix < old.len() && prefix < new.len() && old[prefix] == new[prefix] {
        prefix += 1;
    }
    let mut suffix = 0usize;
    while suffix < old.len() - prefix
        && suffix < new.len() - prefix
        && old[old.len() - 1 - suffix] == new[new.len() - 1 - suffix]
    {
        suffix += 1;
    }

    if prefix == old.len() && prefix == new.len() {
        return Vec::new(); // identical
    }

    let new_middle = &new[prefix..new.len() - suffix];
    let deleted = old.len() - prefix - suffix;
    let data = if new_middle.is_empty() {
        None
    } else {
        Some(new_middle.to_vec())
    };
    vec![lsp::SemanticTokensEdit {
        // 5 ints per token. Counts are tiny in practice; clamp on the
        // theoretical overflow rather than cast-and-truncate.
        start: u32::try_from(prefix * 5).unwrap_or(u32::MAX),
        delete_count: u32::try_from(deleted * 5).unwrap_or(u32::MAX),
        data,
    }]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workspace::Workspace;

    fn ws_with(src: &str) -> (Workspace, lsp::Url) {
        let mut ws = Workspace::default();
        let uri = lsp::Url::parse("file:///t.leek").unwrap();
        ws.open(uri.clone(), src.to_string());
        (ws, uri)
    }

    fn range(sl: u32, sc: u32, el: u32, ec: u32) -> lsp::Range {
        lsp::Range {
            start: lsp::Position {
                line: sl,
                character: sc,
            },
            end: lsp::Position {
                line: el,
                character: ec,
            },
        }
    }

    #[test]
    fn full_stamps_a_result_id() {
        let (mut ws, uri) = ws_with("var apple = 5\nvar n = apple\n");
        let lsp::SemanticTokensResult::Tokens(t) = super::handle(&mut ws, &uri).unwrap() else {
            panic!("expected tokens");
        };
        assert_eq!(t.data.len(), 3);
        assert!(t.result_id.is_some(), "full should carry a result id");
    }

    #[test]
    fn range_filters_to_requested_lines() {
        // Three decls on three lines; ask only for line 1.
        let (ws, uri) = ws_with("var a = 1\nvar b = 2\nvar c = 3\n");
        let lsp::SemanticTokensRangeResult::Tokens(t) =
            super::handle_range(&ws, &uri, range(1, 0, 2, 0)).unwrap()
        else {
            panic!("expected tokens");
        };
        // Only `b`'s declaration falls on line 1.
        assert_eq!(t.data.len(), 1, "data: {:?}", t.data);
        // Encoded relative to the start: delta_line 1 (from line 0 base).
        assert_eq!(t.data[0].delta_line, 1);
    }

    #[test]
    fn delta_against_baseline_is_minimal() {
        let (mut ws, uri) = ws_with("var apple = 5\nvar n = apple\n");
        // First full request establishes a baseline.
        let lsp::SemanticTokensResult::Tokens(first) = super::handle(&mut ws, &uri).unwrap()
        else {
            panic!("tokens");
        };
        let id = first.result_id.unwrap();

        // Edit: append a third statement that adds one new token.
        ws.update(&uri, "var apple = 5\nvar n = apple\nvar z = n\n".to_string());

        let delta = super::handle_delta(&mut ws, &uri, &id).unwrap();
        let lsp::SemanticTokensFullDeltaResult::TokensDelta(d) = delta else {
            panic!("expected a delta, got {delta:?}");
        };
        // The first three tokens are unchanged; the edit only appends.
        assert_eq!(d.edits.len(), 1, "edits: {:?}", d.edits);
        let edit = &d.edits[0];
        // Prefix of 3 unchanged tokens = 15 ints; nothing deleted.
        assert_eq!(edit.start, 15);
        assert_eq!(edit.delete_count, 0);
        assert!(edit.data.as_ref().is_some_and(|d| !d.is_empty()));
    }

    #[test]
    fn delta_with_unknown_baseline_falls_back_to_full() {
        let (mut ws, uri) = ws_with("var apple = 5\n");
        let delta = super::handle_delta(&mut ws, &uri, "does-not-exist").unwrap();
        // No cached baseline → a full token set is returned.
        assert!(
            matches!(delta, lsp::SemanticTokensFullDeltaResult::Tokens(_)),
            "expected full fallback, got {delta:?}"
        );
    }

    #[test]
    fn columns_and_lengths_are_utf16_not_bytes() {
        // `é` is 2 UTF-8 bytes but 1 UTF-16 unit. The `b` declaration
        // sits after it on the same line, so its byte column and UTF-16
        // column diverge by one — the token must use the UTF-16 column.
        let (mut ws, uri) = ws_with("var a = \"é\"; var b = 2\n");
        let lsp::SemanticTokensResult::Tokens(t) = super::handle(&mut ws, &uri).unwrap() else {
            panic!("tokens");
        };
        // Two declarations: `a` (col 4) then `b`.
        assert_eq!(t.data.len(), 2, "data: {:?}", t.data);
        assert_eq!(t.data[0].delta_start, 4, "`a` at column 4");
        // `b` is at byte column 18 but UTF-16 column 17; encoded
        // relative to `a` (col 4) that's 13, not the byte delta 14.
        assert_eq!(
            t.data[1].delta_start, 13,
            "`b` column must be UTF-16-relative, got {:?}",
            t.data[1]
        );
    }

    #[test]
    fn delta_for_identical_content_has_no_edits() {
        let (mut ws, uri) = ws_with("var apple = 5\nvar n = apple\n");
        let lsp::SemanticTokensResult::Tokens(first) = super::handle(&mut ws, &uri).unwrap()
        else {
            panic!("tokens");
        };
        let id = first.result_id.unwrap();
        // No edit between the two requests.
        let delta = super::handle_delta(&mut ws, &uri, &id).unwrap();
        let lsp::SemanticTokensFullDeltaResult::TokensDelta(d) = delta else {
            panic!("expected delta");
        };
        assert!(d.edits.is_empty(), "unchanged file → no edits: {:?}", d.edits);
    }
}
