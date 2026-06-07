//! `@allow(LXXXX)` annotation parsing + lint suppression.
//!
//! Users can suppress an individual lint finding by annotating
//! the *immediately following* statement / declaration with a
//! line- or block-comment of the shape:
//!
//! ```text
//! // @allow(L0001)
//! var unused = 5
//! ```
//!
//! Multiple codes may be combined comma-separated:
//! `// @allow(L0001, L0005)`. The special name `all`
//! (`// @allow(all)`) suppresses *every* lint finding (`L`-range
//! codes) on the annotated statement, while leaving parse and type
//! errors untouched. The annotation applies to the
//! statement it directly precedes — once another statement runs
//! between the annotation and a finding, the suppression no
//! longer covers it. Inside a block, the annotation suppresses
//! findings whose span lands inside that statement's text
//! range, so a finding deep inside the annotated statement's
//! sub-expressions is still suppressed.
//!
//! Run [`collect_allows`] over a CST `SyntaxNode`. The returned
//! [`AllowMap`] then filters lint output via
//! [`AllowMap::suppress`].

use std::collections::HashSet;
use std::ops::Range;

use leek_diagnostics::{Code, Diagnostic};
use leek_syntax::language::NodeOrToken;
use leek_syntax::{SyntaxKind, SyntaxNode};

/// One annotated region: the byte range of the statement it
/// covers, and the set of diagnostic codes the annotation allows.
#[derive(Debug, Clone)]
pub struct AllowRegion {
    pub range: Range<u32>,
    pub codes: HashSet<String>,
}

/// All `@allow` annotations found in a file.
#[derive(Debug, Clone, Default)]
pub struct AllowMap {
    pub regions: Vec<AllowRegion>,
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
            if r.range.contains(&start)
                && (r.codes.contains(id)
                    || name.is_some_and(|n| r.codes.contains(n))
                    || (is_lint && r.codes.contains("all")))
            {
                return true;
            }
        }
        false
    }
}

/// Map a diagnostic code id (`"L0014"`) to its lint rule's name
/// (`"identical-operands"`), built once from [`crate::default_rules`].
/// Returns `None` for non-lint codes (parse/type errors, which have no
/// rule name).
fn code_to_rule_name(id: &str) -> Option<&'static str> {
    use std::collections::HashMap;
    use std::sync::OnceLock;
    static MAP: OnceLock<HashMap<&'static str, &'static str>> = OnceLock::new();
    let map = MAP.get_or_init(|| {
        crate::default_rules()
            .iter()
            .map(|r| (r.code().id(), r.name()))
            .collect()
    });
    map.get(id).copied()
}

/// Walk `root` looking for `@allow(…)` line/block comments. Each
/// such comment claims the next sibling node as its target; the
/// region spans that node's text range.
pub fn collect_allows(root: &SyntaxNode) -> AllowMap {
    let mut regions = Vec::new();
    walk(root, &mut regions);
    AllowMap { regions }
}

fn walk(node: &SyntaxNode, regions: &mut Vec<AllowRegion>) {
    // For each child position in this node, look for an annotation
    // comment immediately preceding a child node. The matching is
    // local — only siblings of the annotated comment can be
    // claimed. Recurse into nodes for nested coverage.
    let mut pending: Option<HashSet<String>> = None;
    for el in node.children_with_tokens() {
        match el {
            NodeOrToken::Token(t) => match t.kind() {
                SyntaxKind::LineComment | SyntaxKind::BlockComment => {
                    if let Some(set) = parse_allow_annotation(t.text()) {
                        // Merge with any prior `@allow` on the same
                        // run (`@allow(L0001)` then `@allow(L0005)`
                        // each get folded into the next stmt).
                        pending.get_or_insert_with(HashSet::new).extend(set);
                    }
                }
                SyntaxKind::Whitespace => {}
                _ => {
                    // Any other token resets the pending block —
                    // an `@allow` only attaches to a CST *node*.
                    pending = None;
                }
            },
            NodeOrToken::Node(n) => {
                if let Some(codes) = pending.take() {
                    let r = n.text_range();
                    regions.push(AllowRegion {
                        range: u32::from(r.start())..u32::from(r.end()),
                        codes,
                    });
                }
                walk(&n, regions);
            }
        }
    }
}

/// Parse `// @allow(L0001, L0005)` (line) or
/// `/* @allow(L0001) */` (block). Returns the set of code names
/// inside the parentheses or `None` if the comment isn't an
/// `@allow` annotation.
pub fn parse_allow_annotation(raw: &str) -> Option<HashSet<String>> {
    // Strip comment delimiters.
    let body = if let Some(rest) = raw.strip_prefix("//") {
        rest.trim_end()
    } else if let Some(rest) = raw.strip_prefix("/*") {
        rest.strip_suffix("*/").unwrap_or(rest).trim()
    } else {
        return None;
    };
    let body = body.trim();
    // Accept `@allow(...)`, also tolerant of `@lint:allow(...)` /
    // `@suppress(...)` synonyms for editor-conventions parity.
    let rest = body
        .strip_prefix("@allow")
        .or_else(|| body.strip_prefix("@suppress"))?
        .trim_start();
    let inside = rest.strip_prefix('(')?.strip_suffix(')')?;
    let mut codes = HashSet::new();
    for code in inside.split(',') {
        let c = code.trim();
        if !c.is_empty() {
            codes.insert(c.to_string());
        }
    }
    if codes.is_empty() { None } else { Some(codes) }
}

/// `Code` accessor wrapper — kept in this module to avoid leaking
/// the public `Code::id` shape elsewhere. (Diagnostics' `Code` is
/// already exported, so this is more about narrowing the import
/// surface for the allow logic.)
#[allow(dead_code)]
fn code_id(code: Code) -> &'static str {
    code.id()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_single_code() {
        let codes = parse_allow_annotation("// @allow(L0001)").unwrap();
        assert!(codes.contains("L0001"));
    }

    #[test]
    fn parses_multiple_codes() {
        let codes = parse_allow_annotation("// @allow(L0001, L0005)").unwrap();
        assert_eq!(codes.len(), 2);
        assert!(codes.contains("L0001"));
        assert!(codes.contains("L0005"));
    }

    #[test]
    fn parses_block_comment_form() {
        let codes = parse_allow_annotation("/* @allow(L0006) */").unwrap();
        assert!(codes.contains("L0006"));
    }

    #[test]
    fn accepts_suppress_synonym() {
        let codes = parse_allow_annotation("// @suppress(L0003)").unwrap();
        assert!(codes.contains("L0003"));
    }

    #[test]
    fn rejects_non_allow() {
        assert!(parse_allow_annotation("// just a comment").is_none());
        assert!(parse_allow_annotation("//@deny(L0001)").is_none());
    }
}
