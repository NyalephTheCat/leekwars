//! Recursive-descent parser for Leekscript.
//!
//! Builds a [`rowan`] green tree directly via `Parser`. Public entry
//! points: [`parse`] for a bare [`ParseResult`], and [`parse_file_with`]
//! for a whole file — green tree, AST view and diagnostics — with the
//! experimental [`FeatureFlags`](leek_span::FeatureFlags) passed in
//! rather than read from the environment.
//!
//! ## What this slice covers
//!
//! Expressions: literal, identifier, parenthesized, unary (`-`, `!`),
//! binary (`* /  %  + -  < <= > >=  == != === !==  &&  || ??  =`),
//! postfix call `f(args)`.
//!
//! Statements: `var name = expr;`, `return [expr];`, expression statement.
//!
//! Top level: a sequence of statements. Functions, classes, includes,
//! control-flow blocks, and typed declarations come in later slices.
//!
//! Errors don't stop the parse: unrecognized input is wrapped in an
//! [`ErrorNode`](leek_syntax::SyntaxKind::ErrorNode) and parsing
//! resynchronizes to the next `;` or close bracket.

mod entry;
mod grammar;
mod header;
mod parser;

pub mod ast;
pub mod pipeline;

pub use entry::{ParsedFile, parse_file, parse_file_with, parse_file_with_classes};
pub use header::parse_signature_header;
pub use parser::{
    ParseFeatures, ParseResult, parse, parse_tokens, parse_tokens_with, parse_tokens_with_classes,
    parse_with_features, scan_class_names,
};
