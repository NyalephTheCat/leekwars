//! Typed manifest errors and warnings.
//!
//! Every variant names *what* the schema rejected — the offending key, the
//! table, the value — rather than a finished sentence, and carries the byte
//! [`Span`] of the offending text where the document had one. Rendering is
//! `Display` (plain prose, for front-ends with no reporter) or
//! [`IntoDiagnostic`] (a caret under the key, for the ones with a
//! [`Reporter`](leek_diagnostics::Reporter)).
//!
//! Spans are `Option` on purpose: `toml_edit` has no span for an implicitly
//! created table (`project.name = …` at the top level synthesizes `project`),
//! and none at all for an error raised before or outside the document — a
//! read failure, a manifest that was never found. Those render detached,
//! pointing at offset 0 of the manifest.

use std::path::PathBuf;

use leek_diagnostics::{Code, Diagnostic, IntoDiagnostic, codes};
use leek_span::Span;

use crate::parse::KNOWN_TOP_LEVEL;

/// The span a manifest error falls back to when it has none of its own.
fn detached_span() -> Span {
    Span::new(Span::MANIFEST_SOURCE, 0, 0)
}

/// Convert a `toml_edit` byte range into a manifest [`Span`]. Returns `None`
/// for a manifest larger than `u32::MAX` bytes, which is not a thing.
pub(crate) fn span_of(range: Option<std::ops::Range<usize>>) -> Option<Span> {
    let range = range?;
    let start = u32::try_from(range.start).ok()?;
    let end = u32::try_from(range.end).ok()?;
    Some(Span::new(Span::MANIFEST_SOURCE, start, end))
}

/// Hard parse failure — invalid TOML, missing required field, unknown
/// top-level key, or a typed field with the wrong shape.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManifestError {
    pub kind: ManifestErrorKind,
    /// Where in `Miku.toml` the problem is, when the document knows.
    pub span: Option<Span>,
}

/// What went wrong, as facts rather than prose.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ManifestErrorKind {
    /// The file is not valid TOML. `message` is `toml_edit`'s own rendering,
    /// which already includes the offending line.
    Toml { message: String },
    /// Reading the file failed.
    Io { path: PathBuf, message: String },
    /// The current directory could not be determined, so discovery had no
    /// place to start.
    Cwd { message: String },
    /// [`discover`](crate::discover) reached the filesystem root without
    /// finding a `Miku.toml`.
    NotFound { start: PathBuf },
    /// A top-level key outside the known set — the typo guard.
    UnknownTopLevelKey { key: String },
    /// A required table is absent (`[project]`).
    MissingTable { name: &'static str },
    /// A required key inside a table that is present (`project.name`).
    MissingKey { key: String },
    /// A key that must hold a table holds something else.
    NotATable { key: String },
    /// A scalar key holds the wrong TOML type.
    WrongType {
        key: String,
        /// The type, spelled as the message wants it: `"a string"`.
        expected: &'static str,
    },
    /// The right TOML type, but a value the schema rejects.
    BadValue {
        key: String,
        /// What would have been accepted: `"1..=4"`, `"\"exact\" or \"clean\""`.
        expected: String,
        /// The rejected value as the message shows it, when showing it helps.
        got: Option<String>,
    },
}

impl ManifestError {
    /// An error located at `span` (which may be `None` when the document had
    /// no span for the offending item).
    pub(crate) fn at(span: Option<Span>, kind: ManifestErrorKind) -> Self {
        Self { kind, span }
    }

    /// An error with no location — raised before or outside the document.
    pub(crate) fn detached(kind: ManifestErrorKind) -> Self {
        Self { kind, span: None }
    }
}

impl ManifestErrorKind {
    /// The catalog code this kind reports as.
    fn code(&self) -> Code {
        match self {
            ManifestErrorKind::Toml { .. }
            | ManifestErrorKind::Io { .. }
            | ManifestErrorKind::Cwd { .. }
            | ManifestErrorKind::NotFound { .. } => codes::MANIFEST_PARSE_ERROR,
            ManifestErrorKind::UnknownTopLevelKey { .. } => codes::MANIFEST_UNKNOWN_KEY,
            ManifestErrorKind::MissingTable { .. } | ManifestErrorKind::MissingKey { .. } => {
                codes::MANIFEST_MISSING_ENTRY
            }
            ManifestErrorKind::NotATable { .. } | ManifestErrorKind::WrongType { .. } => {
                codes::MANIFEST_WRONG_TYPE
            }
            ManifestErrorKind::BadValue { .. } => codes::MANIFEST_BAD_VALUE,
        }
    }

    /// The message *without* the `Miku.toml: ` prefix — what a diagnostic
    /// wants, since the reporter already prints the file name above the
    /// snippet.
    fn body(&self) -> String {
        match self {
            ManifestErrorKind::Toml { message } => message.clone(),
            ManifestErrorKind::Io { path, message } => {
                format!("reading {}: {message}", path.display())
            }
            ManifestErrorKind::Cwd { message } => format!("cwd: {message}"),
            ManifestErrorKind::NotFound { start } => format!(
                "no `Miku.toml` found in {} or any parent directory",
                start.display()
            ),
            ManifestErrorKind::UnknownTopLevelKey { key } => format!(
                "unknown top-level key `{key}` (expected one of: {})",
                KNOWN_TOP_LEVEL.join(", ")
            ),
            ManifestErrorKind::MissingTable { name } => {
                format!("missing required table `[{name}]`")
            }
            ManifestErrorKind::MissingKey { key } => format!("missing required key `{key}`"),
            ManifestErrorKind::NotATable { key } => format!("`{key}` must be a table"),
            ManifestErrorKind::WrongType { key, expected } => {
                format!("{key} must be {expected}")
            }
            ManifestErrorKind::BadValue {
                key,
                expected,
                got: Some(got),
            } => format!("{key} must be {expected}, got {got}"),
            ManifestErrorKind::BadValue {
                key,
                expected,
                got: None,
            } => format!("{key} must be {expected}"),
        }
    }
}

impl std::fmt::Display for ManifestError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The `Miku.toml: ` prefix is what makes a bare `{e}` in a front-end
        // with no reporter (leekc) name the file it is complaining about.
        write!(f, "Miku.toml: {}", self.kind.body())
    }
}

impl std::error::Error for ManifestError {}

impl IntoDiagnostic for ManifestError {
    fn into_diagnostic(self) -> Diagnostic {
        Diagnostic::at(
            self.kind.code(),
            self.span.unwrap_or_else(detached_span),
            self.kind.body(),
        )
    }
}

impl From<ManifestError> for Diagnostic {
    fn from(err: ManifestError) -> Self {
        err.into_diagnostic()
    }
}

/// Soft warning — unknown key inside a known table, or a deferred
/// table that was used. Surfaced so editors can show squiggles but
/// non-fatal so older toolchains can still load newer manifests.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManifestWarning {
    pub kind: ManifestWarningKind,
    pub span: Option<Span>,
}

/// What the manifest said that this toolchain does not act on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ManifestWarningKind {
    /// A key inside a known table that this toolchain does not know.
    UnknownField {
        /// The dotted path of the containing table (`"backend.java"`).
        table: String,
        key: String,
    },
    /// A table that parses but whose behavior is deferred.
    DeferredTable { name: &'static str },
}

impl ManifestWarning {
    pub(crate) fn at(span: Option<Span>, kind: ManifestWarningKind) -> Self {
        Self { kind, span }
    }
}

impl ManifestWarningKind {
    fn code(&self) -> Code {
        match self {
            ManifestWarningKind::UnknownField { .. } => codes::MANIFEST_UNKNOWN_FIELD,
            ManifestWarningKind::DeferredTable { .. } => codes::MANIFEST_DEFERRED_TABLE,
        }
    }

    fn body(&self) -> String {
        match self {
            ManifestWarningKind::UnknownField { table, key } => {
                format!("unknown key `{table}.{key}` (ignored)")
            }
            ManifestWarningKind::DeferredTable { name } => {
                format!("`[{name}]` is parsed but not yet interpreted in this toolchain")
            }
        }
    }
}

impl std::fmt::Display for ManifestWarning {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Miku.toml: {}", self.kind.body())
    }
}

impl IntoDiagnostic for ManifestWarning {
    fn into_diagnostic(self) -> Diagnostic {
        Diagnostic::at(
            self.kind.code(),
            self.span.unwrap_or_else(detached_span),
            self.kind.body(),
        )
    }
}

impl From<ManifestWarning> for Diagnostic {
    fn from(warning: ManifestWarning) -> Self {
        warning.into_diagnostic()
    }
}
