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

use leek_db::queries::{IncludeRef, include_edges, resolve_include};
use leek_db::{Db, WorkspaceFiles};
use leek_pipeline::salsa::SourceFile;
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
    // Index every workspace file by the canonical path its salsa input
    // carries — the same key `WorkspaceFiles` is built with, so a file
    // `resolve_include` hands back is always a file this map holds.
    let mut by_path: HashMap<String, ScopeFile> = HashMap::new();
    let mut home_file: Option<ScopeFile> = None;
    for t in ws.analysis_targets() {
        let sf = ScopeFile {
            uri: t.uri.clone(),
            source_file: t.source_file,
        };
        if &t.uri == home_uri {
            home_file = Some(sf.clone());
        }
        if !t.canonical_path.is_empty() {
            by_path.insert(t.canonical_path.clone(), sf);
        }
    }

    let Some(home_path) = uri_to_path(home_uri).map(|p| p.display().to_string()) else {
        return home_file.into_iter().collect();
    };
    if !by_path.contains_key(&home_path) {
        return home_file.into_iter().collect();
    }

    // Forward include edges: path → the paths it directly includes.
    let db = &ws.db;
    let files = ws.files();
    let mut edges: HashMap<String, Vec<String>> = HashMap::new();
    for (path, sf) in &by_path {
        edges.insert(
            path.clone(),
            include_targets(db, files, sf.source_file, path),
        );
    }

    // Union every forward closure that contains the home file.
    let mut scope: HashSet<String> = HashSet::new();
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
fn forward_closure(edges: &HashMap<String, Vec<String>>, start: &str) -> HashSet<String> {
    let mut seen: HashSet<String> = HashSet::new();
    let mut stack = vec![start.to_string()];
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

/// The canonical paths of the workspace files a file directly
/// `include`s, in source order and without repeats.
///
/// Both halves are tracked queries: [`include_edges`] reads the file's
/// memoized token stream, [`resolve_include`] the workspace's file map.
/// This used to be a second include-graph implementation — a full
/// `Target::Parsed` pipeline run per workspace file, per references /
/// rename / completion request, with its own copy of the folder's
/// candidate order. Both are gone: there is one scan and one candidate
/// order in the workspace now, and repeat requests hit the memo.
fn include_targets(
    db: &dyn Db,
    files: WorkspaceFiles,
    source_file: SourceFile,
    includer: &str,
) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for call in include_edges(db, source_file).includes {
        let site = IncludeRef::new(db, includer.to_string(), call.name);
        let Some(target) = resolve_include(db, files, site) else {
            continue;
        };
        let path = target.canonical_path(db).clone();
        if !out.contains(&path) {
            out.push(path);
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
            let path = std::env::temp_dir()
                .join("leek-lsp-program-scope-tests")
                .join(name);
            let uri = Url::from_file_path(path).expect("test path should be a valid file URI");
            ws.open(uri, src.to_string());
        }
        ws
    }

    fn uri(name: &str) -> Url {
        let path = std::env::temp_dir()
            .join("leek-lsp-program-scope-tests")
            .join(name);
        Url::from_file_path(path).expect("test path should be a valid file URI")
    }

    fn scope_names(ws: &Workspace, home: &str) -> Vec<String> {
        let mut names: Vec<String> = program_scope(ws, &uri(home))
            .into_iter()
            .map(|f| {
                f.uri
                    .path_segments()
                    .and_then(|mut s| s.next_back())
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
            (
                "ai1.leek",
                "include(\"util\")\nfunction priv1() { return shared() }\n",
            ),
            (
                "ai2.leek",
                "include(\"util\")\nfunction priv2() { return shared() }\n",
            ),
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
