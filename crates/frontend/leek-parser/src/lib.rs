//! Recursive-descent parser for Leekscript.
//!
//! Builds a [`rowan`] green tree directly via `Parser`.
//!
//! ## Entry points
//!
//! Everything a parse needs beyond the text and its `SourceId` travels in
//! one [`ParseOptions`] — language version, experimental
//! [`ParseFeatures`], and the class names declared in other files of the
//! program. Pick by what you already have:
//!
//! * **text** → [`parse_file_with`], which lexes internally and returns a
//!   [`ParsedFile`] (green tree, AST view, diagnostics). It is the single
//!   implementation of the lex → parse → prepend-lex-diagnostics sequence;
//!   [`parse_with_features`] is the same thing shaped as a
//!   [`ParseResult`].
//! * **tokens you already lexed** → [`parse_tokens_with`] (or
//!   [`parse_tokens_with_classes`]), which leaves the lex diagnostics to
//!   you. See the [`entry`] module docs for which path reports them.
//! * **a salsa database** → [`pipeline::parse_query`] for an open buffer
//!   and [`pipeline::parse_project_file_query`] for an indexed on-disk
//!   file.
//!
//! No entry point reads `LEEK_EXPERIMENTAL_*`: the flags are threaded in
//! as data, read once at a driver's boundary via
//! [`FeatureFlags::from_env`](leek_span::FeatureFlags::from_env). The
//! wrappers that used to default them off the environment — [`parse`],
//! [`parse_tokens`], [`parse_file`] and [`parse_file_with_classes`] — are
//! deprecated shims.
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
mod header;
mod parser;

pub mod ast;
pub mod entry;
pub mod pipeline;

pub use entry::{ParseOptions, ParsedFile, parse_file_with};
pub use header::parse_signature_header;
pub use parser::{
    ParseFeatures, ParseResult, parse_tokens_with, parse_tokens_with_classes, parse_with_features,
    scan_class_names,
};

/// The env-defaulting shims, kept at their old paths for one release.
/// `#[deprecated]` fires at the use site, not the re-export, so the
/// re-export has to opt out of its own warning — the `expect` also fails
/// loudly on the day these four are deleted.
#[expect(
    deprecated,
    reason = "re-exporting the deprecation shims is the point of this statement"
)]
pub use crate::{
    entry::{parse_file, parse_file_with_classes},
    parser::{parse, parse_tokens},
};
