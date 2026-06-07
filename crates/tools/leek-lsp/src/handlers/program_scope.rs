//! Program-scope computation for semantic cross-file references and
//! rename.
//!
//! Leekscript has a single flat namespace, but it is flat *per program*
//! — an entry file plus everything it transitively `include`s. Two
//! unrelated leek-wars AIs in the same workspace can each define a
//! top-level `tick()`; those are distinct symbols and must never be
//! renamed together. Conversely a function in a shared `util.leek` is
//! the *same* symbol for every AI that includes it, so renaming it must
//! reach all of them.
//!
//! The correct scope for a symbol declared in file `D` is therefore the
//! union of every include-closure that contains `D`:
//!
//! ```text
//! scope(D) = ⋃ { closure(E) : E ∈ workspace, D ∈ closure(E) }
//! ```
//!
//! where `closure(E)` is `E` plus every file it transitively includes.
//! A file `X` lands in the scope exactly when some program (`E` and its
//! includes) contains both `X` and `D` — i.e. `X` and `D` can see each
//! other's flat-namespace symbols. This separates independent AIs even
//! when they share a library (the library is in scope; the *other* AI's
//! private files are not), which an undirected connected-component
//! approach would wrongly merge.

use std::collections::{HashMap, HashSet};
use std::path::{Component, Path, PathBuf};

use leek_pipeline::salsa::SourceFile;
use leek_syntax::{SyntaxKind, SyntaxNode, language::NodeOrToken};
use tower_lsp::lsp_types::Url;

use crate::workspace::{Workspace, uri_to_path};

/// One workspace file resolvable for analysis.
#[derive(Clone)]
pub(crate) struct ScopeFile {
    pub uri: Url,
    pub source_file: SourceFile,
}

/// The files that share a program with `home_uri` — see the module
/// docs. Always contains the home file itself. Falls back to just the
/// home file when it has no filesystem path (an untitled buffer, where
/// include resolution is meaningless).
pub(crate) fn program_scope(ws: &Workspace, home_uri: &Url) -> Vec<ScopeFile> {
    // Index every workspace file by its normalized path.
    let mut by_path: HashMap<PathBuf, ScopeFile> = HashMap::new();
    let mut home_file: Option<ScopeFile> = None;
    for t in ws.analysis_targets() {
        let sf = ScopeFile {
            uri: t.uri.clone(),
            source_file: t.source_file,
        };
        if t.uri == home_uri {
            home_file = Some(sf.clone());
        }
        if let Some(p) = uri_to_path(t.uri) {
            by_path.insert(normalize(&p), sf);
        }
    }

    let Some(home_path) = uri_to_path(home_uri).map(|p| normalize(&p)) else {
        return home_file.into_iter().collect();
    };
    if !by_path.contains_key(&home_path) {
        return home_file.into_iter().collect();
    }

    // Forward include edges: path → the paths it directly includes.
    let mut edges: HashMap<PathBuf, Vec<PathBuf>> = HashMap::new();
    for (path, sf) in &by_path {
        edges.insert(path.clone(), include_targets(ws, sf.source_file, path, &by_path));
    }

    // Union every forward closure that contains the home file.
    let mut scope: HashSet<PathBuf> = HashSet::new();
    for start in by_path.keys() {
        let closure = forward_closure(&edges, start);
        if closure.contains(&home_path) {
            scope.extend(closure);
        }
    }
    scope.insert(home_path);

    scope
        .into_iter()
        .filter_map(|p| by_path.get(&p).cloned())
        .collect()
}

/// `start` plus every file reachable from it through include edges.
fn forward_closure(edges: &HashMap<PathBuf, Vec<PathBuf>>, start: &Path) -> HashSet<PathBuf> {
    let mut seen: HashSet<PathBuf> = HashSet::new();
    let mut stack = vec![start.to_path_buf()];
    while let Some(p) = stack.pop() {
        if !seen.insert(p.clone()) {
            continue;
        }
        if let Some(next) = edges.get(&p) {
            for n in next {
                if !seen.contains(n) {
                    stack.push(n.clone());
                }
            }
        }
    }
    seen
}

/// Resolve a file's `include("name")` statements to normalized
/// workspace paths, mirroring the [`Folder`](leek_resolver::folder)
/// resolution order (sibling `<dir>/<name>.leek`, then `<dir>/<name>`).
/// Names that don't resolve to a known workspace file are dropped.
fn include_targets(
    ws: &Workspace,
    source_file: SourceFile,
    includer: &Path,
    by_path: &HashMap<PathBuf, ScopeFile>,
) -> Vec<PathBuf> {
    let Some(run) = crate::pipeline::run_on_file(ws, source_file, leek_recipes::Target::Parsed)
    else {
        return Vec::new();
    };
    let Some(green) = run.get::<leek_parser::pipeline::GreenTreeArtifact>() else {
        return Vec::new();
    };
    let root = SyntaxNode::new_root(green.0.clone());
    let dir = includer
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_default();

    let mut out: Vec<PathBuf> = Vec::new();
    for node in root.descendants() {
        if node.kind() != SyntaxKind::IncludeStmt {
            continue;
        }
        let Some(name) = include_name(&node) else {
            continue;
        };
        let with_ext = normalize(&dir.join(format!("{name}.leek")));
        let bare = normalize(&dir.join(&name));
        let resolved = if by_path.contains_key(&with_ext) {
            Some(with_ext)
        } else if by_path.contains_key(&bare) {
            Some(bare)
        } else {
            None
        };
        if let Some(p) = resolved
            && !out.contains(&p)
        {
            out.push(p);
        }
    }
    out
}

/// The unquoted name of the first string literal under an
/// `include(...)` statement.
fn include_name(include_stmt: &SyntaxNode) -> Option<String> {
    let tok = include_stmt
        .descendants_with_tokens()
        .filter_map(NodeOrToken::into_token)
        .find(|t| t.kind() == SyntaxKind::StringLiteral)?;
    let raw = tok.text();
    if raw.len() < 2 {
        return None;
    }
    Some(raw[1..raw.len() - 1].to_string())
}

/// Lexically normalize a path (resolve `.` and `..`, no I/O). Both the
/// workspace index keys and the include candidates go through this, so
/// sibling spellings collapse to the same key without touching disk.
fn normalize(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for comp in p.components() {
        match comp {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ws_with(files: &[(&str, &str)]) -> Workspace {
        let mut ws = Workspace::default();
        for (name, src) in files {
            let uri = Url::parse(&format!("file:///proj/{name}")).unwrap();
            ws.open(uri, src.to_string());
        }
        ws
    }

    fn uri(name: &str) -> Url {
        Url::parse(&format!("file:///proj/{name}")).unwrap()
    }

    fn scope_names(ws: &Workspace, home: &str) -> Vec<String> {
        let mut names: Vec<String> = program_scope(ws, &uri(home))
            .into_iter()
            .map(|f| {
                f.uri
                    .path_segments()
                    .and_then(|s| s.last())
                    .unwrap_or("")
                    .to_string()
            })
            .collect();
        names.sort();
        names
    }

    #[test]
    fn lone_file_scopes_to_itself() {
        let ws = ws_with(&[("a.leek", "function f() { return 1 }\n")]);
        assert_eq!(scope_names(&ws, "a.leek"), ["a.leek"]);
    }

    #[test]
    fn includer_and_included_share_scope() {
        let ws = ws_with(&[
            ("util.leek", "function helper() { return 1 }\n"),
            ("main.leek", "include(\"util\")\nvar n = helper()\n"),
        ]);
        // From either end, the program is {main, util}.
        assert_eq!(scope_names(&ws, "util.leek"), ["main.leek", "util.leek"]);
        assert_eq!(scope_names(&ws, "main.leek"), ["main.leek", "util.leek"]);
    }

    #[test]
    fn independent_programs_stay_separate() {
        // Two AIs that never include each other.
        let ws = ws_with(&[
            ("ai1.leek", "function tick() { return 1 }\n"),
            ("ai2.leek", "function tick() { return 2 }\n"),
        ]);
        assert_eq!(scope_names(&ws, "ai1.leek"), ["ai1.leek"]);
        assert_eq!(scope_names(&ws, "ai2.leek"), ["ai2.leek"]);
    }

    #[test]
    fn shared_library_reaches_all_includers_but_not_across_ais() {
        // ai1 and ai2 both include util, but not each other.
        let ws = ws_with(&[
            ("util.leek", "function shared() { return 0 }\n"),
            ("ai1.leek", "include(\"util\")\nfunction priv1() { return shared() }\n"),
            ("ai2.leek", "include(\"util\")\nfunction priv2() { return shared() }\n"),
        ]);
        // The shared library is in every AI's program.
        assert_eq!(
            scope_names(&ws, "util.leek"),
            ["ai1.leek", "ai2.leek", "util.leek"]
        );
        // But a symbol private to ai1 only sees ai1's program (ai1 +
        // util), never ai2 — even though they share util.
        assert_eq!(scope_names(&ws, "ai1.leek"), ["ai1.leek", "util.leek"]);
        assert_eq!(scope_names(&ws, "ai2.leek"), ["ai2.leek", "util.leek"]);
    }
}
