//! An explicit "I won't do that" answer for handlers whose only other
//! option would be a wrong edit.
//!
//! Most handlers say "no result" with `None`, which editors render as a
//! silent no-op. That is the wrong answer for a *destructive* request
//! like `textDocument/rename`: when the occurrence search behind the
//! edit is known to be unsound, the user needs to be told why, not
//! handed a corrupted buffer or a quiet nothing. A [`Refusal`] becomes
//! a JSON-RPC error response carrying the explanation.

/// A handler declining to act, with a user-facing reason. The message
/// is shown to the user verbatim by the editor, so it should say what
/// was refused and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refusal {
    pub message: String,
}

impl Refusal {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

/// A handler result that may also refuse: `Ok(Some(_))` is a result,
/// `Ok(None)` is "nothing here" (the ordinary LSP null response), and
/// `Err(_)` is a refusal to be reported to the user.
pub type Refusable<T> = Result<Option<T>, Refusal>;

/// The refusal message for a rename that targets a class member.
///
/// Member accesses (`this.x`, `obj.m()`) are not recorded as references
/// by the resolver, so a rename anchored on a member both misses every
/// dotted use site and — for a method, which shares
/// `SymbolKind::Function` with top-level functions — rewrites a
/// same-named free function instead. Refusing is strictly better than
/// either outcome. See leekwars#46.
pub(crate) fn member_rename(member: &str, class: &str) -> Refusal {
    Refusal::new(format!(
        "`{member}` is a member of class `{class}`. Renaming class members is \
         disabled: member accesses (`this.{member}`, `obj.{member}`) are not \
         tracked as references yet, so the rename would miss every use site and \
         edit unrelated code (leekwars#46)."
    ))
}
