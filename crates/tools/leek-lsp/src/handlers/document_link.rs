//! `textDocument/documentLink` — clickable `include("name")`
//! statements.
//!
//! Walks the CST for `IncludeStmt` nodes, finds the string-literal
//! argument inside the call, and produces a `DocumentLink` whose
//! target is `<workspace>/<name>.leek` if the include name resolves,
//! else just a tooltip with the raw name.
//!
//! Slice 1 limitation: we don't have access to the project root
//! from inside the workspace today (the LSP is single-document).
//! The link target is the include name with `.leek` appended,
//! resolved relative to the editing document's URI directory.
//! Real Folder-based resolution lives in `leek-hir::include_graph`;
//! plumbing that in is a slice-2 follow-up.

use leek_span::Span;
use leek_syntax::{SyntaxKind, SyntaxNode};
use tower_lsp::lsp_types as lsp;

use crate::util::position::span_to_range;
use crate::workspace::Workspace;

pub fn handle(ws: &Workspace, uri: &lsp::Url) -> Option<Vec<lsp::DocumentLink>> {
    let doc = ws.doc(uri)?;
    let run = crate::pipeline::run(ws, uri, leek_recipes::Target::Parsed)?;
    let green = &run.get::<leek_parser::pipeline::GreenTreeArtifact>()?.0;
    let root = SyntaxNode::new_root(green.clone());

    let mut out: Vec<lsp::DocumentLink> = Vec::new();
    for node in root.descendants() {
        if node.kind() != SyntaxKind::IncludeStmt {
            continue;
        }
        // The IncludeStmt's body is `include("name")` — find the
        // string literal token nested under the call's arg list.
        let Some((name, span)) = string_arg(&node) else {
            continue;
        };
        let range = span_to_range(doc.pos_map(), span);
        let target = resolve_target(uri, &name);
        out.push(lsp::DocumentLink {
            range,
            target,
            tooltip: Some(format!("include \"{name}\"")),
            data: None,
        });
    }
    Some(out)
}

/// Find the first string literal under `include_stmt` and return
/// both its content (without quotes) and its text-range span.
fn string_arg(include_stmt: &SyntaxNode) -> Option<(String, Span)> {
    let tok = include_stmt
        .descendants_with_tokens()
        .filter_map(leek_syntax::language::NodeOrToken::into_token)
        .find(|t| t.kind() == SyntaxKind::StringLiteral)?;
    let raw = tok.text();
    let unquoted = raw.trim_start_matches(['\'', '"']);
    let unquoted = unquoted.trim_end_matches(['\'', '"']);
    let r = tok.text_range();
    let span = Span::new(
        leek_span::SourceId::new(1).unwrap(),
        u32::from(r.start()),
        u32::from(r.end()),
    );
    Some((unquoted.to_string(), span))
}

/// Resolve the include name to a URL by stripping the source's
/// filename and appending `<name>.leek`. Conservative: returns
/// `None` if the source URI doesn't have a parent path.
fn resolve_target(src: &lsp::Url, name: &str) -> Option<lsp::Url> {
    let mut url = src.clone();
    let segments: Vec<String> = url.path_segments()?.map(str::to_string).collect();
    if segments.is_empty() {
        return None;
    }
    let parent: Vec<&str> = segments
        .iter()
        .take(segments.len() - 1)
        .map(String::as_str)
        .collect();
    let leaf = if std::path::Path::new(name)
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("leek"))
    {
        name.to_string()
    } else {
        format!("{name}.leek")
    };
    let mut path = String::from("/");
    if !parent.is_empty() {
        path.push_str(&parent.join("/"));
        path.push('/');
    }
    path.push_str(&leaf);
    url.set_path(&path);
    Some(url)
}
