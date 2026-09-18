//! `@allow(LXXXX)` annotation parsing + lint suppression.
//!
//! Users can suppress an individual lint finding by annotating a
//! statement / declaration with a line- or block-comment of the shape:
//!
//! ```text
//! // @allow(L0001)
//! var unused = 5
//! ```
//!
//! An annotation claims exactly one statement, and which one depends on
//! where the comment sits:
//!
//! - **Leading** — a comment that starts its own line claims the
//!   statement *below* it (the example above).
//! - **Trailing** — a comment on the same line as, and after, a
//!   statement claims *that* statement:
//!   `var unused = 5 // @allow(L0001)`.
//!
//! The two are told apart by whether a newline separates the previous
//! sibling node from the comment, so a comment that is the first thing
//! on its line always looks forward and a trailing one never reaches
//! past the statement it trails.
//!
//! Multiple codes may be combined comma-separated:
//! `// @allow(L0001, L0005)`. The special name `all`
//! (`// @allow(all)`) suppresses *every* lint finding (`L`-range
//! codes) on the annotated statement, while leaving parse and type
//! errors untouched. Once another statement runs between the
//! annotation and a finding, the suppression no longer covers it.
//! Inside a block, the annotation suppresses findings whose span lands
//! inside that statement's text range, so a finding deep inside the
//! annotated statement's sub-expressions is still suppressed.
//!
//! ## Spellings
//!
//! `@suppress(…)` is an accepted synonym, and either keyword may carry
//! a `@lint:` prefix (`@lint:allow(…)`, `@lint:suppress(…)`) for parity
//! with editor conventions.
//!
//! A `-file` suffix widens an annotation to the whole file:
//!
//! ```text
//! // @allow-file(unused-variable)
//! ```
//!
//! It belongs in the file's leading comment band by convention, but it
//! is file-scoped wherever it is written — a file-level annotation that
//! only counted in one place would be the same silent no-op this module
//! otherwise goes out of its way to report.
//!
//! Anything after the closing parenthesis is free text, so the reason
//! can sit beside the annotation: `// @allow(L0001) kept on purpose`.
//!
//! ## Names that resolve to nothing
//!
//! A name is accepted when it is a registered lint's code (`L0001`),
//! that lint's kebab-case name (`unused-variable`), or `all`. Anything
//! else — a typo, a code that is not a lint — can never suppress a
//! finding, so it is reported as [`codes::UNKNOWN_LINT_IN_ALLOW`] with
//! a "did you mean" hint instead of being silently ignored.
//!
//! Run [`collect_allows`] over a CST `SyntaxNode`. The returned
//! [`AllowMap`] then filters lint output via [`AllowMap::suppress`],
//! and carries the annotations' own diagnostics in
//! [`AllowMap::diagnostics`].

use std::collections::HashSet;
use std::ops::Range;

use leek_diagnostics::{Diagnostic, codes, diag, suggest_similar};
use leek_span::{SourceId, Span};
use leek_syntax::language::NodeOrToken;
use leek_syntax::{SyntaxKind, SyntaxNode};

/// The catch-all name: every `L`-range finding in the region.
const ALL: &str = "all";

/// One annotated region: the byte range of the statement it
/// covers, and the set of diagnostic codes the annotation allows.
#[derive(Debug, Clone)]
pub struct AllowRegion {
    /// The file the range indexes. Byte offsets alone don't identify a
    /// location: a program's findings come from its whole include
    /// closure, and without this a region would suppress whatever
    /// happened to sit at the same offset in another file.
    pub source: SourceId,
    pub range: Range<u32>,
    pub codes: HashSet<String>,
}

/// All `@allow` annotations found in a file.
#[derive(Debug, Clone, Default)]
pub struct AllowMap {
    pub regions: Vec<AllowRegion>,
    /// Findings about the annotations themselves — names that resolve
    /// to no known lint. Emitted alongside the lint findings the map
    /// filters; see [`crate::lint_file`].
    pub diagnostics: Vec<Diagnostic>,
}

impl AllowMap {
    /// Drop every diagnostic whose code matches an annotation
    /// covering its span. Returns a new vector preserving order.
    pub fn suppress(&self, diags: Vec<Diagnostic>) -> Vec<Diagnostic> {
        diags.into_iter().filter(|d| !self.is_allowed(d)).collect()
    }

    fn is_allowed(&self, d: &Diagnostic) -> bool {
        let id = d.code.id();
        // Allow suppressing by either the numeric code (`L0001`) or the
        // rule's kebab-case name (`unused-variable`), whichever the user
        // wrote — names are friendlier and don't require memorizing codes.
        let name = code_to_rule_name(id);
        // `@allow(all)` is a catch-all that suppresses every *lint*
        // finding (codes in the `L` range) on the annotated statement,
        // without silencing parse/type errors.
        let is_lint = id.starts_with('L');
        let start = d.span.start;
        for r in &self.regions {
            if r.source == d.span.source
                && r.range.contains(&start)
                && (r.codes.contains(id)
                    || name.is_some_and(|n| r.codes.contains(n))
                    || (is_lint && r.codes.contains(ALL)))
            {
                return true;
            }
        }
        false
    }
}

/// Map a diagnostic code id (`"L0014"`) to its lint rule's name
/// (`"identical-operands"`), built once from [`crate::rules::REGISTRY`] —
/// static metadata, so the lookup never builds a pass. Returns `None` for
/// non-lint codes (parse/type errors, which have no rule name).
pub fn code_to_rule_name(id: &str) -> Option<&'static str> {
    use std::collections::HashMap;
    use std::sync::OnceLock;
    static MAP: OnceLock<HashMap<&'static str, &'static str>> = OnceLock::new();
    let map = MAP.get_or_init(|| {
        crate::rules::REGISTRY
            .iter()
            .map(|r| (r.meta.code.id(), r.meta.name))
            .collect()
    });
    map.get(id).copied()
}

/// Every name an annotation may contain: each registered lint's code
/// and kebab name, plus the [`ALL`] catch-all. The suggestion pool for
/// a name that resolves to none of them.
fn known_allow_names() -> impl Iterator<Item = &'static str> {
    crate::rules::REGISTRY
        .iter()
        .flat_map(|r| [r.meta.code.id(), r.meta.name])
        .chain(std::iter::once(ALL))
}

/// Whether `name` can ever suppress anything.
fn is_known_allow_name(name: &str) -> bool {
    known_allow_names().any(|k| k == name)
}

/// Walk `root` looking for `@allow(…)` line/block comments, reporting
/// the names among them that resolve to no lint.
///
/// A leading annotation claims the next sibling node; a trailing one
/// claims the sibling node it shares a line with; an `@allow-file(…)`
/// claims the whole file. The region spans the claimed node's text
/// range.
///
/// `source` is the file the tree was parsed from — the annotations'
/// own diagnostics point into it, and a green tree carries no
/// [`SourceId`] of its own.
#[must_use]
pub fn collect_allows(root: &SyntaxNode, source: SourceId) -> AllowMap {
    let mut out = Collected {
        source,
        map: AllowMap::default(),
        file_codes: HashSet::new(),
    };
    walk(root, &mut out);
    if !out.file_codes.is_empty() {
        let r = root.text_range();
        let codes = std::mem::take(&mut out.file_codes);
        out.push(u32::from(r.start())..u32::from(r.end()), codes);
    }
    out.map
}

/// The walk's accumulator: the map being built, the file-scoped codes
/// seen so far (folded into one whole-file region at the end), and the
/// source the annotation diagnostics point into.
struct Collected {
    source: SourceId,
    map: AllowMap,
    file_codes: HashSet<String>,
}

impl Collected {
    /// Record one annotated region, in the file being walked.
    fn push(&mut self, range: Range<u32>, codes: HashSet<String>) {
        self.map.regions.push(AllowRegion {
            source: self.source,
            range,
            codes,
        });
    }

    /// Validate one annotation's names against the registry, reporting
    /// the ones that resolve to nothing and returning the rest.
    ///
    /// `at` is the comment token's start offset, which the names'
    /// in-comment offsets are relative to — so the caret lands on the
    /// misspelled name, not on the whole comment.
    fn accept(&mut self, ann: &AllowAnnotation, at: u32) -> HashSet<String> {
        let mut codes = HashSet::new();
        for name in &ann.names {
            if is_known_allow_name(&name.text) {
                codes.insert(name.text.clone());
                continue;
            }
            let len = u32::try_from(name.text.len()).unwrap_or(0);
            let start = at + name.offset;
            let span = Span::new(self.source, start, start + len);
            let text = &name.text;
            let mut d = diag!(
                codes::UNKNOWN_LINT_IN_ALLOW,
                span,
                "unknown lint `{text}` — this annotation suppresses nothing"
            );
            if let Some(hint) = suggest_similar(text, known_allow_names()) {
                d = d.with_note(hint);
            }
            self.map.diagnostics.push(d);
        }
        codes
    }
}

fn walk(node: &SyntaxNode, out: &mut Collected) {
    // For each child position in this node, look for an annotation
    // comment next to a child node. The matching is local — only
    // siblings of the annotated comment can be claimed. Recurse into
    // nodes for nested coverage.
    let mut pending: Option<HashSet<String>> = None;
    // The previous sibling node, and whether the walk is still on the
    // line it ended on. Together they decide whether an annotation
    // trails that node or leads the next one.
    let mut prev: Option<Range<u32>> = None;
    let mut same_line = false;
    for el in node.children_with_tokens() {
        match el {
            NodeOrToken::Token(t) => {
                if matches!(t.kind(), SyntaxKind::LineComment | SyntaxKind::BlockComment)
                    && let Some(ann) = parse_allow_annotation(t.text())
                {
                    let at = u32::from(t.text_range().start());
                    let codes = out.accept(&ann, at);
                    match ann.scope {
                        AllowScope::File => out.file_codes.extend(codes),
                        AllowScope::Statement => match prev.clone() {
                            // Trailing: `var x = 1 // @allow(L0001)`
                            // annotates the statement it sits behind.
                            Some(range) if same_line => out.push(range, codes),
                            // Leading: merge with any prior `@allow` in
                            // the same run (`@allow(L0001)` then
                            // `@allow(L0005)` both fold into the next
                            // statement).
                            _ => pending.get_or_insert_with(HashSet::new).extend(codes),
                        },
                    }
                }
                if t.text().contains('\n') {
                    same_line = false;
                }
                if !t.kind().is_trivia() {
                    // Any non-trivia token resets the pending block —
                    // an `@allow` only attaches to a CST *node*.
                    pending = None;
                }
            }
            NodeOrToken::Node(n) => {
                let r = n.text_range();
                let range = u32::from(r.start())..u32::from(r.end());
                if let Some(codes) = pending.take() {
                    out.push(range.clone(), codes);
                }
                walk(&n, out);
                prev = Some(range);
                // A node whose last token carries a newline leaves the
                // walk on a fresh line, so a comment after it leads
                // rather than trails.
                same_line = n.last_token().is_none_or(|t| !t.text().contains('\n'));
            }
        }
    }
}

/// How far an annotation reaches.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AllowScope {
    /// The statement the comment leads or trails.
    Statement,
    /// The whole file (`@allow-file(…)`).
    File,
}

/// One name inside an annotation's parentheses, with its byte offset
/// within the comment text so a diagnostic can point at the name
/// itself rather than at the whole comment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AllowName {
    pub text: String,
    pub offset: u32,
}

/// A parsed `@allow(…)` comment: what it covers and what it names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AllowAnnotation {
    pub scope: AllowScope,
    pub names: Vec<AllowName>,
}

/// Parse `// @allow(L0001, L0005)` (line) or
/// `/* @allow(L0001) */` (block), including the `@suppress`,
/// `@lint:allow` and `@allow-file` spellings. Returns `None` if the
/// comment isn't an annotation at all.
///
/// Names are returned as written — resolving them against the registry
/// is [`collect_allows`]'s job, since that is where a name that
/// resolves to nothing can be reported with a span.
#[must_use]
pub fn parse_allow_annotation(raw: &str) -> Option<AllowAnnotation> {
    // Strip comment delimiters, tracking the offset into `raw` so the
    // names' offsets stay comment-relative.
    let (mut at, end) = if raw.starts_with("//") {
        (2, raw.len())
    } else if raw.starts_with("/*") {
        let end = raw.strip_suffix("*/").map_or(raw.len(), str::len);
        (2, end)
    } else {
        return None;
    };
    let body = raw.get(at..end)?;
    at += body.len() - body.trim_start().len();
    let rest = raw.get(at..end)?.trim_end();

    // `@allow`, `@suppress`, either under an optional `@lint:` prefix,
    // and either widened to the file by a `-file` suffix. The `-file`
    // spellings are tried first: `allow-file` starts with `allow`, so
    // the narrower match would eat the keyword and leave `-file(…)`,
    // which parses as nothing at all.
    let rest = rest.strip_prefix('@')?;
    at += 1;
    let rest = match rest.strip_prefix("lint:") {
        Some(r) => {
            at += "lint:".len();
            r
        }
        None => rest,
    };
    let mut scope = AllowScope::Statement;
    let rest = ["allow-file", "suppress-file", "allow", "suppress"]
        .into_iter()
        .find_map(|kw| {
            let r = rest.strip_prefix(kw)?;
            at += kw.len();
            if kw.ends_with("-file") {
                scope = AllowScope::File;
            }
            Some(r)
        })?;

    // Up to the closing parenthesis, not to the end of the comment:
    // `// @allow(L0001) kept on purpose` is an annotation with a reason
    // written after it, and dropping it on the floor would be one more
    // silent no-op.
    let trimmed = rest.trim_start();
    at += rest.len() - trimmed.len();
    let after_paren = trimmed.strip_prefix('(')?;
    at += 1;
    let inside = &after_paren[..after_paren.find(')')?];

    // Split on commas, carrying each name's offset from the start of
    // the comment.
    let mut names = Vec::new();
    let mut pos = 0usize;
    for part in inside.split(',') {
        let lead = part.len() - part.trim_start().len();
        let text = part.trim();
        if !text.is_empty() {
            names.push(AllowName {
                text: text.to_string(),
                offset: u32::try_from(at + pos + lead).unwrap_or(0),
            });
        }
        pos += part.len() + 1; // the comma
    }
    if names.is_empty() {
        None
    } else {
        Some(AllowAnnotation { scope, names })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(raw: &str) -> Vec<String> {
        parse_allow_annotation(raw)
            .expect("annotation")
            .names
            .into_iter()
            .map(|n| n.text)
            .collect()
    }

    #[test]
    fn parses_single_code() {
        assert_eq!(names("// @allow(L0001)"), ["L0001"]);
    }

    #[test]
    fn parses_multiple_codes() {
        assert_eq!(names("// @allow(L0001, L0005)"), ["L0001", "L0005"]);
    }

    #[test]
    fn parses_block_comment_form() {
        assert_eq!(names("/* @allow(L0006) */"), ["L0006"]);
    }

    #[test]
    fn accepts_suppress_synonym() {
        assert_eq!(names("// @suppress(L0003)"), ["L0003"]);
    }

    #[test]
    fn accepts_lint_prefixed_synonyms() {
        // The `@lint:` prefix the module docs have always advertised.
        assert_eq!(names("// @lint:allow(L0003)"), ["L0003"]);
        assert_eq!(names("// @lint:suppress(L0003)"), ["L0003"]);
        assert_eq!(names("/* @lint:allow(L0003) */"), ["L0003"]);
    }

    #[test]
    fn rejects_non_allow() {
        assert!(parse_allow_annotation("// just a comment").is_none());
        assert!(parse_allow_annotation("//@deny(L0001)").is_none());
        assert!(parse_allow_annotation("// @allow").is_none());
        assert!(parse_allow_annotation("// @allow()").is_none());
        assert!(parse_allow_annotation("// @allow(L0001").is_none());
        assert!(parse_allow_annotation("// @lint:deny(L0001)").is_none());
    }

    #[test]
    fn keeps_a_reason_written_after_the_annotation() {
        assert_eq!(names("// @allow(L0001) kept on purpose"), ["L0001"]);
        assert_eq!(names("/* @suppress(L0001) — on purpose */"), ["L0001"]);
    }

    #[test]
    fn scope_defaults_to_the_statement() {
        let ann = parse_allow_annotation("// @allow(L0001)").unwrap();
        assert_eq!(ann.scope, AllowScope::Statement);
    }

    #[test]
    fn file_suffix_widens_the_scope() {
        for raw in [
            "// @allow-file(L0001)",
            "// @suppress-file(L0001)",
            "// @lint:allow-file(L0001)",
            "/* @allow-file(L0001) */",
        ] {
            let ann = parse_allow_annotation(raw).unwrap_or_else(|| panic!("{raw} should parse"));
            assert_eq!(ann.scope, AllowScope::File, "{raw}");
            assert_eq!(ann.names[0].text, "L0001", "{raw}");
        }
    }

    #[test]
    fn name_offsets_point_at_the_name() {
        // The offsets are what a diagnostic's caret is built from, so
        // they must index the *comment* text exactly.
        let raw = "// @allow(L0001, unused-variable)";
        let ann = parse_allow_annotation(raw).unwrap();
        for name in &ann.names {
            let at = name.offset as usize;
            assert_eq!(&raw[at..at + name.text.len()], name.text, "{name:?}");
        }
    }

    #[test]
    fn block_comment_name_offsets_point_at_the_name() {
        let raw = "/*   @lint:allow-file( L0001 , all ) */";
        let ann = parse_allow_annotation(raw).unwrap();
        assert_eq!(ann.scope, AllowScope::File);
        for name in &ann.names {
            let at = name.offset as usize;
            assert_eq!(&raw[at..at + name.text.len()], name.text, "{name:?}");
        }
    }

    #[test]
    fn known_names_cover_codes_names_and_the_catch_all() {
        assert!(is_known_allow_name("L0001"));
        assert!(is_known_allow_name("unused-variable"));
        assert!(is_known_allow_name(ALL));
        assert!(!is_known_allow_name("unused-varible"));
        // A real catalog code that is not a lint cannot be allowed.
        assert!(!is_known_allow_name("E0200"));
    }
}
