//! `workspace/willRenameFiles` — keep `include(...)` references valid
//! when a `.leek` file is renamed in the editor.
//!
//! When the user renames `helpers.leek` → `util.leek`, every
//! `include("helpers")` elsewhere in the workspace should become
//! `include("util")`. We compute those edits *before* the rename
//! happens (the LSP `willRename` hook returns a [`WorkspaceEdit`] the
//! client applies atomically with the move) by scanning each known
//! document's CST for `IncludeStmt` string arguments whose target name
//! matches a renamed file's stem.
//!
//! Includes are matched on their final path component's *stem* (the
//! name without a `.leek` extension), which is how Leekscript resolves
//! them. A directory prefix and an explicit `.leek` extension on the
//! include literal are both preserved in the rewrite.

use std::collections::HashMap;
use std::path::Path;

use leek_span::Span;
use leek_syntax::{SyntaxKind, SyntaxNode};
use tower_lsp::lsp_types as lsp;

use crate::util::position::span_to_range;
use crate::workspace::{Workspace, uri_to_path};

/// Compute the include-rewrite edits for a batch of renames. Returns
/// `None` when nothing references any renamed file (the client treats
/// `None`/empty as "no changes"). Only `.leek` file renames are
/// considered.
pub fn will_rename(ws: &Workspace, renames: &[(String, String)]) -> Option<lsp::WorkspaceEdit> {
    // old-stem → new include literal core (without extension).
    let mut stem_map: HashMap<String, String> = HashMap::new();
    for (old_uri, new_uri) in renames {
        let (Some(old), Some(new)) = (parse_leek_uri(old_uri), parse_leek_uri(new_uri)) else {
            continue;
        };
        if let (Some(old_stem), Some(new_stem)) = (file_stem(&old), file_stem(&new)) {
            stem_map.insert(old_stem, new_stem);
        }
    }
    if stem_map.is_empty() {
        return None;
    }

    let mut changes: HashMap<lsp::Url, Vec<lsp::TextEdit>> = HashMap::new();
    for target in ws.analysis_targets() {
        let edits = edits_for_document(ws, target.source_file, &stem_map);
        if !edits.is_empty() {
            changes.insert(target.uri.clone(), edits);
        }
    }

    if changes.is_empty() {
        return None;
    }
    Some(lsp::WorkspaceEdit {
        changes: Some(changes),
        document_changes: None,
        change_annotations: None,
    })
}

/// Edits for one document: rewrite each `include("…")` whose target
/// stem appears in `stem_map`.
fn edits_for_document(
    ws: &Workspace,
    source_file: leek_pipeline::salsa::SourceFile,
    stem_map: &HashMap<String, String>,
) -> Vec<lsp::TextEdit> {
    let Some(run) = crate::pipeline::run_on_file(ws, source_file, leek_recipes::Target::Parsed)
    else {
        return Vec::new();
    };
    let Some(green) = run.get::<leek_parser::pipeline::GreenTreeArtifact>() else {
        return Vec::new();
    };
    let root = SyntaxNode::new_root(green.0.clone());
    let text = source_file.text(&ws.db);
    let line_table = leek_span::LineTable::new(text);
    let pm = crate::util::position::PosMap::new(&line_table, text);

    let mut edits: Vec<lsp::TextEdit> = Vec::new();
    for node in root.descendants() {
        if node.kind() != SyntaxKind::IncludeStmt {
            continue;
        }
        let Some((content, inner_span)) = include_string_arg(&node) else {
            continue;
        };
        let Some(new_content) = rewritten_include(&content, stem_map) else {
            continue;
        };
        edits.push(lsp::TextEdit {
            range: span_to_range(pm, inner_span),
            new_text: new_content,
        });
    }
    edits
}

/// The string-literal argument of an `include(...)` statement: its
/// content (without quotes) and the span of that *inner* content (so a
/// rewrite leaves the surrounding quotes untouched).
fn include_string_arg(include_stmt: &SyntaxNode) -> Option<(String, Span)> {
    let tok = include_stmt
        .descendants_with_tokens()
        .filter_map(leek_syntax::language::NodeOrToken::into_token)
        .find(|t| t.kind() == SyntaxKind::StringLiteral)?;
    let raw = tok.text();
    // Need at least an open + close quote to have inner content.
    if raw.len() < 2 {
        return None;
    }
    let content = raw[1..raw.len() - 1].to_string();
    let r = tok.text_range();
    let inner = Span::new(
        leek_span::SourceId::new(1).unwrap(),
        u32::from(r.start()) + 1,
        u32::from(r.end()) - 1,
    );
    Some((content, inner))
}

/// If `content` (an include literal like `helpers`, `sub/helpers`, or
/// `helpers.leek`) targets a renamed file, return its rewritten form;
/// else `None`. Preserves any directory prefix and `.leek` extension.
fn rewritten_include(content: &str, stem_map: &HashMap<String, String>) -> Option<String> {
    let (dir, last) = match content.rsplit_once('/') {
        Some((dir, last)) => (Some(dir), last),
        None => (None, content),
    };
    let (stem, had_ext) = match last.strip_suffix(".leek") {
        Some(s) => (s, true),
        None => (last, false),
    };
    let new_stem = stem_map.get(stem)?;
    let mut out = String::new();
    if let Some(dir) = dir {
        out.push_str(dir);
        out.push('/');
    }
    out.push_str(new_stem);
    if had_ext {
        out.push_str(".leek");
    }
    Some(out)
}

/// Parse a URI string into a path, keeping only `.leek` files.
fn parse_leek_uri(uri: &str) -> Option<std::path::PathBuf> {
    let url = lsp::Url::parse(uri).ok()?;
    let path = uri_to_path(&url)?;
    path.extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("leek"))
        .then_some(path)
}

fn file_stem(path: &Path) -> Option<String> {
    path.file_stem().and_then(|s| s.to_str()).map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workspace::Workspace;

    fn ws_with(files: &[(&str, &str)]) -> Workspace {
        let mut ws = Workspace::default();
        for (name, src) in files {
            let uri = lsp::Url::parse(&format!("file:///proj/{name}")).unwrap();
            ws.open(uri, src.to_string());
        }
        ws
    }

    fn rename(old: &str, new: &str) -> (String, String) {
        (
            format!("file:///proj/{old}"),
            format!("file:///proj/{new}"),
        )
    }

    #[test]
    fn rewrites_bare_include_reference() {
        let ws = ws_with(&[
            ("main.leek", "include(\"helpers\")\nreturn 0\n"),
            ("helpers.leek", "function help() { return 1 }\n"),
        ]);
        let edit = will_rename(&ws, &[rename("helpers.leek", "util.leek")]).expect("edit");
        let changes = edit.changes.unwrap();
        let main_uri = lsp::Url::parse("file:///proj/main.leek").unwrap();
        let edits = changes.get(&main_uri).expect("edit for main");
        assert_eq!(edits.len(), 1);
        assert_eq!(edits[0].new_text, "util");
    }

    #[test]
    fn preserves_extension_and_directory_prefix() {
        let ws = ws_with(&[("main.leek", "include(\"lib/helpers.leek\")\nreturn 0\n")]);
        let edit = will_rename(&ws, &[rename("lib/helpers.leek", "lib/util.leek")]).expect("edit");
        let edits = edit
            .changes
            .unwrap()
            .remove(&lsp::Url::parse("file:///proj/main.leek").unwrap())
            .unwrap();
        assert_eq!(edits[0].new_text, "lib/util.leek");
    }

    #[test]
    fn unrelated_rename_produces_no_edit() {
        let ws = ws_with(&[("main.leek", "include(\"helpers\")\nreturn 0\n")]);
        assert!(will_rename(&ws, &[rename("other.leek", "renamed.leek")]).is_none());
    }

    #[test]
    fn non_leek_rename_is_ignored() {
        let ws = ws_with(&[("main.leek", "include(\"helpers\")\nreturn 0\n")]);
        let r = (
            "file:///proj/helpers.txt".to_string(),
            "file:///proj/util.txt".to_string(),
        );
        assert!(will_rename(&ws, &[r]).is_none());
    }
}
