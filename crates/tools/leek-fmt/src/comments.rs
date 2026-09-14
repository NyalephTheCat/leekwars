//! Comment ownership: one pre-pass that decides, for every comment
//! token in a tree, *which node owns it* and *where it sits* relative
//! to that node.
//!
//! The formatter currently rediscovers comment context while walking —
//! each sibling walker inspects the trivia it happens to pass and
//! guesses. That scatters the placement rules across
//! [`crate::format`] and leaves constructs whose walker never looks at
//! trivia falling back to verbatim output. This module computes the
//! same information once, up front, as data: a map from anchor node to
//! the comments attached to it.
//!
//! # Placement rules
//!
//! For each comment token, in order:
//!
//! 1. **Trailing** — the nearest preceding sibling element (skipping
//!    whitespace and other comments) is a node *on the comment's own
//!    source line*. `var x = 1; // why` trails the declaration.
//! 2. **Leading** — otherwise, the nearest following sibling element
//!    (skipping whitespace and other comments) is a node. A comment on
//!    its own line leads whatever comes next.
//! 3. **Dangling** — otherwise the comment has no sibling node to hang
//!    off at all, and belongs to its parent: `{ // todo`, an empty
//!    argument list holding a note, a comment at the end of a file.
//!
//! A preceding *token* (`(`, `{`, `=`) is deliberately not a trailing
//! anchor: it is the parent's own delimiter, so `f(/* k */ a)` leads
//! the first argument rather than trailing the paren.
//!
//! Every comment lands in exactly one bucket — rule 3 has no escape —
//! which is the property the later slices lean on to retire the
//! verbatim fallback. [`Comments::total`] plus the fixture test at the
//! bottom of this file guard it.
//!
//! # Keying
//!
//! Anchors are keyed by [`TextRange`]: rowan's `SyntaxNode` is not
//! `Hash`, and cloning one per entry to compare by identity would cost
//! more than it buys. Two *distinct* nodes can share a range (a
//! single-child wrapper such as `ExprStmt` around `CallExpr` with no
//! semicolon), but never two nodes that both own comments: comments
//! anchor to siblings, and a node with a sibling comment is strictly
//! narrower than the parent that contains both. So a range identifies
//! an anchor unambiguously here.

// The map is built and tested here before anything consumes it: this
// slice adds the data structure, the next one teaches `format` to read
// it. `expect` (not `allow`) so the attribute reports itself as stale
// the moment the whole module has callers; `not(test)` because the
// tests below already exercise every item.
#![cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "pre-pass lands before the formatter reads it (#196)"
    )
)]

use std::collections::HashMap;

use leek_syntax::language::NodeOrToken;
use leek_syntax::{SyntaxKind as S, SyntaxNode, SyntaxToken};
use rowan::TextRange;

use crate::format::count_newlines;

/// Where a comment sits relative to the node that owns it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Placement {
    /// Printed before the node, on its own line(s).
    Leading,
    /// Printed after the node, on the node's last line.
    Trailing,
    /// Owned by the node but attached to none of its children —
    /// printed inside it, between its delimiters.
    Dangling,
}

/// Every comment in one tree, indexed by the node that owns it.
///
/// Build with [`Comments::build`]; query with [`Comments::leading`],
/// [`Comments::trailing`] and [`Comments::dangling`].
#[derive(Debug, Default)]
pub(crate) struct Comments {
    leading: HashMap<TextRange, Vec<SyntaxToken>>,
    trailing: HashMap<TextRange, Vec<SyntaxToken>>,
    dangling: HashMap<TextRange, Vec<SyntaxToken>>,
}

impl Comments {
    /// Map every comment token under `root` to an (anchor, placement)
    /// pair. One walk over the tree; see the module docs for the rules.
    pub(crate) fn build(root: &SyntaxNode) -> Self {
        let mut out = Self::default();
        for comment in root
            .descendants_with_tokens()
            .filter_map(NodeOrToken::into_token)
            .filter(is_comment)
        {
            // Every token yielded here sits under a node, so `parent()`
            // is always `Some`. Skipping would drop a comment from the
            // map; the fixture invariant test would catch it.
            let Some(parent) = comment.parent() else {
                continue;
            };
            let (anchor, placement) = anchor_for(&comment, &parent);
            out.attach(&anchor, placement, comment);
        }
        out
    }

    /// Comments printed before `node`.
    pub(crate) fn leading(&self, node: &SyntaxNode) -> &[SyntaxToken] {
        lookup(&self.leading, node)
    }

    /// Comments printed after `node`, on its last line.
    pub(crate) fn trailing(&self, node: &SyntaxNode) -> &[SyntaxToken] {
        lookup(&self.trailing, node)
    }

    /// Comments owned by `node` but by none of its children.
    pub(crate) fn dangling(&self, node: &SyntaxNode) -> &[SyntaxToken] {
        lookup(&self.dangling, node)
    }

    /// How many comments the map holds, over all anchors and
    /// placements. Equal to the number of comment tokens in the tree —
    /// that is the invariant the later slices depend on.
    pub(crate) fn total(&self) -> usize {
        [&self.leading, &self.trailing, &self.dangling]
            .into_iter()
            .map(|map| map.values().map(Vec::len).sum::<usize>())
            .sum()
    }

    /// Record `comment` against `anchor`. Comments arrive in source
    /// order, so each bucket stays in source order too.
    fn attach(&mut self, anchor: &SyntaxNode, placement: Placement, comment: SyntaxToken) {
        let map = match placement {
            Placement::Leading => &mut self.leading,
            Placement::Trailing => &mut self.trailing,
            Placement::Dangling => &mut self.dangling,
        };
        map.entry(anchor.text_range()).or_default().push(comment);
    }
}

/// Look one node up in a bucket, treating "absent" as "no comments".
fn lookup<'a>(
    map: &'a HashMap<TextRange, Vec<SyntaxToken>>,
    node: &SyntaxNode,
) -> &'a [SyntaxToken] {
    map.get(&node.text_range()).map_or(&[], Vec::as_slice)
}

/// The same line- and block-comment filter
/// [`crate::collect_off_regions`] scans with.
fn is_comment(token: &SyntaxToken) -> bool {
    matches!(token.kind(), S::LineComment | S::BlockComment)
}

/// Decide which node owns `comment` and where it sits. `parent` is the
/// comment's parent node — the fallback anchor.
fn anchor_for(comment: &SyntaxToken, parent: &SyntaxNode) -> (SyntaxNode, Placement) {
    if let Some(node) = same_line_predecessor(comment) {
        return (node, Placement::Trailing);
    }
    if let Some(node) = following_sibling_node(comment) {
        return (node, Placement::Leading);
    }
    (parent.clone(), Placement::Dangling)
}

/// The sibling node the comment trails: the nearest preceding sibling
/// element, skipping whitespace and other comments, provided nothing
/// skipped broke the line and the element itself is a node.
///
/// A preceding token yields `None` — it is the parent's own delimiter
/// or keyword, not something a comment can trail.
fn same_line_predecessor(comment: &SyntaxToken) -> Option<SyntaxNode> {
    let mut cursor = comment.prev_sibling_or_token();
    while let Some(element) = cursor {
        match &element {
            NodeOrToken::Token(t) if t.kind() == S::Whitespace || is_comment(t) => {
                // A line break between the comment and whatever
                // precedes it means the comment starts its own line.
                if count_newlines(t.text()) > 0 {
                    return None;
                }
            }
            NodeOrToken::Node(n) => return Some(n.clone()),
            NodeOrToken::Token(_) => return None,
        }
        cursor = element.prev_sibling_or_token();
    }
    None
}

/// The sibling node the comment leads: the nearest following sibling
/// element, skipping whitespace and other comments. A following token
/// (`}`, `)`) yields `None` — there is nothing after the comment for it
/// to lead, so it dangles in its parent.
fn following_sibling_node(comment: &SyntaxToken) -> Option<SyntaxNode> {
    let mut cursor = comment.next_sibling_or_token();
    while let Some(element) = cursor {
        match &element {
            NodeOrToken::Token(t) if t.kind() == S::Whitespace || is_comment(t) => {}
            NodeOrToken::Node(n) => return Some(n.clone()),
            NodeOrToken::Token(_) => return None,
        }
        cursor = element.next_sibling_or_token();
    }
    None
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use leek_span::SourceId;
    use leek_syntax::language::NodeOrToken;
    use leek_syntax::{SyntaxKind as S, SyntaxNode, SyntaxToken, Version};

    use super::{Comments, is_comment};

    fn parse(src: &str) -> SyntaxNode {
        let parsed = leek_parser::parse_with_features(
            src,
            SourceId::new(1).unwrap(),
            Version::V4,
            leek_parser::ParseFeatures::default(),
        );
        SyntaxNode::new_root(parsed.green)
    }

    /// First node of `kind` in document order.
    fn find(root: &SyntaxNode, kind: S) -> SyntaxNode {
        root.descendants()
            .find(|n| n.kind() == kind)
            .unwrap_or_else(|| panic!("no {kind:?} in tree:\n{root:#?}"))
    }

    /// The comment tokens in the tree, in source order.
    fn comment_texts(root: &SyntaxNode) -> Vec<String> {
        root.descendants_with_tokens()
            .filter_map(NodeOrToken::into_token)
            .filter(is_comment)
            .map(|t| t.text().to_string())
            .collect()
    }

    fn texts(tokens: &[SyntaxToken]) -> Vec<&str> {
        tokens.iter().map(SyntaxToken::text).collect()
    }

    #[test]
    fn own_line_comment_leads_the_next_statement() {
        let root = parse("// lead\nvar x = 1;\n");
        let comments = Comments::build(&root);
        let stmt = find(&root, S::VarDeclStmt);

        assert_eq!(comments.total(), 1);
        assert_eq!(texts(comments.leading(&stmt)), ["// lead"]);
        assert!(comments.trailing(&stmt).is_empty());
        assert!(comments.dangling(&root).is_empty());
    }

    #[test]
    fn end_of_line_comment_trails_the_statement() {
        let root = parse("var x = 1; // why\n");
        let comments = Comments::build(&root);
        let stmt = find(&root, S::VarDeclStmt);

        assert_eq!(comments.total(), 1);
        assert_eq!(texts(comments.trailing(&stmt)), ["// why"]);
        assert!(comments.leading(&stmt).is_empty());
        assert!(comments.dangling(&root).is_empty());
    }

    #[test]
    fn lone_comment_in_a_block_dangles_on_the_block() {
        let root = parse("{ // todo\n}\n");
        let comments = Comments::build(&root);
        let block = find(&root, S::Block);

        assert_eq!(comments.total(), 1);
        assert_eq!(texts(comments.dangling(&block)), ["// todo"]);
        assert!(comments.leading(&block).is_empty());
        assert!(comments.trailing(&block).is_empty());
    }

    #[test]
    fn argument_comment_stays_inside_the_call() {
        let root = parse("f(a /* k */, b);\n");
        let comments = Comments::build(&root);
        let args = find(&root, S::ArgList);
        let first_arg = args
            .children()
            .next()
            .expect("argument list has a first argument");

        assert_eq!(comments.total(), 1);
        // It trails the argument it follows — not the file, and not
        // the statement.
        assert_eq!(texts(comments.trailing(&first_arg)), ["/* k */"]);
        for bucket in [
            comments.leading(&root),
            comments.trailing(&root),
            comments.dangling(&root),
        ] {
            assert!(bucket.is_empty(), "comment escaped to SourceFile");
        }
    }

    #[test]
    fn comment_before_the_first_argument_leads_it() {
        let root = parse("f(/* k */ a);\n");
        let comments = Comments::build(&root);
        let args = find(&root, S::ArgList);
        let first_arg = args
            .children()
            .next()
            .expect("argument list has a first argument");

        assert_eq!(comments.total(), 1);
        assert_eq!(texts(comments.leading(&first_arg)), ["/* k */"]);
    }

    #[test]
    fn consecutive_own_line_comments_all_lead_the_same_node() {
        let root = parse("// one\n// two\nvar x = 1;\n");
        let comments = Comments::build(&root);
        let stmt = find(&root, S::VarDeclStmt);

        assert_eq!(comments.total(), 2);
        assert_eq!(texts(comments.leading(&stmt)), ["// one", "// two"]);
    }

    #[test]
    fn trailing_comment_is_not_stolen_by_the_next_statement() {
        let root = parse("var x = 1; // why\nvar y = 2;\n");
        let comments = Comments::build(&root);
        let mut stmts = root.children().filter(|n| n.kind() == S::VarDeclStmt);
        let first = stmts.next().expect("first declaration");
        let second = stmts.next().expect("second declaration");

        assert_eq!(comments.total(), 1);
        assert_eq!(texts(comments.trailing(&first)), ["// why"]);
        assert!(comments.leading(&second).is_empty());
    }

    #[test]
    fn comment_after_the_last_statement_dangles_on_its_parent() {
        let root = parse("var x = 1;\n// tail\n");
        let comments = Comments::build(&root);

        assert_eq!(comments.total(), 1);
        assert_eq!(texts(comments.dangling(&root)), ["// tail"]);
    }

    /// The invariant the later slices lean on: every comment token in
    /// the tree gets exactly one anchor, for every fixture the
    /// formatter ships.
    #[test]
    fn every_fixture_comment_has_exactly_one_anchor() {
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
        let mut files = 0;
        let mut anchored = 0;
        for entry in std::fs::read_dir(&dir).expect("read fixtures dir") {
            let path = entry.expect("fixture dir entry").path();
            let name = path.file_name().unwrap_or_default().to_string_lossy();
            // Both halves of every pair: the inputs are the corpus the
            // slice was asked for, and the formatted outputs are what
            // the *next* format pass will see.
            if !name.ends_with(".leek") {
                continue;
            }
            let src = std::fs::read_to_string(&path).expect("read fixture");
            let root = parse(&src);
            let expected = comment_texts(&root);
            let comments = Comments::build(&root);
            assert_eq!(
                comments.total(),
                expected.len(),
                "{name}: {} comment tokens in the tree, {} anchored ({expected:?})",
                expected.len(),
                comments.total(),
            );
            files += 1;
            anchored += comments.total();
        }
        assert!(
            files >= 20,
            "expected the whole fixture corpus, found {files} files"
        );
        // Guard against a vacuous pass: the corpus really does carry
        // comments, so `total()` is being compared against a non-zero
        // count somewhere.
        assert!(
            anchored >= 10,
            "fixture corpus anchored only {anchored} comments"
        );
    }
}
