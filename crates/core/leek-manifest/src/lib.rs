//! `Miku.toml` — Leekscript project manifest.
//!
//! Single source of truth for the schema documented in
//! `bins/miku/README.md` ("The `Miku.toml` schema"). Used by `miku`
//! (workspace tool), `leekc` (`--fmt-config`), `leek-fmt`, the linter, and
//! the LSP.
//!
//! ## Validation rules
//!
//! Unknown **top-level** keys → [`ManifestError`] (typo protection).
//! Unknown keys **inside** known tables → [`ManifestWarning`]
//! (forward-compat).
//!
//! Both carry the byte [`Span`](leek_span::Span) of the offending key or
//! value, so a tool with a
//! [`Reporter`](leek_diagnostics::Reporter) renders a caret under it rather
//! than a bare line of prose. See [`ManifestErrorKind`] for what the parser
//! distinguishes.
//!
//! Several tables are recognized but **not interpreted** in v0.1:
//! `[lsp]`, `[bench]`, `[experimental]`, `[profiles]`, `[workspace]`,
//! `[toolchain]`. They parse without errors so older manifests work,
//! but the corresponding behavior is deferred.

mod discover;
mod error;
mod format;
mod parse;
mod types;

pub use discover::{ManifestLoad, discover, load_from, load_str};
pub use error::{ManifestError, ManifestErrorKind, ManifestWarning, ManifestWarningKind};
pub use format::{
    BraceStyle, ControlBraces, FormatOptionError, FormatOptions, IndentStyle, LineEnding,
    OperatorPosition, QuoteStyle, Semicolons, TrailingComma,
};
pub use types::{
    BackendKind, BackendSettings, BackendTable, FightTable, JavaMode, LintTable, Manifest,
    NativeOptLevel, PathsTable, ProjectTable, TestTable,
};
