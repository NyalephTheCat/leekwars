//! Block-shaped constructs: `SourceFile` (top level) and `Block`
//! (`{ … }` statement). Both share the "indented list of items
//! separated by hardlines, with blank-line preservation between
//! items" pattern.

use leek_syntax::language::NodeOrToken;
use leek_syntax::{SyntaxKind as S, SyntaxNode, SyntaxToken};

use crate::doc::{Doc, blank_line, concat, hardline, indent, text};

use super::{
    apply_pragma_to_ctx, comment_doc, count_newlines, fmt_node, fmt_node_with_next_overrides,
    is_doc_comment, is_trivia, with_ctx,
};

/// Format the top-level item list inside a `SourceFile`.
pub(super) fn format_top_level(root: &SyntaxNode) -> Doc {
    debug_assert_eq!(root.kind(), S::SourceFile);
    format_item_sequence(root, /* allow_blanks = */ true)
}

/// Format a `{ … }` block: open brace + indented stmt list +
/// close brace.
pub(super) fn format_block(node: &SyntaxNode) -> Doc {
    debug_assert_eq!(node.kind(), S::Block);

    let inner_root = match find_block_inner(node) {
        Some(()) => node,
        None => return text("{}"),
    };
    let body_doc = format_item_sequence_bounded(inner_root, true, true);
    if matches!(body_doc, Doc::Nil) {
        return text("{}");
    }
    concat([
        text("{"),
        indent(1, concat([hardline(), body_doc])),
        hardline(),
        text("}"),
    ])
}

/// Is there at least one significant child *inside* the `{ }` of
/// `node`? Used to short-circuit empty blocks to `{}`.
fn find_block_inner(node: &SyntaxNode) -> Option<()> {
    let mut in_body = false;
    for el in node.children_with_tokens() {
        match el {
            NodeOrToken::Token(t) if t.kind() == S::LBrace => in_body = true,
            NodeOrToken::Token(t) if t.kind() == S::RBrace => return None,
            NodeOrToken::Token(t) if in_body && (is_trivia(&t) || t.kind() == S::Semicolon) => {
                // Bare semicolons inside an otherwise empty block
                // don't count as "has content" for the `{}` shortcut.
            }
            NodeOrToken::Token(_) | NodeOrToken::Node(_) if in_body => return Some(()),
            _ => {}
        }
    }
    None
}

/// Same as [`format_item_sequence`] but skips the `{` / `}` brace
/// tokens (used by `Block`, which adds its own braces).
fn format_item_sequence_bounded(node: &SyntaxNode, allow_blanks: bool, skip_braces: bool) -> Doc {
    let mut items: Vec<Doc> = Vec::new();
    let mut leading: Vec<(Doc, usize, bool)> = Vec::new(); // (doc, newlines_before, is_doc)
    let mut pending: usize = 0;
    let mut saw_first = false;
    let mut started = !skip_braces;
    // `// fmt: next <key> = <value>` pragmas queue here; the next
    // item formatted consumes them as one-shot push/pop overrides.
    let mut pending_next: Vec<(String, String)> = Vec::new();

    for el in node.children_with_tokens() {
        if let NodeOrToken::Token(t) = &el {
            if skip_braces && t.kind() == S::LBrace {
                started = true;
                continue;
            }
            if skip_braces && t.kind() == S::RBrace {
                break;
            }
            if !started {
                continue;
            }
        }
        match el {
            NodeOrToken::Token(t) if t.kind() == S::Whitespace => {
                pending += count_newlines(t.text());
            }
            NodeOrToken::Token(t) if is_trivia(&t) => {
                // `// fmt: …` pragma comments mutate state but
                // shouldn't appear in the output. The newline-budget
                // (`pending`) for the next real item carries through
                // unchanged so blank-line preservation still works
                // even with pragmas in between.
                let pragma = crate::parse_fmt_pragma(t.text());
                if let crate::FmtPragma::Next(k, v) = &pragma {
                    pending_next.push((k.clone(), v.clone()));
                    continue;
                }
                if pragma != crate::FmtPragma::None {
                    apply_pragma_to_ctx(&pragma);
                    continue;
                }
                let is_doc = is_doc_comment(t.text());
                leading.push((comment_doc(&t), pending, is_doc));
                pending = 0;
            }
            NodeOrToken::Token(t) => {
                emit_item(
                    &mut items,
                    &mut leading,
                    pending,
                    saw_first,
                    allow_blanks,
                    token_doc(&t),
                );
                // Stray significant tokens don't consume `next`
                // overrides — those wait for a real node child.
                saw_first = true;
                pending = 0;
            }
            NodeOrToken::Node(child) => {
                let item_doc = if pending_next.is_empty() {
                    fmt_node(&child)
                } else {
                    let d = fmt_node_with_next_overrides(&child, &pending_next);
                    pending_next.clear();
                    d
                };
                // Capture the *current* opts so any `// fmt: push
                // indent = …` that fired earlier in this sibling
                // walker reaches the printer when this item renders.
                let item_doc = super::wrap_with_active_opts(item_doc);
                emit_item(
                    &mut items,
                    &mut leading,
                    pending,
                    saw_first,
                    allow_blanks,
                    item_doc,
                );
                saw_first = true;
                pending = 0;
            }
        }
    }

    // Drain any trailing-only comments (after the last item but
    // before EOF / `}`).
    if !leading.is_empty() {
        emit_trailing_comments(&mut items, &mut leading, pending, saw_first, allow_blanks);
    }

    if items.is_empty() {
        return Doc::Nil;
    }
    concat(items)
}

/// Format a series of items separated by hardlines (or blank lines
/// when `allow_blanks` is true and source had a blank between them).
fn format_item_sequence(node: &SyntaxNode, allow_blanks: bool) -> Doc {
    format_item_sequence_bounded(node, allow_blanks, false)
}

/// Push one item, preceded by any pending leading comments and the
/// appropriate separator. Mutates `leading` (drained).
fn emit_item(
    items: &mut Vec<Doc>,
    leading: &mut Vec<(Doc, usize, bool)>,
    newlines_before_item: usize,
    saw_first: bool,
    allow_blanks: bool,
    item_doc: Doc,
) {
    // If the immediately-preceding leading entry is a doc comment,
    // suppress any blank line between it and the item. Doxygen /
    // rustdoc convention: doc comment sticks to the following decl.
    let mut effective_newlines = newlines_before_item;
    if let Some(last) = leading.last()
        && last.2 {
            effective_newlines = 1; // force tight attachment
        }

    // Separator before the leading-comment block (or before the
    // item if there are no leading comments). For the very first
    // emitted item, no separator.
    let first_leading_newlines = leading.first().map_or(effective_newlines, |x| x.1);
    if saw_first {
        items.push(separator(first_leading_newlines, allow_blanks));
    }

    // Drain comments with their own inter-separators.
    let drained: Vec<_> = std::mem::take(leading);
    for (i, (doc, newlines_before, _is_doc)) in drained.into_iter().enumerate() {
        if i > 0 {
            items.push(separator(newlines_before, allow_blanks));
        }
        items.push(doc);
    }
    if !items.is_empty() && saw_first_was_set(items) {
        // Comments need a hardline before the upcoming item (or the
        // item-separator we just emitted). If the last thing pushed
        // was a comment text, separate with a hardline (or blank if
        // the user wrote a blank between the last comment and the
        // item — suppressed above for doc comments).
        if !matches!(items.last(), Some(Doc::HardLine | Doc::BlankLine)) {
            items.push(separator(effective_newlines, allow_blanks));
        }
    }
    items.push(item_doc);
}

/// True iff `items` ends with something that isn't a separator.
/// Equivalent to "we just pushed a comment".
fn saw_first_was_set(items: &[Doc]) -> bool {
    !items.is_empty()
}

/// Emit any leading-only trailing comments (after the last item but
/// before the closing boundary).
fn emit_trailing_comments(
    items: &mut Vec<Doc>,
    leading: &mut Vec<(Doc, usize, bool)>,
    _pending: usize,
    saw_first: bool,
    allow_blanks: bool,
) {
    let first_nl = leading.first().map_or(0, |x| x.1);
    if saw_first {
        items.push(separator(first_nl, allow_blanks));
    }
    let drained: Vec<_> = std::mem::take(leading);
    for (i, (doc, newlines_before, _is_doc)) in drained.into_iter().enumerate() {
        if i > 0 {
            items.push(separator(newlines_before, allow_blanks));
        }
        items.push(doc);
    }
}

/// Pick `BlankLine` vs `HardLine` based on `newlines` from source.
///
/// Respects [`FormatOptions::max_blank_lines`]: when zero, even a
/// `\n\n+` run collapses to a single hardline.
fn separator(newlines: usize, allow_blanks: bool) -> Doc {
    let max = with_ctx(|cx| cx.opts.max_blank_lines);
    if newlines >= 2 && allow_blanks && max >= 1 {
        blank_line()
    } else {
        hardline()
    }
}

/// Render a token's text as a [`Doc`]. Local helper so we don't
/// pull in `super::token_text` for the one call site here.
fn token_doc(t: &SyntaxToken) -> Doc {
    text(t.text().to_string())
}
