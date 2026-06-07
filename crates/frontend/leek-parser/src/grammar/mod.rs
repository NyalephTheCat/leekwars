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

/// Consume `@name [(args)]` zero or more times. Each annotation is its
/// own `Annotation` node, emitted into the current scope.
pub(crate) fn consume_annotations(p: &mut Parser) {
    while p.at(SyntaxKind::At) {
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
