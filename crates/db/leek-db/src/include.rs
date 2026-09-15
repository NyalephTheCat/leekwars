//! The include closure as tracked queries.
//!
//! Three queries, layered so that each one re-runs on the smallest
//! change that can affect it:
//!
//! 1. [`include_edges`] — the `include("…")` sites and `class IDENT`
//!    names in one file, over
//!    [`lex_query`](leek_lexer::pipeline::lex_query). Re-runs when that
//!    file's tokens change and nothing else.
//! 2. [`resolve_include`] — one `include("name")` written in one file,
//!    resolved against [`WorkspaceFiles`]. Re-runs when the set of
//!    files in the workspace changes, not when any file's *text*
//!    changes.
//! 3. [`include_graph`] — the DFS over the two above plus
//!    [`pragma_query`](leek_syntax::pipeline::pragma_query), settling
//!    each reached file's version and recording the edges, the
//!    dependency order and the include sites.
//!
//! [`include_parse_failures`] hangs off the graph for the consumers
//! that need to know an included file did not parse, and
//! [`program_classes`] folds the graph into the interned class set every
//! parse in the program is keyed on.
//!
//! ### Token level, never parse level
//!
//! [`include_edges`] reads *tokens*. It must: the parse of any file in
//! a program depends on the program-wide `class` set, which is
//! collected from the include closure, so an include graph built over
//! parses would be a cycle. Tokens break it, and the same scan —
//! [`leek_resolver::include_graph::scan_include_edges`] — backs the
//! non-memoized [`build_include_graph`](leek_resolver::include_graph::build_include_graph)
//! walk, so the two cannot drift on what counts as an include site.
//!
//! ### Why `entry_version` is a key and not a field
//!
//! A pragma-less included file inherits the *entry's* settled version
//! (`docs/semantics.md` §2.2, pinned by
//! `entry_uses_caller_version_and_pragmaless_include_inherits_it` in
//! `leek-resolver`). So the very same leaf file is a v2 file inside a
//! v2 program and a v4 file inside a v4 one. Keying the graph on the
//! entry's version keeps those two answers apart; folding it into the
//! graph's *value* instead would let a v4 program read a v2 program's
//! settled versions back out of the cache.
//!
//! ### Layering
//!
//! Every query body calls *down* into `leek-resolver`'s pure
//! functions. `leek-resolver` gains no call back up into this crate:
//! [`ResolveIncludes`](leek_resolver::pipeline::ResolveIncludes) still
//! drives the same pure walk it always did, because middle → db is the
//! breach `cargo xtask check-layers` exists to reject.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use leek_diagnostics::{Diagnostic, IntoDiagnostic, codes, diag};
use leek_resolver::folder::{IncludeError, LoadError, include_candidates};
use leek_resolver::include_graph::{IncludeEdges, IncludeSite};
use leek_span::paths::canonical_or_normalized;
use leek_span::{SourceId, Span};
use leek_syntax::Version;

use crate::{Db, ProgramClasses, SourceFile, WorkspaceFiles};

/// One file the include walk reached: who it is, not what it says.
///
/// Deliberately no tree. A rowan red tree is a per-thread cursor with
/// interior mutability, so it may never enter a query result; even the
/// green tree stays out, because the graph's whole job is to tell a
/// caller *which* files to ask [`parse_query`](leek_parser::pipeline::parse_query)
/// about. Adding a tree here would make the graph re-run on every edit
/// to any file it reaches.
#[derive(Clone, PartialEq, Eq, salsa::Update)]
pub struct IncludeGraphFile {
    /// The workspace input this file's text and queries hang off.
    pub file: SourceFile,
    /// Canonical path — the key every map in [`IncludeGraph`] uses.
    pub path: PathBuf,
    /// The id every span in this file carries.
    pub source: SourceId,
    /// The version this file is settled at: its own `@version` pragma
    /// if it has one, else the entry's — see the [module docs](self).
    pub version: Version,
    /// The `class IDENT` names declared in this file, in declaration
    /// order, as its include scan saw them.
    pub classes: Vec<String>,
}

/// Renders everything but the salsa handle: a [`SourceFile`] is an id
/// into a database, so printing it without one would print an integer
/// and call it a file. The path beside it is the readable identity.
impl std::fmt::Debug for IncludeGraphFile {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("IncludeGraphFile")
            .field("path", &self.path)
            .field("source", &self.source)
            .field("version", &self.version)
            .field("classes", &self.classes)
            .finish_non_exhaustive()
    }
}

/// Everything an entry file's `include("…")` statements reach.
///
/// The same shape as
/// [`IncludeGraphResult`](leek_resolver::include_graph::IncludeGraphResult),
/// minus the file texts and plus the [`SourceFile`] handles — paths,
/// ids, versions, edges and include sites only, so the value is cheap
/// to compare and safe to keep across revisions.
#[derive(Debug, Clone, Default, PartialEq, Eq, salsa::Update)]
pub struct IncludeGraph {
    /// Canonical path of the entry file.
    pub entry_path: PathBuf,
    /// Every reached file in dependency order — each one after
    /// everything it transitively includes, the entry last.
    pub files: Vec<IncludeGraphFile>,
    /// Forward edges keyed by canonical path.
    pub forward: BTreeMap<PathBuf, BTreeSet<PathBuf>>,
    /// `(includer_canonical, include_name)` → included canonical path.
    pub resolved: BTreeMap<(PathBuf, String), PathBuf>,
    /// Every `include("…")` site that reached a given file.
    pub include_sites: BTreeMap<PathBuf, Vec<IncludeSite>>,
    /// What the walk itself found wrong: unresolved names and cycles.
    ///
    /// Carried in the value rather than emitted while walking, so a
    /// cache *hit* reports them exactly as a miss does. A diagnostic
    /// raised only while a memo is filled is a diagnostic the second
    /// run silently drops.
    pub diagnostics: Vec<Diagnostic>,
}

impl IncludeGraph {
    /// The included files — everything but the entry, in dependency
    /// order, leaves first.
    pub fn includes(&self) -> impl Iterator<Item = &IncludeGraphFile> {
        self.files.iter().filter(|f| f.path != self.entry_path)
    }

    /// Every `class IDENT` name declared anywhere in the closure, the
    /// entry included, sorted and deduplicated — the program-wide set
    /// a parse needs to read `lowercaseClassFromInclude x = …` as a
    /// typed declaration.
    #[must_use]
    pub fn class_names(&self) -> Vec<String> {
        let mut names: Vec<String> = self
            .files
            .iter()
            .flat_map(|f| f.classes.iter().cloned())
            .collect();
        names.sort();
        names.dedup();
        names
    }

    /// The reached file at `path`, if the walk got there.
    #[must_use]
    pub fn file(&self, path: &Path) -> Option<&IncludeGraphFile> {
        self.files.iter().find(|f| f.path == path)
    }
}

/// The `include("…")` sites and `class IDENT` names in one file.
///
/// Over [`lex_query`](leek_lexer::pipeline::lex_query), so editing a
/// file re-lexes and re-scans that file and leaves every other file's
/// edges cached.
#[salsa::tracked]
pub fn include_edges(db: &dyn Db, file: SourceFile) -> IncludeEdges {
    let lexed = leek_lexer::pipeline::lex_query(db, file);
    leek_resolver::include_graph::scan_include_edges(file.text(db), &lexed.tokens)
}

/// The `class IDENT` names one file declares, in declaration order.
///
/// The same token-level scan [`include_edges`] runs, asked on its own so
/// that it can answer on its own: a file whose `include("…")` sites
/// changed but whose classes did not leaves this query's value equal,
/// salsa backdates it, and [`program_classes`] — and therefore every
/// parse in the program — is left alone.
///
/// Over [`lex_query`](leek_lexer::pipeline::lex_query) for the same
/// reason [`include_edges`] is: a program's class set decides how its
/// files parse, so deriving it from a parse would be a cycle.
#[salsa::tracked]
pub fn class_names(db: &dyn Db, file: SourceFile) -> Vec<String> {
    let lexed = leek_lexer::pipeline::lex_query(db, file);
    leek_parser::scan_class_names(file.text(db), &lexed.tokens)
}

/// Every `class IDENT` name declared anywhere in `entry`'s include
/// closure, sorted and deduplicated, interned as the key each file's
/// [`parse_query`](leek_parser::pipeline::parse_query) is asked for.
///
/// Upstream resolves a potential type word against the program-wide
/// defined-class set, so a class declared in any file of the closure is
/// a type head in every other — `lowercaseClassFromInclude x = …` has
/// to parse as a typed declaration. That set is a property of the
/// *program*: the same leaf, included by two different entries, has two
/// of them. Hence a query keyed exactly as [`include_graph`] is, whose
/// interned result is then a parse key rather than a field on any
/// file's input.
///
/// This is what retires the LSP's workspace-wide union (#163): a class
/// typed into one AI no longer changes how an unrelated AI parses, and
/// an edit that leaves a program's class set alone re-parses only the
/// file that changed instead of every open document.
#[salsa::tracked]
pub fn program_classes(
    db: &dyn Db,
    files: WorkspaceFiles,
    entry: SourceFile,
    entry_version: Version,
) -> ProgramClasses<'_> {
    let graph = include_graph(db, files, entry, entry_version);
    let mut names: Vec<String> = graph
        .files
        .iter()
        .flat_map(|file| class_names(db, file.file))
        .collect();
    names.sort();
    names.dedup();
    ProgramClasses::new(db, names)
}

/// One `include("name")` as written in one file — the key
/// [`resolve_include`] memoizes on.
///
/// Interned rather than passed as a loose `(String, String)` pair so
/// the key is `Copy` and has one identity: every site in the workspace
/// that spells the same name from the same file shares a memo, and no
/// caller can accidentally key on a differently-normalized spelling of
/// the includer's path.
#[salsa::interned]
pub struct IncludeRef<'db> {
    /// Canonical path of the file the `include("…")` is written in —
    /// the same spelling [`SourceFile::canonical_path`] carries.
    #[returns(ref)]
    pub includer_path: String,
    /// The text between the quotes, exactly as the token scan spells
    /// it (stripped, never unescaped).
    #[returns(ref)]
    pub name: String,
}

/// The workspace file an [`IncludeRef`] names — or `None` when the
/// workspace holds no file for it.
///
/// The candidate order is [`include_candidates`]'s, the one spelling
/// shared with `DiskFolder`, `MemFolder` and (until this query) a
/// fourth copy in the LSP: sibling `<dir>/<name>.leek` first, bare
/// `<dir>/<name>` second.
///
/// Reads [`WorkspaceFiles`], so opening or deleting *any* file
/// re-resolves every include name, while editing a file's text
/// re-resolves nothing.
#[salsa::tracked]
pub fn resolve_include<'db>(
    db: &'db dyn Db,
    files: WorkspaceFiles,
    site: IncludeRef<'db>,
) -> Option<SourceFile> {
    include_candidates(Path::new(site.includer_path(db)), site.name(db))
        .iter()
        .find_map(|candidate| {
            files.get(
                db,
                &canonical_or_normalized(candidate).display().to_string(),
            )
        })
}

/// Walk `entry`'s includes transitively.
///
/// `entry_version` is the entry's *settled* version — the caller's,
/// never re-derived from the entry's own pragma, matching
/// [`build_include_graph`](leek_resolver::include_graph::build_include_graph).
/// It is part of the key: see the [module docs](self).
#[salsa::tracked]
pub fn include_graph(
    db: &dyn Db,
    files: WorkspaceFiles,
    entry: SourceFile,
    entry_version: Version,
) -> IncludeGraph {
    let entry_path = PathBuf::from(entry.canonical_path(db));
    let mut walk = Walk {
        db,
        files,
        entry_version,
        known: BTreeMap::new(),
        forward: BTreeMap::new(),
        resolved: BTreeMap::new(),
        include_sites: BTreeMap::new(),
        diagnostics: Vec::new(),
        order: Vec::new(),
        visiting: Vec::new(),
        done: BTreeSet::new(),
    };
    walk.known.insert(
        entry_path.clone(),
        IncludeGraphFile {
            file: entry,
            path: entry_path.clone(),
            source: entry.source(db),
            version: entry_version,
            classes: Vec::new(),
        },
    );
    walk.visit(entry_path.clone(), None);

    let Walk {
        mut known,
        forward,
        resolved,
        include_sites,
        diagnostics,
        order,
        ..
    } = walk;
    IncludeGraph {
        entry_path,
        files: order.into_iter().filter_map(|p| known.remove(&p)).collect(),
        forward,
        resolved,
        include_sites,
        diagnostics,
    }
}

/// The [`INCLUDE_PARSE_FAILED`](codes::INCLUDE_PARSE_FAILED)
/// diagnostics the closure of `entry` earns: one per `include("…")`
/// site that reaches a file whose parse raised an error, in the same
/// dependency order — and with the same text and spans —
/// [`resolve_include_closure`](leek_resolver::closure::resolve_include_closure)
/// produces them in.
///
/// They are re-derived from each leaf's memoized
/// [`parse_query`](leek_parser::pipeline::parse_query) and returned in
/// the value, so a run that hits every cache still reports them. That
/// is the whole point of this query existing separately from
/// [`include_graph`]: the graph must *not* depend on any leaf's parse
/// (editing a leaf would then re-run the graph's parse too), and a
/// diagnostic produced only while filling a cache is a diagnostic the
/// second run drops on the floor.
#[salsa::tracked]
pub fn include_parse_failures(
    db: &dyn Db,
    files: WorkspaceFiles,
    entry: SourceFile,
    entry_version: Version,
) -> Vec<Diagnostic> {
    let graph = include_graph(db, files, entry, entry_version);
    // The same parse key the program's own passes use, so this reads the
    // memo they filled instead of parsing the closure a second time
    // under an empty class set.
    let classes = program_classes(db, files, entry, entry_version);
    graph
        .includes()
        .flat_map(|file| {
            let parse = leek_parser::pipeline::parse_query(db, file.file, classes);
            leek_resolver::closure::include_parse_failures(
                &file.path,
                file.source,
                graph.include_sites.get(&file.path).map(Vec::as_slice),
                &parse.diagnostics,
            )
        })
        .collect()
}

/// DFS state for [`include_graph`], mirroring the pure walk in
/// [`leek_resolver::include_graph`]: `visiting` is the current path
/// stack (cycle detection), `done` everything already finalised, and
/// `order` the post-order the entry lands last in.
struct Walk<'db> {
    db: &'db dyn Db,
    files: WorkspaceFiles,
    entry_version: Version,
    known: BTreeMap<PathBuf, IncludeGraphFile>,
    forward: BTreeMap<PathBuf, BTreeSet<PathBuf>>,
    resolved: BTreeMap<(PathBuf, String), PathBuf>,
    include_sites: BTreeMap<PathBuf, Vec<IncludeSite>>,
    diagnostics: Vec<Diagnostic>,
    order: Vec<PathBuf>,
    visiting: Vec<PathBuf>,
    done: BTreeSet<PathBuf>,
}

impl Walk<'_> {
    /// Walk `current`, reached from the `include("…")` literal at
    /// `site` (`None` for the entry, which nothing includes).
    fn visit(&mut self, current: PathBuf, site: Option<Span>) {
        if self.done.contains(&current) {
            return;
        }
        let Some(known) = self.known.get(&current) else {
            return;
        };
        let (file, source) = (known.file, known.source);
        if self.visiting.contains(&current) {
            // Cycle: report at the `include("…")` that closed it. Only
            // the entry has no site; fall back to a zero span on the
            // file's own source id there.
            self.diagnostics.push(diag!(
                codes::CIRCULAR_INCLUDE,
                site.unwrap_or_else(|| Span::new(source, 0, 0)),
                "circular include involving `{}`",
                current.display(),
            ));
            return;
        }
        self.visiting.push(current.clone());

        let edges = include_edges(self.db, file);
        if let Some(known) = self.known.get_mut(&current) {
            known.classes = edges.classes;
        }
        for inc in &edges.includes {
            let include_ref =
                IncludeRef::new(self.db, current.display().to_string(), inc.name.clone());
            let Some(target) = resolve_include(self.db, self.files, include_ref) else {
                // The workspace holds no file for the name. Same
                // diagnostic the folder-backed walk raises, built from
                // the same type so the two cannot word it differently.
                self.diagnostics.push(
                    IncludeError {
                        cause: LoadError::NotFound,
                        span: inc.span,
                        name: &inc.name,
                    }
                    .into_diagnostic(),
                );
                continue;
            };
            let path = PathBuf::from(target.canonical_path(self.db));
            if !self.known.contains_key(&path) {
                // Its own `@version` pragma if it has one, else the
                // entry's settled version.
                let version = leek_syntax::pipeline::pragma_query(self.db, target)
                    .pragmas
                    .effective_version(self.entry_version);
                self.known.insert(
                    path.clone(),
                    IncludeGraphFile {
                        file: target,
                        path: path.clone(),
                        source: target.source(self.db),
                        version,
                        classes: Vec::new(),
                    },
                );
            }
            self.forward
                .entry(current.clone())
                .or_default()
                .insert(path.clone());
            self.resolved
                .insert((current.clone(), inc.name.clone()), path.clone());
            self.include_sites
                .entry(path.clone())
                .or_default()
                .push(IncludeSite {
                    includer: current.clone(),
                    span: inc.span,
                });
            self.visit(path, Some(inc.span));
        }

        self.visiting.pop();
        self.done.insert(current.clone());
        self.order.push(current);
    }
}
