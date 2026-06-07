//! Recursive-descent parser for Leekscript.
//!
//! Builds a [`rowan`] green tree directly via [`Parser`]. Public entry
//! point: [`parse`].
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

mod grammar;
mod parser;

pub mod ast;
pub mod pipeline;

pub use parser::{
    ParseFeatures, ParseResult, parse, parse_tokens, parse_tokens_with, parse_with_features,
};
