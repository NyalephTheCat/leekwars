//! `workspace/willRenameFiles` — keep `include(...)` references valid
//! when a `.leek` file is renamed or moved in the editor.
//!
//! When the user renames `helpers.leek` → `util.leek`, every
//! `include("helpers")` that actually *resolved to that file* should
//! become `include("util")`. We compute those edits *before* the rename
//! happens (the LSP `willRename` hook returns a [`WorkspaceEdit`] the
//! client applies atomically with the move) by scanning each known
//! document's CST for `IncludeStmt` string arguments.
//!
//! Matching is done on the resolved *path*, never on the bare stem: an
//! include literal is resolved against the includer's directory the way
//! [`Folder`](leek_resolver::folder::Folder) would (sibling
//! `<dir>/<name>.leek` first, then `<dir>/<name>`) and the result is
//! compared to the renamed file's old path. Two unrelated AIs can each
//! own a `util.leek`, and renaming one must not touch the other's
//! `include("util")`.
//!
//! The replacement literal is *recomputed* as a path relative to the
//! includer's directory, so a move across directories produces a real
//! edit (`include("lib/util")` → `include("vendor/util")`, or
//! `include("util")` → `include("../ai1/util")` when the includer
//! itself is the file being moved). An explicit `.leek` extension on
//! the original literal is preserved; a bare literal stays bare.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use leek_span::Span;
use leek_span::paths::normalize_lexical;
use leek_syntax::{SyntaxKind, SyntaxNode};
use tower_lsp::lsp_types as lsp;

use crate::workspace::{Workspace, uri_to_path};

/// Compute the include-rewrite edits for a batch of renames. Returns
/// `None` when nothing references any renamed file (the client treats
/// `None`/empty as "no changes"). Only `.leek` file renames are
/// considered.
pub fn will_rename(ws: &Workspace, renames: &[(String, String)]) -> Option<lsp::WorkspaceEdit> {
    // Normalized old path → normalized new path.
    let mut renamed: HashMap<PathBuf, PathBuf> = HashMap::new();
    for (old_uri, new_uri) in renames {
        let (Some(old), Some(new)) = (parse_leek_uri(old_uri), parse_leek_uri(new_uri)) else {
            continue;
        };
        let (old, new) = (normalize_lexical(&old), normalize_lexical(&new));
        if old != new {
            renamed.insert(old, new);
        }
    }
    if renamed.is_empty() {
        return None;
    }

    // Every path an include literal may resolve to: the workspace files
    // plus the renamed originals (which need not be open or indexed).
    let targets = ws.analysis_targets();
    let mut known: HashSet<PathBuf> = renamed.keys().cloned().collect();
    for target in &targets {
        if let Some(path) = uri_to_path(target.uri) {
            known.insert(normalize_lexical(&path));
        }
    }

    let mut changes: HashMap<lsp::Url, Vec<lsp::TextEdit>> = HashMap::new();
    for target in &targets {
        let Some(path) = uri_to_path(target.uri).map(|p| normalize_lexical(&p)) else {
            continue;
        };
        let edits = edits_for_document(ws, target.source_file, &path, &known, &renamed);
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

/// Edits for one document: rewrite each `include("…")` whose resolved
/// target is renamed, plus — when the document itself is the file being
/// moved — every literal whose relative spelling changes with it.
fn edits_for_document(
    ws: &Workspace,
    source_file: leek_pipeline::salsa::SourceFile,
    includer: &Path,
    known: &HashSet<PathBuf>,
    renamed: &HashMap<PathBuf, PathBuf>,
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

    let old_dir = parent_dir(includer);
    let moved_includer = renamed.get(includer);
    let new_dir = match moved_includer {
        Some(moved) => parent_dir(moved),
        None => old_dir.clone(),
    };

    let mut edits: Vec<lsp::TextEdit> = Vec::new();
    for node in root.descendants() {
        if node.kind() != SyntaxKind::IncludeStmt {
            continue;
        }
        let Some((content, inner_span)) = include_string_arg(&node) else {
            continue;
        };
        let Some(old_target) = resolve_include(&old_dir, &content, known) else {
            continue;
        };
        let new_target = renamed.get(&old_target);
        // A literal only needs rewriting when its target moved, or when
        // the includer moved out from under an unchanged target.
        if new_target.is_none() && moved_includer.is_none() {
            continue;
        }
        let new_target = new_target.unwrap_or(&old_target);
        let keep_ext = has_leek_extension(&content);
        let Some(new_content) = include_literal(&new_dir, new_target, keep_ext) else {
            continue;
        };
        if new_content == content {
            continue;
        }
        edits.push(lsp::TextEdit {
            range: pm.span_range(inner_span),
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

/// Resolve an include literal against the includer's directory, in the
/// [`Folder`](leek_resolver::folder::Folder) order: `<dir>/<name>.leek`
/// first, then `<dir>/<name>`. Only paths in `known` count as resolved.
fn resolve_include(dir: &Path, name: &str, known: &HashSet<PathBuf>) -> Option<PathBuf> {
    if name.is_empty() {
        return None;
    }
    let with_ext = normalize_lexical(&dir.join(format!("{name}.leek")));
    if known.contains(&with_ext) {
        return Some(with_ext);
    }
    let bare = normalize_lexical(&dir.join(name));
    known.contains(&bare).then_some(bare)
}

/// The include literal that names `target` from a file living in
/// `dir` — a `/`-separated relative path, with the `.leek` extension
/// kept only when the original literal spelled it out. `None` when the
/// two paths share no common root and no relative spelling exists.
fn include_literal(dir: &Path, target: &Path, keep_ext: bool) -> Option<String> {
    let stripped;
    let target = if keep_ext {
        target
    } else {
        stripped = strip_leek_extension(target);
        stripped.as_path()
    };

    let from: Vec<_> = dir.components().collect();
    let to: Vec<_> = target.components().collect();
    let shared = from.iter().zip(&to).take_while(|(a, b)| a == b).count();
    // Nothing in common while `dir` is non-empty means different roots
    // (distinct Windows prefixes); `..` cannot bridge those.
    if shared == 0 && !from.is_empty() {
        return None;
    }

    let mut parts: Vec<String> = vec!["..".to_string(); from.len() - shared];
    for component in &to[shared..] {
        parts.push(component.as_os_str().to_string_lossy().into_owned());
    }
    (!parts.is_empty()).then(|| parts.join("/"))
}

fn parent_dir(path: &Path) -> PathBuf {
    path.parent().map(Path::to_path_buf).unwrap_or_default()
}

fn has_leek_extension(name: &str) -> bool {
    is_leek_path(Path::new(name))
}

fn strip_leek_extension(path: &Path) -> PathBuf {
    if is_leek_path(path) {
        path.with_extension("")
    } else {
        path.to_path_buf()
    }
}

fn is_leek_path(path: &Path) -> bool {
    path.extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("leek"))
}

/// Parse a URI string into a path, keeping only `.leek` files.
fn parse_leek_uri(uri: &str) -> Option<std::path::PathBuf> {
    let url = lsp::Url::parse(uri).ok()?;
    let path = uri_to_path(&url)?;
    is_leek_path(&path).then_some(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workspace::Workspace;

    fn test_url(name: &str) -> lsp::Url {
        let path = std::env::temp_dir()
            .join("leek-lsp-file-operation-tests")
            .join(name);
        lsp::Url::from_file_path(path).expect("test path should be a valid file URI")
    }

    fn ws_with(files: &[(&str, &str)]) -> Workspace {
        let mut ws = Workspace::default();
        for (name, src) in files {
            let uri = test_url(name);
            ws.open(uri, src.to_string());
        }
        ws
    }

    fn rename(old: &str, new: &str) -> (String, String) {
        (test_url(old).to_string(), test_url(new).to_string())
    }

    /// The rewritten literals for one document, or `None` when the
    /// document gets no edit at all.
    fn edits_for(edit: &lsp::WorkspaceEdit, name: &str) -> Option<Vec<String>> {
        edit.changes
            .as_ref()?
            .get(&test_url(name))
            .map(|edits| edits.iter().map(|e| e.new_text.clone()).collect())
    }

    #[test]
    fn rewrites_bare_include_reference() {
        let ws = ws_with(&[
            ("main.leek", "include(\"helpers\")\nreturn 0\n"),
            ("helpers.leek", "function help() { return 1 }\n"),
        ]);
        let edit = will_rename(&ws, &[rename("helpers.leek", "util.leek")]).expect("edit");
        assert_eq!(
            edits_for(&edit, "main.leek").as_deref(),
            Some(&["util".to_string()][..])
        );
    }

    #[test]
    fn preserves_extension_and_directory_prefix() {
        let ws = ws_with(&[("main.leek", "include(\"lib/helpers.leek\")\nreturn 0\n")]);
        let edit = will_rename(&ws, &[rename("lib/helpers.leek", "lib/util.leek")]).expect("edit");
        assert_eq!(
            edits_for(&edit, "main.leek").as_deref(),
            Some(&["lib/util.leek".to_string()][..])
        );
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
            test_url("helpers.txt").to_string(),
            test_url("util.txt").to_string(),
        );
        assert!(will_rename(&ws, &[r]).is_none());
    }

    #[test]
    fn same_stem_in_another_directory_is_left_alone() {
        // Two independent AIs, each with its own `util.leek`. Renaming
        // ai1's must not touch ai2's include.
        let ws = ws_with(&[
            ("ai1/main.leek", "include(\"util\")\nreturn 0\n"),
            ("ai1/util.leek", "function a() { return 1 }\n"),
            ("ai2/main.leek", "include(\"util\")\nreturn 0\n"),
            ("ai2/util.leek", "function b() { return 2 }\n"),
        ]);
        let edit = will_rename(&ws, &[rename("ai1/util.leek", "ai1/helpers.leek")]).expect("edit");
        assert_eq!(
            edits_for(&edit, "ai1/main.leek").as_deref(),
            Some(&["helpers".to_string()][..])
        );
        assert_eq!(edits_for(&edit, "ai2/main.leek"), None);
    }

    #[test]
    fn same_stem_under_a_directory_prefix_is_left_alone() {
        // `include("lib/util")` resolves to lib/util.leek, not to the
        // sibling util.leek being renamed.
        let ws = ws_with(&[
            ("main.leek", "include(\"lib/util\")\nreturn 0\n"),
            ("lib/util.leek", "function a() { return 1 }\n"),
            ("util.leek", "function b() { return 2 }\n"),
        ]);
        assert!(will_rename(&ws, &[rename("util.leek", "renamed.leek")]).is_none());
    }

    #[test]
    fn cross_directory_move_recomputes_the_relative_path() {
        // Same stem, new directory: the old code produced no edit.
        let ws = ws_with(&[
            ("main.leek", "include(\"lib/util\")\nreturn 0\n"),
            ("lib/util.leek", "function a() { return 1 }\n"),
        ]);
        let edit = will_rename(&ws, &[rename("lib/util.leek", "vendor/util.leek")]).expect("edit");
        assert_eq!(
            edits_for(&edit, "main.leek").as_deref(),
            Some(&["vendor/util".to_string()][..])
        );
    }

    #[test]
    fn move_out_of_a_subdirectory_walks_back_up() {
        let ws = ws_with(&[
            ("ai1/main.leek", "include(\"util\")\nreturn 0\n"),
            ("ai1/util.leek", "function a() { return 1 }\n"),
        ]);
        let edit = will_rename(&ws, &[rename("ai1/util.leek", "util.leek")]).expect("edit");
        assert_eq!(
            edits_for(&edit, "ai1/main.leek").as_deref(),
            Some(&["../util".to_string()][..])
        );
    }

    #[test]
    fn moving_the_includer_rewrites_its_own_includes() {
        let ws = ws_with(&[
            ("ai1/main.leek", "include(\"util\")\nreturn 0\n"),
            ("ai1/util.leek", "function a() { return 1 }\n"),
        ]);
        let edit = will_rename(&ws, &[rename("ai1/main.leek", "ai2/main.leek")]).expect("edit");
        assert_eq!(
            edits_for(&edit, "ai1/main.leek").as_deref(),
            Some(&["../ai1/util".to_string()][..])
        );
    }

    #[test]
    fn moving_a_whole_program_keeps_literals_stable() {
        let ws = ws_with(&[
            ("ai1/main.leek", "include(\"util\")\nreturn 0\n"),
            ("ai1/util.leek", "function a() { return 1 }\n"),
        ]);
        let renames = [
            rename("ai1/main.leek", "ai2/main.leek"),
            rename("ai1/util.leek", "ai2/util.leek"),
        ];
        assert!(will_rename(&ws, &renames).is_none());
    }

    #[test]
    fn unresolvable_include_is_never_rewritten() {
        // `missing` resolves to nothing in the workspace, so a rename of
        // an unrelated same-stem-free file leaves it untouched.
        let ws = ws_with(&[
            ("main.leek", "include(\"missing\")\nreturn 0\n"),
            ("other.leek", "function a() { return 1 }\n"),
        ]);
        assert!(will_rename(&ws, &[rename("other.leek", "renamed.leek")]).is_none());
    }
}
