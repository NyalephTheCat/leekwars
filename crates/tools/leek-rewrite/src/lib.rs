//! Safe text-edit composition for Leekscript tooling.
//!
//! The core type is [`EditSet`]: a sorted, validated collection of
//! byte-range edits over a single source file. It catches the
//! common mistakes that bite hand-rolled rewriters:
//!
//! - Overlapping edits silently dropping each other.
//! - Out-of-order application shifting subsequent offsets.
//! - Spans pointing past the end of source.
//! - Multi-edit rewrites landing half-way: [`EditSet::try_push_all`]
//!   takes a group of [`Edit`]s and pushes all of them or none.
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
//! // Rewrites that are only correct as a unit go in as one group.
//! edits.try_push_all([
//!     Edit::for_token(&first_param, "value".into()),
//!     Edit::for_token(&second_param, "key".into()),
//! ])?;
//! let result = edits.apply(source);
//! ```
//!
//! A rejected edit is a [`Result`], never a silent no-op: callers are
//! expected to surface it (as a diagnostic, say) rather than carry on
//! as if the rewrite had been applied.
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
