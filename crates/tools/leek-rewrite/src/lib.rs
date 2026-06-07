//! Safe text-edit composition for Leekscript tooling.
//!
//! The core type is [`EditSet`]: a sorted, validated collection of
//! byte-range edits over a single source file. It catches the
//! common mistakes that bite hand-rolled rewriters:
//!
//! - Overlapping edits silently dropping each other.
//! - Out-of-order application shifting subsequent offsets.
//! - Spans pointing past the end of source.
//!
//! Higher-level helpers wrap edits in terms of [`SyntaxToken`] and
//! [`SyntaxNode`], so callers don't have to do span arithmetic by
//! hand:
//!
//! ```ignore
//! use leek_rewrite::EditSet;
//! let mut edits = EditSet::new(source.len());
//! edits.replace_token(&ident, "new_name".into())?;
//! edits.replace_node(&array_expr, "[1, 2, 3]".into())?;
//! let result = edits.apply(source);
//! ```
//!
//! `EditSet` is the foundation for:
//! - **Formatter range formatting** — replace a single subtree's
//!   text with its formatted form.
//! - **LSP code actions** — turn [`Diagnostic::suggestions`] into
//!   safe `WorkspaceEdit`s.
//! - **Future** refactors: cross-symbol renames, v3→v4 source
//!   migration, etc.
//!
//! [`Diagnostic::suggestions`]: leek_diagnostics::Diagnostic

mod edit;
mod edit_set;

pub use edit::{Edit, EditError};
pub use edit_set::EditSet;
