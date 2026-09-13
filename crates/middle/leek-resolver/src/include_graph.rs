//! Build the include dependency graph for an entry source file.
//!
//! Walks `include("name")` statements transitively using a
//! [`Folder`] for I/O. Stops on cycles (reported as
//! `CircularInclude`) and missing files (reported as
//! `AI_NOT_EXISTING`). Returns a topologically-ordered list of
//! `LoadedFile`s — entry last — so callers can pre-declare items
//! from leaves first and the entry inherits everything.
//!
//! ### Version-aware lexing
//!
//! Each file's `include(...)` calls must be extracted with that
//! file's own `@version` pragma applied. Otherwise a v2 file using
//! `and` as a keyword gets tokenized at v4 (where `and` is an
//! identifier) and the cached token stream is wrong for the next
//! real compile pass. See `doc/pipeline.md` §5.1.7. We honor this
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

use leek_diagnostics::{Diagnostic, IntoDiagnostic, codes, diag};
use leek_lexer::lex;
use leek_span::{SourceId, Span};
use leek_syntax::{SyntaxKind, Version, parse_pragmas};

use crate::folder::{Folder, IncludeError, LoadedFile, canonical_or_normalized};

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
#[derive(Debug, Clone)]
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
    pub text: String,
    pub version: Version,
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
            text: entry_text.to_string(),
            version: entry_version,
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
    ) {
        if done.contains(&current) {
            return;
        }
        if visiting.contains(&current) {
            // Cycle: report against the includer site. We can't
            // recover a precise span here (the include call is on
            // the previous frame); attach a zero-span diagnostic
            // anchored on the file's source id.
            let file = files.get(&current).cloned();
            if let Some(file) = file {
                diagnostics.push(diag!(
                    codes::CIRCULAR_INCLUDE,
                    Span::new(file.source, 0, 0),
                    "circular include involving `{}`",
                    current.display(),
                ));
            }
            return;
        }
        visiting.push(current.clone());

        // Extract include names + their spans from this file.
        let file = files
            .get(&current)
            .cloned()
            .expect("file already inserted before walk");
        let includes = extract_include_calls(&file);

        for inc in &includes {
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

/// One `include(...)` call extracted from a file.
struct IncludeCall {
    name: String,
    span: Span,
}

/// Token-level scan for `include("…")`. Uses the file's own
/// `@version` pragma so v2's `and`-keyword case doesn't get
/// mis-lexed. Returns the include names plus their string-literal
/// spans for diagnostic reporting.
fn extract_include_calls(file: &ResolvedFile) -> Vec<IncludeCall> {
    let mut out: Vec<IncludeCall> = Vec::new();
    let tokens = lex(&file.text, file.source, file.version);
    // Walk a small state machine: KwInclude, LParen, StringLiteral,
    // optional RParen. Whitespace + comments are skipped via
    // `is_trivia`.
    let mut iter = tokens.tokens.iter().filter(|t| !t.kind.is_trivia());
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
            let raw = &file.text[s.span.start as usize..s.span.end as usize];
            if raw.len() >= 2 {
                let name = raw[1..raw.len() - 1].to_string();
                out.push(IncludeCall { name, span: s.span });
            }
        }
    }
    out
}

/// An included file's version: its own explicit `@version` pragma,
/// else the entry's settled version.
fn included_version(text: &str, entry_version: Version) -> Version {
    // `parse_pragmas` wants a SourceId for its diagnostics; the
    // included file's own pragma diagnostics are not reported here.
    let (pragmas, _diags) = parse_pragmas(text, SourceId::new(1).unwrap());
    pragmas.effective_version(entry_version)
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
