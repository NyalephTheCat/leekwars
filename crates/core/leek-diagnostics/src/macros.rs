//! The [`diag!`](crate::diag) construction macro.
//!
//! `diag!` is the ergonomic front door for building a [`Diagnostic`]:
//! the severity is taken from the code's catalog default, the message
//! is `format!`-style (so inline captures like `` "`{name}`" `` work),
//! and optional `note` / `label` / `suggest` clauses map onto the
//! `with_*` builder methods.
//!
//! ```
//! use leek_diagnostics::{codes, diag, Severity, Suggestion};
//! use leek_span::{SourceId, Span};
//! let span = Span::new(SourceId::new(1).unwrap(), 0, 1);
//! let name = "damge";
//!
//! // Severity from the catalog; message interpolated:
//! let d = diag!(codes::UNKNOWN_VARIABLE, span, "unknown variable `{name}`");
//! assert_eq!(d.severity, Severity::Error);
//! assert_eq!(d.message, "unknown variable `damge`");
//!
//! // Optional clauses after `;` (each repeatable):
//! let d = diag!(codes::REDECLARED_SYMBOL, span, "`{name}` is already declared";
//!     label = (span, "first declared here"),
//!     note = "shadowing is forbidden in the same scope",
//!     suggest = Suggestion::replace("rename it", span, "damage"),
//! );
//! assert_eq!(d.labels.len(), 1);
//! assert_eq!(d.notes.len(), 1);
//! assert_eq!(d.suggestions.len(), 1);
//!
//! // Explicit severity override when it differs from the catalog default:
//! let d = diag!(warning, codes::UNKNOWN_VARIABLE, span, "soft warning");
//! assert_eq!(d.severity, Severity::Warning);
//! ```
//!
//! [`Diagnostic`]: crate::Diagnostic

/// Materialize a `format_args!` result into a `String`.
///
/// This is what `diag!` uses instead of the `format!` macro so that a
/// static message (no interpolation) does not trip
/// `clippy::useless_format`, while inline captures still work. Not part
/// of the public API — call [`diag!`](crate::diag) instead.
#[doc(hidden)]
#[must_use]
pub fn __format_message(args: std::fmt::Arguments<'_>) -> String {
    std::fmt::format(args)
}

/// Build a [`Diagnostic`](crate::Diagnostic). See the [module
/// docs](self) for the grammar and examples.
#[macro_export]
macro_rules! diag {
    // ----- explicit severity -----
    (error, $code:expr, $span:expr, $fmt:literal $(, $arg:expr)* $(,)? $(; $($clause:tt)*)?) => {
        $crate::__diag_clauses!(
            $crate::Diagnostic::error(
                $code, $span, $crate::__format_message(format_args!($fmt $(, $arg)*)),
            )
            $(; $($clause)*)?
        )
    };
    (warning, $code:expr, $span:expr, $fmt:literal $(, $arg:expr)* $(,)? $(; $($clause:tt)*)?) => {
        $crate::__diag_clauses!(
            $crate::Diagnostic::warning(
                $code, $span, $crate::__format_message(format_args!($fmt $(, $arg)*)),
            )
            $(; $($clause)*)?
        )
    };
    // ----- catalog-default severity -----
    ($code:expr, $span:expr, $fmt:literal $(, $arg:expr)* $(,)? $(; $($clause:tt)*)?) => {
        $crate::__diag_clauses!(
            $crate::Diagnostic::at(
                $code, $span, $crate::__format_message(format_args!($fmt $(, $arg)*)),
            )
            $(; $($clause)*)?
        )
    };
}

/// Internal clause muncher for [`diag!`]. Applies `note` / `label` /
/// `suggest` clauses left-to-right onto a base `Diagnostic` expression.
#[doc(hidden)]
#[macro_export]
macro_rules! __diag_clauses {
    ($d:expr) => { $d };
    ($d:expr;) => { $d };
    ($d:expr; note = $e:expr $(, $($rest:tt)*)?) => {
        $crate::__diag_clauses!($d.with_note($e) $(; $($rest)*)?)
    };
    ($d:expr; label = ($s:expr, $m:expr) $(, $($rest:tt)*)?) => {
        $crate::__diag_clauses!($d.with_label($s, $m) $(; $($rest)*)?)
    };
    ($d:expr; suggest = $e:expr $(, $($rest:tt)*)?) => {
        $crate::__diag_clauses!($d.with_suggestion($e) $(; $($rest)*)?)
    };
}

#[cfg(test)]
mod tests {
    use crate::{Applicability, Severity, Suggestion, codes};
    use leek_span::{SourceId, Span};

    fn span() -> Span {
        Span::new(SourceId::new(1).unwrap(), 0, 1)
    }

    #[test]
    fn catalog_severity_and_static_message() {
        let d = diag!(codes::UNUSED_VARIABLE, span(), "unused");
        assert_eq!(d.severity, Severity::Warning); // catalog default
        assert_eq!(d.message, "unused");
        assert!(d.notes.is_empty() && d.labels.is_empty());
    }

    #[test]
    fn inline_capture_and_positional_args() {
        let name = "x";
        let d = diag!(codes::UNKNOWN_VARIABLE, span(), "unknown `{name}`");
        assert_eq!(d.message, "unknown `x`");
        let d = diag!(
            codes::INVALID_PARAMETER_COUNT,
            span(),
            "expected {}, got {}",
            2,
            3
        );
        assert_eq!(d.message, "expected 2, got 3");
    }

    #[test]
    fn explicit_severity() {
        let d = diag!(warning, codes::UNKNOWN_VARIABLE, span(), "soft");
        assert_eq!(d.severity, Severity::Warning);
        let d = diag!(error, codes::SHADOWED_BINDING, span(), "hard");
        assert_eq!(d.severity, Severity::Error);
    }

    #[test]
    fn all_clauses() {
        let d = diag!(codes::REDECLARED_SYMBOL, span(), "`{}` redeclared", "x";
            label = (span(), "first here"),
            note = "no shadowing",
            note = format!("second {}", "note"),
            suggest = Suggestion::remove("drop it", span())
                .with_applicability(Applicability::MaybeIncorrect),
        );
        assert_eq!(d.labels.len(), 1);
        assert_eq!(d.notes, vec!["no shadowing", "second note"]);
        assert_eq!(d.suggestions.len(), 1);
        assert_eq!(
            d.suggestions[0].applicability,
            Applicability::MaybeIncorrect
        );
    }
}
