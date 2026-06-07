//! Diagnostic model, catalog, and rendering.
//!
//! Diagnostic codes are declared in `catalog.yaml` and generated at
//! build time into the [`codes`] module. Each producer (lexer, parser,
//! resolver, type checker, …) refers to the constants in [`codes`]
//! rather than minting its own.
//!
//! See `doc/diagnostics.md` for the contract.

#![cfg_attr(docsrs, feature(doc_auto_cfg))]

use leek_span::{LineTable, Span};

/// Generated from `catalog.yaml` by `build.rs`.
pub mod codes {
    include!(concat!(env!("OUT_DIR"), "/catalog.rs"));
}
pub mod convert;
mod render;
pub mod report;
mod suggest;

pub use render::{Renderer, Style};
pub use report::{ColorWhen, LintLevels, MessageFormat, Reporter};
pub use suggest::{best_match, suggest_similar};

#[cfg(feature = "serde")]
mod serde_impls;

// ---- Severity ----

#[cfg_attr(feature = "salsa", derive(salsa::Update))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Severity {
    Error,
    Warning,
    Info,
    Hint,
}

impl Severity {
    pub fn as_str(self) -> &'static str {
        match self {
            Severity::Error => "error",
            Severity::Warning => "warning",
            Severity::Info => "info",
            Severity::Hint => "hint",
        }
    }
}

// ---- Code (stable identifier + metadata lookup) ----

/// Stable diagnostic code. The wrapped string (e.g. `"E0250"`) is the
/// ID published to tooling; the human-readable name (e.g.
/// `"AssignmentIncompatibleType"`) and default severity are looked up
/// from [`codes::CATALOG`].
///
/// `Code` is kept as a `&'static str` newtype so the wire format is
/// trivially serializable and the in-process value is cheap to copy.
#[cfg_attr(feature = "salsa", derive(salsa::Update))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Code(pub &'static str);

impl Code {
    pub const fn id(&self) -> &'static str {
        self.0
    }

    /// Catalog entry for this code, if any.
    pub fn meta(&self) -> Option<&'static CodeMeta> {
        codes::lookup_meta(self.0)
    }

    /// Canonical name (`"AssignmentIncompatibleType"`). Falls back to
    /// the ID for codes not yet in the catalog.
    pub fn name(&self) -> &'static str {
        self.meta().map_or(self.0, |m| m.name)
    }

    /// Default severity. Falls back to `Error` if not in the catalog.
    pub fn default_severity(&self) -> Severity {
        self.meta()
            .map_or(Severity::Error, |m| m.default_severity)
    }

    /// Extended explanation (the `explain/<ID>.md` write-up) shown by
    /// `miku explain <CODE>`, or `None` if none has been authored for
    /// this code.
    pub fn explain(&self) -> Option<&'static str> {
        codes::explain_for(self.0)
    }
}

impl std::fmt::Display for Code {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.0)
    }
}

/// Metadata for a diagnostic code — the structured form of an entry
/// in [`codes::CATALOG`].
#[derive(Debug, Clone, Copy)]
pub struct CodeMeta {
    pub id: &'static str,
    pub name: &'static str,
    pub default_severity: Severity,
    pub category: Category,
}

#[cfg_attr(feature = "salsa", derive(salsa::Update))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Category {
    Lexer,
    Parser,
    Pragma,
    Resolver,
    Types,
    Lowering,
    Manifest,
    Rewrite,
    Lint,
    Runtime,
}

// ---- Diagnostic ----

/// A diagnostic with optional labels, notes, and machine-applicable
/// fix suggestions. `Diagnostic::new(code, sev, span, msg)` remains
/// the minimal constructor; the rest are opt-in via builder methods.
#[cfg_attr(feature = "salsa", derive(salsa::Update))]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnostic {
    pub code: Code,
    pub severity: Severity,
    /// Primary span — where the diagnostic is pointing. `message` is
    /// the label for this span.
    pub span: Span,
    pub message: String,
    /// Extra spans elsewhere in the source that are *related* to this
    /// diagnostic (e.g. "previously declared here").
    pub labels: Vec<Label>,
    /// Free-form context lines printed below the snippet.
    pub notes: Vec<String>,
    /// Machine-applicable autofixes — at most one is applied per
    /// invocation; renderers display all.
    pub suggestions: Vec<Suggestion>,
}

impl Diagnostic {
    pub fn new(code: Code, severity: Severity, span: Span, message: impl Into<String>) -> Self {
        Self {
            code,
            severity,
            span,
            message: message.into(),
            labels: Vec::new(),
            notes: Vec::new(),
            suggestions: Vec::new(),
        }
    }

    pub fn error(code: Code, span: Span, message: impl Into<String>) -> Self {
        Self::new(code, Severity::Error, span, message)
    }

    pub fn warning(code: Code, span: Span, message: impl Into<String>) -> Self {
        Self::new(code, Severity::Warning, span, message)
    }

    /// Attach a secondary label at `span` — typically used for
    /// "previously declared here" / "this binding had type X".
    pub fn with_label(mut self, span: Span, label: impl Into<String>) -> Self {
        self.labels.push(Label {
            span,
            message: label.into(),
        });
        self
    }

    /// Attach a free-form note printed below the snippet.
    pub fn with_note(mut self, note: impl Into<String>) -> Self {
        self.notes.push(note.into());
        self
    }

    /// Attach a machine-applicable fix suggestion.
    pub fn with_suggestion(mut self, suggestion: Suggestion) -> Self {
        self.suggestions.push(suggestion);
        self
    }
}

/// A secondary labeled location attached to a diagnostic.
#[cfg_attr(feature = "salsa", derive(salsa::Update))]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Label {
    pub span: Span,
    pub message: String,
}

/// A machine-applicable fix suggestion — one or more text edits that
/// would resolve the diagnostic.
#[cfg_attr(feature = "salsa", derive(salsa::Update))]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Suggestion {
    pub message: String,
    pub edits: Vec<TextEdit>,
    pub applicability: Applicability,
}

impl Suggestion {
    pub fn replace(message: impl Into<String>, span: Span, with: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            edits: vec![TextEdit {
                span,
                replacement: with.into(),
            }],
            applicability: Applicability::MachineApplicable,
        }
    }
}

#[cfg_attr(feature = "salsa", derive(salsa::Update))]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextEdit {
    pub span: Span,
    pub replacement: String,
}

/// How confident we are that applying a suggestion is correct.
/// Editors should auto-apply only `MachineApplicable` ones without
/// confirmation.
#[cfg_attr(feature = "salsa", derive(salsa::Update))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Applicability {
    /// Trivially correct — safe to apply unattended.
    MachineApplicable,
    /// Probably correct but worth a human glance.
    MaybeIncorrect,
    /// Has placeholders the user must fill in.
    HasPlaceholders,
    /// Heuristic — apply only after review.
    Unspecified,
}

// ---- Rendering helpers ----

impl Diagnostic {
    /// Render the diagnostic against a source-line table and the
    /// file contents into a colored snippet. See [`Renderer`] for
    /// configuration.
    pub fn render(&self, source: &str, file: &str, lines: &LineTable) -> String {
        Renderer::default().render(self, source, file, lines)
    }
}

// ---- Severity overrides (`--allow` / `--warn` / `--deny`) ----

/// Per-code severity overrides applied at emission time. Mirrors the
/// `--deny <code>`, `--warn <code>`, `--allow <code>` CLI flags spec'd
/// in `doc/diagnostics.md` §5.
#[derive(Debug, Clone, Default)]
pub struct SeverityConfig {
    overrides: std::collections::HashMap<&'static str, Severity>,
    allowed: std::collections::HashSet<&'static str>,
}

impl SeverityConfig {
    pub fn new() -> Self {
        Self::default()
    }

    /// Promote `code` to error (or keep it if already error).
    pub fn deny(&mut self, code: Code) {
        self.overrides.insert(code.0, Severity::Error);
        self.allowed.remove(code.0);
    }

    /// Force `code` to warning level.
    pub fn warn(&mut self, code: Code) {
        self.overrides.insert(code.0, Severity::Warning);
        self.allowed.remove(code.0);
    }

    /// Silence `code` entirely. [`apply_mut`] returns `false` for it.
    pub fn allow(&mut self, code: Code) {
        self.allowed.insert(code.0);
        self.overrides.remove(code.0);
    }

    /// Apply overrides in-place. Returns `false` if the diagnostic
    /// should be dropped (allowed/silenced).
    ///
    /// [`apply_mut`]: Self::apply_mut
    #[must_use]
    pub fn apply_mut(&self, diag: &mut Diagnostic) -> bool {
        if self.allowed.contains(diag.code.0) {
            return false;
        }
        if let Some(&sev) = self.overrides.get(diag.code.0) {
            diag.severity = sev;
        }
        true
    }
}
