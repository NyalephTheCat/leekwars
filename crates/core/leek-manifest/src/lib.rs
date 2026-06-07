//! `Miku.toml` — Leekscript project manifest.
//!
//! Single source of truth for the schema documented in
//! `doc/manifest.md`. Used by `miku` (workspace tool), `leekc`
//! (`--fmt-config`), `leek-fmt`, the linter, and the LSP.
//!
//! ## Validation rules
//!
//! Unknown **top-level** keys → [`ManifestError`] (typo protection).
//! Unknown keys **inside** known tables → [`ManifestWarning`]
//! (forward-compat).
//!
//! Several tables are recognized but **not interpreted** in v0.1:
//! `[lsp]`, `[bench]`, `[experimental]`, `[profiles]`, `[workspace]`,
//! `[toolchain]`. They parse without errors so older manifests work,
//! but the corresponding behavior is deferred.

mod discover;
mod format;
mod parse;
mod types;

pub use discover::{ManifestLoad, discover, load_from, load_str};
pub use format::{BraceStyle, FormatOptions, IndentStyle, TrailingComma};
pub use parse::{ManifestError, ManifestWarning};
pub use types::{
    BackendKind, BackendSettings, BackendTable, JavaMode, LintTable, Manifest, PathsTable,
    ProjectTable, TestTable,
};
