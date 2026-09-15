//! The include closure: every file an entry file transitively pulls
//! in, parsed once, as a value.
//!
//! [`resolve_include_closure`] is the whole of what the
//! [`ResolveIncludes`](crate::pipeline::ResolveIncludes) step used to
//! do inline — walk the graph, settle each file's version, collect the
//! program-wide class set, parse every included file and report the
//! failures at their `include("…")` sites — with nothing but its
//! arguments for input. The step is now an adapter that hands it the
//! run's text, version and flags and publishes what comes back.
//!
//! ### Green trees, not red ones
//!
//! [`ClosureFile`] carries a [`GreenNode`], never a
//! [`SyntaxNode`](leek_syntax::SyntaxNode). Green trees are the
//! immutable, thread-safe, structurally-shared half of rowan; a red
//! tree is a per-thread cursor over one, with interior mutability and
//! no `Send`. A closure is meant to become a memoized query result, and
//! a query result has to survive being handed to another thread, so the
//! red trees are cast at the boundary — see
//! [`IncludeGraphArtifact`](crate::pipeline::IncludeGraphArtifact),
//! which does exactly that for today's AST-walking consumers.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use leek_diagnostics::{Diagnostic, Severity, codes, diag};
use leek_parser::{ParseOptions, parse_file_with};
use leek_span::paths::canonical_or_normalized;
use leek_span::{FeatureFlags, SourceId, Span};
use leek_syntax::Version;
use leek_syntax::language::GreenNode;

use crate::folder::Folder;
use crate::include_graph::{IncludeSite, ResolvedFile, build_include_graph};
use crate::interner::SourceInterner;

/// One included file of a closure: its identity, its text and the
/// green tree it parsed to.
#[derive(Debug, Clone)]
pub struct ClosureFile {
    /// The id every span in this file's tree carries.
    pub source: SourceId,
    /// Canonical path — the key `resolved` and `include_sites` use.
    pub path: PathBuf,
    /// The file's text, shared with the folder that loaded it.
    pub text: Arc<str>,
    /// The version the file was lexed and parsed at: its own
    /// `@version` pragma, else the entry's settled version.
    pub version: Version,
    /// The parse result. Cast to a red tree by whoever needs to walk it.
    pub green: GreenNode,
}

/// Everything an entry file's `include("…")` statements pull in.
///
/// The files are in dependency order, leaves first, and **exclude the
/// entry**: the entry's own tree belongs to the parse of the entry, not
/// to its include closure.
#[derive(Debug, Clone)]
pub struct IncludeClosure {
    /// Canonical path of the entry file, the key its own include sites
    /// are recorded under.
    pub entry_path: PathBuf,
    /// The included files, leaves first.
    pub files: Vec<ClosureFile>,
    /// Every `class IDENT` name declared anywhere in the closure, the
    /// entry included, sorted and deduplicated. Upstream resolves
    /// potential type words against the program-wide defined-class set,
    /// so the entry parse needs this to read `lowercaseClassFromInclude
    /// x = …` as a typed declaration.
    pub class_names: Vec<String>,
    /// Forward edges keyed by canonical path. Callers (LSP, miku) use
    /// them to invalidate caches when a leaf changes.
    pub forward: BTreeMap<PathBuf, BTreeSet<PathBuf>>,
    /// `(includer_canonical, include_name)` → included canonical path.
    /// The passes that expand include sites resolve names through this.
    pub resolved: BTreeMap<(PathBuf, String), PathBuf>,
    /// Every `include("…")` site that resolved to a given included
    /// file, so a caller can point at the sites a file is reached from.
    pub include_sites: BTreeMap<PathBuf, Vec<IncludeSite>>,
}

/// Walk `entry_text`'s includes, parse the closure, and return it
/// together with every diagnostic the walk and those parses raised.
///
/// `entry_version` is the entry's settled language version — the
/// caller's, never re-derived from the text — and pragma-less included
/// files inherit it. `flags` are the run's experimental opt-ins; they
/// reach the included files' parses exactly as they reach the entry's.
///
/// Diagnostics come back in report order: the walk's (missing files,
/// cycles) first, then, per included file in dependency order, an
/// [`INCLUDE_PARSE_FAILED`](codes::INCLUDE_PARSE_FAILED) for each
/// `include("…")` site that reaches a file whose parse failed,
/// followed by that file's own lex and parse diagnostics.
pub fn resolve_include_closure(
    entry_path: &Path,
    entry_text: &str,
    entry_version: Version,
    folder: &dyn Folder,
    interner: &dyn SourceInterner,
    flags: FeatureFlags,
) -> (IncludeClosure, Vec<Diagnostic>) {
    let entry_path = canonical_or_normalized(entry_path);
    let graph = build_include_graph(&entry_path, entry_text, entry_version, folder, |p| {
        interner.intern(p)
    });
    let mut diagnostics = graph.diagnostics;

    // The class set is program-wide, so it spans the entry too, and it
    // must be complete before the first file is parsed: a class
    // declared in a leaf is a type head in every other file.
    let mut class_names: Vec<String> = graph
        .files
        .iter()
        .flat_map(|f| f.classes.iter().cloned())
        .collect();
    class_names.sort();
    class_names.dedup();

    let mut files: Vec<ClosureFile> = Vec::with_capacity(graph.files.len().saturating_sub(1));
    for file in graph.files {
        let ResolvedFile {
            source,
            path,
            text,
            version,
            classes: _,
        } = file;
        if path == entry_path {
            continue;
        }
        let parsed = parse_file_with(
            &text,
            source,
            &ParseOptions::new(version)
                .with_flags(flags)
                .with_extra_classes(&class_names),
        );
        diagnostics.extend(include_parse_failures(
            &path,
            source,
            graph.include_sites.get(&path).map(Vec::as_slice),
            &parsed.diagnostics,
        ));
        diagnostics.extend(parsed.diagnostics);
        files.push(ClosureFile {
            source,
            path,
            text,
            version,
            green: parsed.green,
        });
    }

    (
        IncludeClosure {
            entry_path,
            files,
            class_names,
            forward: graph.forward,
            resolved: graph.resolved,
            include_sites: graph.include_sites,
        },
        diagnostics,
    )
}

/// The [`INCLUDE_PARSE_FAILED`](codes::INCLUDE_PARSE_FAILED)
/// diagnostics an included file's parse earns: one per `include("…")`
/// site that reaches it, or one anchored on the file itself when the
/// graph recorded no site for it.
///
/// The parse always yields a tree — error recovery builds `ErrorNode`s
/// inside the `SourceFile` root rather than failing the root cast — so
/// a broken include is only visible in `parse_diagnostics`. Reporting
/// the chain at the `include(...)` site too is what keeps the entry
/// file's author from seeing errors that point only into a file they
/// may not have open. Errors only: a lint in an included file must not
/// mark the include site.
///
/// Split out of [`resolve_include_closure`] so the memoized path can
/// re-derive exactly these diagnostics from its cached parses — see
/// `leek_db::queries::include_parse_failures`. A diagnostic produced
/// once while a cache is filled and never again is the failure this
/// sharing exists to prevent.
#[must_use]
pub fn include_parse_failures(
    path: &Path,
    source: SourceId,
    sites: Option<&[IncludeSite]>,
    parse_diagnostics: &[Diagnostic],
) -> Vec<Diagnostic> {
    if !parse_diagnostics
        .iter()
        .any(|d| d.severity == Severity::Error)
    {
        return Vec::new();
    }
    match sites {
        Some(sites) => sites
            .iter()
            .map(|site| {
                diag!(
                    codes::INCLUDE_PARSE_FAILED,
                    site.span,
                    "included file `{}` failed to parse",
                    path.display(),
                )
            })
            .collect(),
        None => vec![diag!(
            codes::INCLUDE_PARSE_FAILED,
            Span::new(source, 0, 0),
            "included file `{}` failed to parse",
            path.display(),
        )],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::folder::MemFolder;
    use crate::interner::PathInterner;

    fn closure(entry: &str, files: &[(&str, &str)]) -> (IncludeClosure, Vec<Diagnostic>) {
        let mut folder = MemFolder::new();
        for (p, t) in files {
            folder.insert(*p, *t);
        }
        let entry_text = files
            .iter()
            .find(|(p, _)| *p == entry)
            .map(|(_, t)| *t)
            .expect("entry exists in fixture");
        resolve_include_closure(
            Path::new(entry),
            entry_text,
            Version::V4,
            &folder,
            &PathInterner::new(),
            FeatureFlags::none(),
        )
    }

    #[test]
    fn the_closure_holds_the_includes_leaves_first_and_never_the_entry() {
        let (closure, diags) = closure(
            "/main.leek",
            &[
                ("/main.leek", "include(\"a\")\nvar m = 1;\n"),
                ("/a.leek", "include(\"b\")\nvar a = 1;\n"),
                ("/b.leek", "var b = 1;\n"),
            ],
        );
        assert!(diags.is_empty(), "{diags:?}");
        let paths: Vec<_> = closure.files.iter().map(|f| f.path.clone()).collect();
        assert_eq!(
            paths,
            [PathBuf::from("/b.leek"), PathBuf::from("/a.leek")],
            "leaves first, entry absent"
        );
        assert_eq!(closure.entry_path, PathBuf::from("/main.leek"));
    }

    /// The class set is what lets the entry parse `thing t = …` as a
    /// declaration, so it has to span the whole closure — the entry's
    /// own classes included, even though the entry is not a member of
    /// `files`.
    #[test]
    fn class_names_span_the_entry_and_its_includes() {
        let (closure, _) = closure(
            "/main.leek",
            &[
                ("/main.leek", "include(\"a\")\nclass entryClass {}\n"),
                ("/a.leek", "class leafClass {}\nclass leafClass2 {}\n"),
            ],
        );
        assert_eq!(
            closure.class_names,
            ["entryClass", "leafClass", "leafClass2"],
            "sorted and deduplicated across the closure"
        );
    }

    /// A class declared in one included file must be a type head in
    /// another: the parse of every file sees the whole closure's set.
    #[test]
    fn a_class_from_one_include_is_a_type_head_in_another() {
        let (_, diags) = closure(
            "/main.leek",
            &[
                ("/main.leek", "include(\"a\")\ninclude(\"b\")\n"),
                ("/a.leek", "class thing { constructor() {} }\n"),
                ("/b.leek", "thing t = new thing();\n"),
            ],
        );
        assert!(diags.is_empty(), "{diags:?}");
    }

    #[test]
    fn a_broken_include_is_reported_at_every_site_that_reaches_it() {
        let (_, diags) = closure(
            "/main.leek",
            &[
                ("/main.leek", "include(\"a\")\ninclude(\"b\")\n"),
                ("/a.leek", "include(\"bad\")\n"),
                ("/b.leek", "include(\"bad\")\n"),
                ("/bad.leek", "function ( { var\n"),
            ],
        );
        let sites: Vec<_> = diags
            .iter()
            .filter(|d| d.code == codes::INCLUDE_PARSE_FAILED)
            .map(|d| d.span.source)
            .collect();
        assert_eq!(sites.len(), 2, "one per include site: {diags:?}");
        assert_ne!(sites[0], sites[1], "the two sites are in different files");
    }

    /// The walk's diagnostics precede the parses', and a file's
    /// `INCLUDE_PARSE_FAILED` precedes its own parse errors. Consumers
    /// render the list in order, so the order is part of the behaviour.
    #[test]
    fn the_walk_reports_before_the_parses_do() {
        let (_, diags) = closure(
            "/main.leek",
            &[
                ("/main.leek", "include(\"ghost\")\ninclude(\"bad\")\n"),
                ("/bad.leek", "function ( { var\n"),
            ],
        );
        let codes: Vec<_> = diags.iter().map(|d| d.code.name().to_string()).collect();
        assert_eq!(codes[0], codes::INCLUDE_NOT_FOUND.name(), "{codes:?}");
        assert_eq!(codes[1], codes::INCLUDE_PARSE_FAILED.name(), "{codes:?}");
        assert!(codes.len() > 2, "the parse errors follow: {codes:?}");
    }

    /// Lex diagnostics reach the caller exactly once. The walk lexes
    /// every file to find its includes and drops what that lex says;
    /// only the parse reports.
    #[test]
    fn a_lex_error_in_an_include_is_reported_once() {
        let (_, diags) = closure(
            "/main.leek",
            &[
                ("/main.leek", "include(\"bad\")\n"),
                ("/bad.leek", "var s = \"unterminated;\n"),
            ],
        );
        let lex_errors = diags
            .iter()
            .filter(|d| d.code == codes::STRING_NOT_CLOSED)
            .count();
        assert_eq!(lex_errors, 1, "{diags:?}");
    }

    /// The closure is a value, not a tree of cursors: its trees are
    /// green, and casting one to a red root is the consumer's job.
    #[test]
    fn a_closure_file_carries_the_tree_it_parsed_to() {
        use leek_parser::ast::AstNode;

        let (closure, _) = closure(
            "/main.leek",
            &[
                ("/main.leek", "include(\"a\")\n"),
                ("/a.leek", "function helper() {}\n"),
            ],
        );
        let file = &closure.files[0];
        let root = leek_syntax::SyntaxNode::new_root(file.green.clone());
        assert!(leek_parser::ast::SourceFile::cast(root).is_some());
    }
}
