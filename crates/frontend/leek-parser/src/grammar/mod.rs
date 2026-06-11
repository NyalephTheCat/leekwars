//! Grammar productions.
//!
//! Entry point: [`source_file`]. Sub-modules split expressions
//! (`expr`) from statements (`stmt`).

mod decls;
mod expr;
mod stmt;
mod types;

use leek_syntax::SyntaxKind;

use crate::parser::Parser;

/// Top-level entry: parse a complete `.leek` file.
///
/// First-slice scope: zero or more statements. Functions, classes,
/// includes, and globals are added in later slices.
pub(crate) fn source_file(p: &mut Parser) {
    p.start_node(SyntaxKind::SourceFile);
    p.finish_trivia();
    while !p.at_eof() {
        let before = p.position();
        top_level_item(p);
        p.finish_trivia();
        if !p.at_eof() && p.position() == before {
            let kind = p.current();
            p.err_and_bump(format!("unexpected token at top level: {kind:?}"));
        }
    }
    // Flush any trailing trivia (a final newline or comment after the last
    // item) into the tree before closing the root, so the CST stays lossless —
    // `at_eof()` skips trivia, so the loop can exit with trailing trivia still
    // pending.
    p.finish_trivia();
    p.finish_node();
}

/// One item at the file's top level: a function declaration, a class
/// declaration, an `include(...)`/`import ...`, or any statement.
fn top_level_item(p: &mut Parser) {
    // Annotations like `@deprecated`, `@override`, `@unused`, `@todo`
    // precede the declaration they apply to. We set a checkpoint
    // before them so the eventual decl node wraps them.
    let cp = p.checkpoint();
    consume_annotations(p);
    match p.current() {
        SyntaxKind::KwFunction => decls::fn_decl_at(p, cp),
        SyntaxKind::KwClass => decls::class_decl_at(p, cp),
        SyntaxKind::KwInclude => stmt::include_stmt(p),
        SyntaxKind::KwImport => stmt::import_stmt(p),
        _ => stmt::stmt_at(p, cp),
    }
}

/// Whether the `@` at the cursor starts an *annotation* (`@unused`,
/// `@deprecated(...)` preceding a declaration) rather than a reference
/// expression statement (`@LamaSwag();` — upstream's `@` prefix operator
/// applied to a call). Annotations are our extension, so when the
/// `@name[(args)]` construct is immediately followed by `;` we prefer
/// the upstream expression reading and leave the `@` for the statement
/// parser.
fn at_annotation(p: &Parser) -> bool {
    if !p.at(SyntaxKind::At) {
        return false;
    }
    let name = p.nth(1);
    if !(matches!(name, SyntaxKind::Ident) || name.is_keyword()) {
        // Malformed either way — let `consume_annotations` report it.
        return true;
    }
    let mut i = 2;
    if p.nth(i) == SyntaxKind::LParen {
        // Skip the balanced `( … )` (bounded so a runaway unclosed
        // paren can't scan the whole file).
        let mut depth = 0usize;
        loop {
            match p.nth(i) {
                SyntaxKind::LParen => depth += 1,
                SyntaxKind::RParen => {
                    depth -= 1;
                    if depth == 0 {
                        i += 1;
                        break;
                    }
                }
                SyntaxKind::Eof => break,
                _ => {}
            }
            i += 1;
            if i > 256 {
                break;
            }
        }
    }
    p.nth(i) != SyntaxKind::Semicolon
}

/// Consume `@name [(args)]` zero or more times. Each annotation is its
/// own `Annotation` node, emitted into the current scope.
pub(crate) fn consume_annotations(p: &mut Parser) {
    while at_annotation(p) {
        p.start_node(SyntaxKind::Annotation);
        p.bump(); // '@'
        // Annotation name — accept any identifier (and any keyword,
        // since names like `@deprecated`, `@override` may collide
        // with reserved words).
        if matches!(p.current(), SyntaxKind::Ident) || p.current().is_keyword() {
            p.bump();
        } else {
            p.error(format!(
                "expected annotation name after `@`, found {:?}",
                p.current()
            ));
        }
        // Optional `( args )`.
        if p.at(SyntaxKind::LParen) {
            expr::annotation_args(p);
        }
        p.finish_node();
    }
}
