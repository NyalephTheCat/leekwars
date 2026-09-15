//! Build the include dependency graph for an entry source file.
//!
//! Walks `include("name")` statements transitively using a
//! [`Folder`] for I/O. Stops on cycles (reported as
//! [`CIRCULAR_INCLUDE`](leek_diagnostics::codes::CIRCULAR_INCLUDE)) and
//! missing files (reported as
//! [`INCLUDE_NOT_FOUND`](leek_diagnostics::codes::INCLUDE_NOT_FOUND)).
//! Returns a topologically-ordered list of
//! `LoadedFile`s — entry last — so callers can pre-declare items
//! from leaves first and the entry inherits everything.
//!
//! ### Version-aware lexing
//!
//! Each file's `include(...)` calls must be extracted with that
//! file's own `@version` pragma applied. Otherwise a v2 file using
//! `and` as a keyword gets tokenized at v4 (where `and` is an
//! identifier) and the cached token stream is wrong for the next
//! real compile pass (`docs/semantics.md` §2.2). We honor this
//! by re-parsing each file with its declared version before
//! scanning for include tokens.
//!
//! The entry file's version is the caller's settled `Input::version_byte`
//! (never re-derived here). An included file uses its own explicit
//! `@version` pragma when it has one and otherwise **inherits the entry's
//! version**, so a pragma-less include in a v2 program is lexed, parsed,
//! resolved, checked and lowered at v2 rather than silently at v4.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use leek_diagnostics::{Diagnostic, IntoDiagnostic, codes, diag};
use leek_lexer::lex;
use leek_span::paths::canonical_or_normalized;
use leek_span::{SourceId, Span};
use leek_syntax::{SyntaxKind, SyntaxNode, Version, parse_pragmas};

use crate::folder::{Folder, IncludeError, LoadedFile};

/// Build outcome — the ordered file list plus the forward edge
/// map and any diagnostics raised during the walk.
pub struct IncludeGraphResult {
    /// Files in dependency order: every file appears after all the
    /// files it transitively includes. The entry file is last. The
    /// HIR lowerer iterates in this order so when it lowers `Main`,
    /// the included files' symbols are already pre-declared.
    pub files: Vec<ResolvedFile>,
    /// Forward edges keyed by canonical path. Used by callers
    /// (LSP, miku) to invalidate caches when a leaf changes.
    pub forward: BTreeMap<PathBuf, BTreeSet<PathBuf>>,
    /// Maps `(includer_canonical, include_name_text)` to the
    /// included file's canonical path. Used by the HIR lowerer when
    /// it walks a `Stmt::Include("name")` and needs to splice in
    /// the right file's main block.
    pub resolved: BTreeMap<(PathBuf, String), PathBuf>,
    /// Diagnostics raised while walking. Cycle and missing-file
    /// errors land here; the caller routes them into the regular
    /// diagnostic stream.
    pub diagnostics: Vec<Diagnostic>,
    /// Every `include("…")` site that resolved to a given included
    /// file. Used to attach parse failures at the include call site.
    pub include_sites: BTreeMap<PathBuf, Vec<IncludeSite>>,
}

/// One `include("…")` call site in an includer file.
///
/// Comparable and `salsa::Update`-able so it can ride inside a tracked
/// query's return value — see `leek_db::queries::include_graph`.
#[derive(salsa::Update, Debug, Clone, PartialEq, Eq)]
pub struct IncludeSite {
    pub includer: PathBuf,
    pub span: Span,
}

/// One file's parsed identity — canonical path, contents, the
/// `@version` pragma we used to tokenize its includes, and the
/// `SourceId` the caller stamped on it.
#[derive(Debug, Clone)]
pub struct ResolvedFile {
    pub source: SourceId,
    pub path: PathBuf,
    pub text: Arc<str>,
    pub version: Version,
    /// The `class IDENT` names declared in this file, in declaration
    /// order. A by-product of the include scan's lex (same text, same
    /// source, same version), so callers that need the closure's class
    /// set — the entry parse does, to recognise a lowercase class from
    /// an included file as a type head — read it from here instead of
    /// lexing every file a second time.
    pub classes: Vec<String>,
}

impl ResolvedFile {
    /// Pragma byte (1..=4) for downstream consumers that prefer the
    /// pipeline's byte representation of the version.
    pub fn version_byte(&self) -> u8 {
        u8::from(self.version)
    }
}

/// Locate `include("…")` references in `text`, run them through the
/// folder transitively, and report cycles / missing files.
///
/// `source_for(path)` is the caller's `SourceId` allocator —
/// the resolver/HIR keep span tables per-file, so every file the
/// walker discovers gets a fresh, stable id. Callers that don't
/// care about ids (e.g. simple tests) can pass `|_| SourceId::new(1).unwrap()`
/// — diagnostics still attach to the right text but spans across
/// files become indistinguishable.
///
/// `entry_version` is the entry's settled language version (the
/// pipeline's `Input::version_byte`); pragma-less included files
/// inherit it.
pub fn build_include_graph(
    entry_path: &Path,
    entry_text: &str,
    entry_version: Version,
    folder: &dyn Folder,
    mut source_for: impl FnMut(&Path) -> SourceId,
) -> IncludeGraphResult {
    let mut files: BTreeMap<PathBuf, ResolvedFile> = BTreeMap::new();
    let mut forward: BTreeMap<PathBuf, BTreeSet<PathBuf>> = BTreeMap::new();
    let mut resolved: BTreeMap<(PathBuf, String), PathBuf> = BTreeMap::new();
    let mut diagnostics: Vec<Diagnostic> = Vec::new();
    let mut include_sites: BTreeMap<PathBuf, Vec<IncludeSite>> = BTreeMap::new();
    let mut order: Vec<PathBuf> = Vec::new();

    // DFS state — `visiting` is the current path stack (cycle
    // detection); `done` is everything we've already finalised.
    let mut visiting: Vec<PathBuf> = Vec::new();
    let mut done: BTreeSet<PathBuf> = BTreeSet::new();

    // Seed the entry file.
    let entry_canonical = canonical_or_normalized(entry_path);
    let entry_source = source_for(&entry_canonical);
    files.insert(
        entry_canonical.clone(),
        ResolvedFile {
            source: entry_source,
            path: entry_canonical.clone(),
            text: Arc::from(entry_text),
            version: entry_version,
            classes: Vec::new(),
        },
    );

    fn walk(
        current: PathBuf,
        files: &mut BTreeMap<PathBuf, ResolvedFile>,
        forward: &mut BTreeMap<PathBuf, BTreeSet<PathBuf>>,
        resolved: &mut BTreeMap<(PathBuf, String), PathBuf>,
        include_sites: &mut BTreeMap<PathBuf, Vec<IncludeSite>>,
        diagnostics: &mut Vec<Diagnostic>,
        order: &mut Vec<PathBuf>,
        visiting: &mut Vec<PathBuf>,
        done: &mut BTreeSet<PathBuf>,
        folder: &dyn Folder,
        source_for: &mut dyn FnMut(&Path) -> SourceId,
        entry_version: Version,
        site: Option<Span>,
    ) {
        if done.contains(&current) {
            return;
        }
        if visiting.contains(&current) {
            // Cycle: report at the `include("…")` call that closed it,
            // which the caller passed down as `site`. Only the top-level
            // entry has no site; fall back to a zero span anchored on the
            // file's own source id there.
            let source = files.get(&current).map(|f| f.source);
            if let Some(source) = source {
                diagnostics.push(diag!(
                    codes::CIRCULAR_INCLUDE,
                    site.unwrap_or_else(|| Span::new(source, 0, 0)),
                    "circular include involving `{}`",
                    current.display(),
                ));
            }
            return;
        }
        visiting.push(current.clone());

        // One lex for this file, at its own version: the `include(...)`
        // calls to walk and the `class IDENT` names the closure's parse
        // needs. Borrowing the entry rather than cloning it keeps the
        // file's text out of the walk entirely.
        let file = files
            .get_mut(&current)
            .expect("file already inserted before walk");
        let scan = scan_file(&file.text, file.source, file.version);
        file.classes = scan.classes;

        for inc in &scan.includes {
            let loaded = folder.load(&current, &inc.name);
            match loaded {
                Ok(LoadedFile { path, text }) => {
                    if !files.contains_key(&path) {
                        let src = source_for(&path);
                        let ver = included_version(&text, entry_version);
                        files.insert(
                            path.clone(),
                            ResolvedFile {
                                source: src,
                                path: path.clone(),
                                text,
                                version: ver,
                                classes: Vec::new(),
                            },
                        );
                    }
                    forward
                        .entry(current.clone())
                        .or_default()
                        .insert(path.clone());
                    resolved.insert((current.clone(), inc.name.clone()), path.clone());
                    include_sites
                        .entry(path.clone())
                        .or_default()
                        .push(IncludeSite {
                            includer: current.clone(),
                            span: inc.span,
                        });
                    walk(
                        path,
                        files,
                        forward,
                        resolved,
                        include_sites,
                        diagnostics,
                        order,
                        visiting,
                        done,
                        folder,
                        source_for,
                        entry_version,
                        Some(inc.span),
                    );
                }
                Err(e) => {
                    diagnostics.push(
                        IncludeError {
                            cause: e,
                            span: inc.span,
                            name: &inc.name,
                        }
                        .into_diagnostic(),
                    );
                }
            }
        }

        visiting.pop();
        done.insert(current.clone());
        order.push(current);
    }

    walk(
        entry_canonical,
        &mut files,
        &mut forward,
        &mut resolved,
        &mut include_sites,
        &mut diagnostics,
        &mut order,
        &mut visiting,
        &mut done,
        folder,
        &mut source_for,
        entry_version,
        None,
    );

    // Project `order` (canonical paths) onto `ResolvedFile` so
    // callers receive a self-contained list.
    let ordered = order.into_iter().filter_map(|p| files.remove(&p)).collect();

    IncludeGraphResult {
        files: ordered,
        forward,
        resolved,
        diagnostics,
        include_sites,
    }
}

/// One `include(...)` call extracted from a file: the name as written,
/// without its quotes, and the span of the string literal it came from.
#[derive(salsa::Update, Debug, Clone, PartialEq, Eq)]
pub struct IncludeCall {
    pub name: String,
    pub span: Span,
}

/// What one lex of a file tells the walk: where it includes from and
/// what classes it declares.
///
/// This is the unit of work the memoized include graph is built from
/// (`leek_db::queries::include_edges`), which is why it is comparable
/// and `salsa::Update`-able.
#[derive(salsa::Update, Debug, Clone, Default, PartialEq, Eq)]
pub struct IncludeEdges {
    pub includes: Vec<IncludeCall>,
    pub classes: Vec<String>,
}

/// Token-level scan for `include("…")` and `class IDENT`. Uses the
/// file's own `@version` pragma so v2's `and`-keyword case doesn't get
/// mis-lexed. Returns the include names plus their string-literal
/// spans for diagnostic reporting, and the class names the closure's
/// parse must know about.
///
/// The lexer's own diagnostics are deliberately dropped: every file
/// this scan sees is lexed again by the parse that follows, and that
/// parse reports them. Keeping them here would double every lex
/// diagnostic in an included file.
fn scan_file(text: &str, source: SourceId, version: Version) -> IncludeEdges {
    let lexed = lex(text, source, version);
    scan_include_edges(text, &lexed.tokens)
}

/// The include sites and class declarations `tokens` (a lex of `text`)
/// contains.
///
/// Split out of [`scan_file`] so the tracked
/// `leek_db::queries::include_edges` can run the *same* scan over the
/// memoized `lex_query` token stream instead of lexing again. The walk
/// below and the query therefore cannot drift on what counts as an
/// include site.
///
/// **Token level, never parse level.** Reading includes off a parse
/// tree would make the include graph depend on the parse, and the
/// parse already depends on the graph for the program-wide class set —
/// a cycle. Tokens break it.
#[must_use]
pub fn scan_include_edges(text: &str, tokens: &[leek_syntax::Token]) -> IncludeEdges {
    let mut includes: Vec<IncludeCall> = Vec::new();
    // Walk a small state machine: KwInclude, LParen, StringLiteral,
    // optional RParen. Whitespace + comments are skipped via
    // `is_trivia`.
    let mut iter = tokens.iter().filter(|t| !t.kind.is_trivia());
    while let Some(t) = iter.next() {
        if t.kind != SyntaxKind::KwInclude {
            continue;
        }
        // Expect `(`.
        match iter.next() {
            Some(p) if p.kind == SyntaxKind::LParen => {}
            _ => continue,
        }
        // Expect a string literal — strip the surrounding quotes.
        if let Some(s) = iter.next()
            && s.kind == SyntaxKind::StringLiteral
        {
            let raw = &text[s.span.start as usize..s.span.end as usize];
            if raw.len() >= 2 {
                let name = raw[1..raw.len() - 1].to_string();
                includes.push(IncludeCall { name, span: s.span });
            }
        }
    }
    IncludeEdges {
        includes,
        classes: leek_parser::scan_class_names(text, tokens),
    }
}

/// An included file's version: its own explicit `@version` pragma,
/// else the entry's settled version.
fn included_version(text: &str, entry_version: Version) -> Version {
    // `parse_pragmas` wants a SourceId for its diagnostics; the
    // included file's own pragma diagnostics are not reported here.
    let (pragmas, _diags) = parse_pragmas(text, SourceId::new(1).unwrap());
    pragmas.effective_version(entry_version)
}

// ---- Include-site expansion, shared by the passes that walk ASTs ----

/// One file of a multi-file pass, in the owned form
/// [`IncludeExpander`] keeps. Rowan nodes are reference-counted, so
/// cloning a root out of the caller's parse costs a refcount bump and
/// frees the expander from the caller's lifetimes.
#[derive(Debug, Clone)]
pub struct ExpandUnit {
    /// Canonical path — the key `resolved` lookups use.
    pub path: PathBuf,
    /// The file's `SourceFile` syntax node.
    pub root: SyntaxNode,
    pub source: SourceId,
    pub version: Version,
}

/// What an `include("name")` site expands to: the included file's
/// syntax root plus the source id and version its spans and
/// version-dependent analysis must follow.
#[derive(Debug, Clone)]
pub struct Expansion {
    pub root: SyntaxNode,
    pub source: SourceId,
    pub version: Version,
}

impl Expansion {
    /// The included file's full source text. Computed on demand — only
    /// the passes that read doc comments need it.
    #[must_use]
    pub fn text(&self) -> String {
        self.root.text().to_string()
    }
}

/// Drives `include(...)` as inline expansion for a pass that walks the
/// ASTs rather than rewriting them.
///
/// Upstream's `include` is textual splicing, so every pass with an
/// execution order — lowering, resolution, type checking — must enter an
/// included file *at its include site*, in the state live there, rather
/// than in include-graph order (#118, #339). The three passes share this
/// one expander so they cannot drift on the rules:
///
/// - A file expands at most once, at the first site that reaches it, so a
///   diamond include doesn't run its main block twice.
/// - The entry counts as already expanded from the start. A cycle's back
///   edge lands in `resolved` before [`build_include_graph`] detects the
///   cycle, so an AST-driven walker without that seed recurses forever.
/// - A name that resolves to nothing (a missing file, already reported by
///   the graph walk) expands to nothing.
pub struct IncludeExpander {
    /// `(includer_canonical, include_name)` → included canonical path,
    /// from [`build_include_graph`].
    resolved: BTreeMap<(PathBuf, String), PathBuf>,
    /// Every unit of the run, keyed by canonical path.
    units: BTreeMap<PathBuf, ExpandUnit>,
    /// Files whose main block has already been expanded.
    already: BTreeSet<PathBuf>,
    /// Include stack; the back is the file being walked right now, which
    /// is the key `resolved` lookups use.
    stack: Vec<PathBuf>,
}

impl IncludeExpander {
    /// Arm expansion for a run whose entry file is `entry_path`.
    #[must_use]
    pub fn new(
        entry_path: &Path,
        units: impl IntoIterator<Item = ExpandUnit>,
        resolved: BTreeMap<(PathBuf, String), PathBuf>,
    ) -> Self {
        Self {
            resolved,
            units: units.into_iter().map(|u| (u.path.clone(), u)).collect(),
            already: std::iter::once(entry_path.to_path_buf()).collect(),
            stack: vec![entry_path.to_path_buf()],
        }
    }

    /// Re-root the stack at `path`, for walking that file's own function
    /// and class bodies: include sites inside them resolve relative to
    /// the file they are written in, not to whatever the main walk last
    /// entered.
    pub fn set_current(&mut self, path: &Path) {
        self.stack.clear();
        self.stack.push(path.to_path_buf());
    }

    /// Enter `include("name")` as written in the file currently on top
    /// of the stack.
    ///
    /// Returns the unit to walk and pushes it on the stack — the caller
    /// must call [`leave`](Self::leave) when it is done with it. `None`
    /// when the name resolves to nothing, the file has already been
    /// expanded, or it is not part of this run.
    pub fn enter(&mut self, name: &str) -> Option<Expansion> {
        let current = self.stack.last()?.clone();
        let path = self.resolved.get(&(current, name.to_string()))?.clone();
        if !self.already.insert(path.clone()) {
            return None;
        }
        let unit = self.units.get(&path)?;
        let expansion = Expansion {
            root: unit.root.clone(),
            source: unit.source,
            version: unit.version,
        };
        self.stack.push(path);
        Some(expansion)
    }

    /// Leave the file the matching [`enter`](Self::enter) returned.
    pub fn leave(&mut self) {
        self.stack.pop();
    }
}

/// The name in an `include("…")` statement, spelled as the key
/// [`IncludeGraphResult::resolved`] is built with.
///
/// Quotes are stripped without unescaping, matching the token scan the
/// graph walk itself uses — the two must agree or a site looks up a key
/// that was never inserted.
#[must_use]
pub fn include_name(include_stmt: &SyntaxNode) -> Option<String> {
    let token = include_stmt
        .children_with_tokens()
        .filter_map(rowan::NodeOrToken::into_token)
        .find(|t| t.kind() == SyntaxKind::StringLiteral)?;
    let text = token.text();
    (text.len() >= 2).then(|| text[1..text.len() - 1].to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::folder::MemFolder;

    fn build(entry: &str, files: &[(&str, &str)]) -> IncludeGraphResult {
        build_at(entry, Version::V4, files)
    }

    fn build_at(entry: &str, version: Version, files: &[(&str, &str)]) -> IncludeGraphResult {
        let mut folder = MemFolder::new();
        for (p, t) in files {
            folder.insert(*p, *t);
        }
        let entry_text = files
            .iter()
            .find(|(p, _)| *p == entry)
            .map(|(_, t)| (*t).to_string())
            .unwrap_or_default();
        let mut next: u32 = 1;
        build_include_graph(Path::new(entry), &entry_text, version, &folder, |_| {
            let id = SourceId::new(next).unwrap();
            next += 1;
            id
        })
    }

    fn version_of(result: &IncludeGraphResult, path: &str) -> Version {
        result
            .files
            .iter()
            .find(|f| f.path == Path::new(path))
            .expect("path present")
            .version
    }

    #[test]
    fn entry_uses_caller_version_and_pragmaless_include_inherits_it() {
        // The entry has no pragma: its version is the caller's (the
        // settled `Input::version_byte`), not a V4 default. The include
        // has no pragma either, so it inherits the entry's version.
        let result = build_at(
            "/main.leek",
            Version::V2,
            &[
                ("/main.leek", "include(\"util\")\n"),
                ("/util.leek", "var x = 1;\n"),
            ],
        );
        assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
        assert_eq!(version_of(&result, "/main.leek"), Version::V2);
        assert_eq!(version_of(&result, "/util.leek"), Version::V2);
    }

    #[test]
    fn include_pragma_overrides_entry_version() {
        let result = build_at(
            "/main.leek",
            Version::V4,
            &[
                ("/main.leek", "// @version:4\ninclude(\"old\")\n"),
                ("/old.leek", "// @version:1\nvar x = 1;\n"),
            ],
        );
        assert_eq!(version_of(&result, "/old.leek"), Version::V1);
    }

    #[test]
    fn entry_version_is_not_re_derived_from_its_pragma() {
        // The caller already settled the entry's version (e.g. a CLI
        // override); the walker must not second-guess it from the text.
        let result = build_at(
            "/main.leek",
            Version::V3,
            &[("/main.leek", "// @version:1\nvar x = 1;\n")],
        );
        assert_eq!(version_of(&result, "/main.leek"), Version::V3);
    }

    #[test]
    fn simple_chain_orders_includes_before_entry() {
        let result = build(
            "/main.leek",
            &[
                ("/main.leek", "include(\"util\")\nfunction main() {}"),
                ("/util.leek", "function helper() {}"),
            ],
        );
        assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
        let paths: Vec<_> = result.files.iter().map(|f| f.path.clone()).collect();
        assert_eq!(
            paths,
            [PathBuf::from("/util.leek"), PathBuf::from("/main.leek")]
        );
    }

    #[test]
    fn missing_include_reports_not_existing() {
        let result = build("/main.leek", &[("/main.leek", "include(\"ghost\")\n")]);
        assert_eq!(result.diagnostics.len(), 1);
        assert_eq!(
            result.diagnostics[0].code,
            leek_diagnostics::codes::INCLUDE_NOT_FOUND
        );
    }

    #[test]
    fn circular_include_detected() {
        let result = build(
            "/a.leek",
            &[("/a.leek", "include(\"b\")"), ("/b.leek", "include(\"a\")")],
        );
        let codes: Vec<_> = result
            .diagnostics
            .iter()
            .map(|d| d.code.0.to_string())
            .collect();
        assert!(
            codes.contains(&leek_diagnostics::codes::CIRCULAR_INCLUDE.0.to_string()),
            "expected circular-include diagnostic, got {codes:?}"
        );
    }

    #[test]
    fn circular_include_points_at_the_include_site() {
        // The diagnostic used to be anchored at offset 0..0 of the file
        // the cycle led back to, which points an editor at the wrong file
        // and the wrong line. It belongs on the `include("…")` that closed
        // the cycle — `/b.leek`'s, here.
        let b_text = "include(\"a\")";
        let result = build(
            "/a.leek",
            &[("/a.leek", "include(\"b\")"), ("/b.leek", b_text)],
        );
        let d = result
            .diagnostics
            .iter()
            .find(|d| d.code == leek_diagnostics::codes::CIRCULAR_INCLUDE)
            .expect("circular-include diagnostic");
        // `build` hands out source ids in discovery order: a = 1, b = 2.
        assert_eq!(d.span.source, SourceId::new(2).unwrap(), "wrong file");
        let start = u32::try_from(b_text.find("\"a\"").unwrap()).unwrap();
        assert_eq!(
            (d.span.start, d.span.end),
            (start, start + 3),
            "expected the include site's string literal, got {:?}",
            d.span
        );
    }

    #[test]
    fn duplicate_include_deduplicated() {
        // main → a, main → b, b → a.  `a` should appear only once.
        let result = build(
            "/main.leek",
            &[
                ("/main.leek", "include(\"a\")\ninclude(\"b\")"),
                ("/a.leek", "function alpha() {}"),
                ("/b.leek", "include(\"a\")"),
            ],
        );
        assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
        let paths: Vec<_> = result.files.iter().map(|f| f.path.clone()).collect();
        // Some valid topological order: a before b, a before main, b before main.
        let pos = |p: &str| {
            paths
                .iter()
                .position(|x| x == &PathBuf::from(p))
                .expect("path present")
        };
        assert!(pos("/a.leek") < pos("/b.leek"));
        assert!(pos("/b.leek") < pos("/main.leek"));
        // `a` appears exactly once.
        let count = paths
            .iter()
            .filter(|p| p.as_path() == std::path::Path::new("/a.leek"))
            .count();
        assert_eq!(count, 1);
    }
}
