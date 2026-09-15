//! The formatter as a tracked query.
//!
//! **Unverified.** This formats; it does not run the safety net. Every
//! consumer that writes the text to a file, an editor buffer or stdout
//! must call [`check_equivalence`](crate::check_equivalence) against the
//! input text first and refuse the output on error — the contract
//! `miku fmt`, `leekc --emit fmt` and the LSP formatting handlers all
//! honor.

use std::sync::Arc;

use leek_syntax::version::version_from_byte;

use crate::FormatOptions;

#[derive(salsa::Update, Debug, Clone, PartialEq, Eq)]
pub struct FormatQueryResult {
    pub text: Arc<String>,
}

/// The formatter settings a [`format_query`] formats under, interned so
/// that they can be a tracked-query *argument*.
///
/// Interned rather than passed as a loose [`FormatOptions`] for the
/// reason [`ProgramClasses`](leek_query::salsa::ProgramClasses) is: a
/// tracked query's arguments have to be `Copy`, and interning gives one
/// identity to a settings value, so two callers that assemble the same
/// options hit the same memo.
///
/// Keying on them is what lets the editor's own settings go through the
/// cache. While the query formatted with `FormatOptions::default()`
/// unconditionally, every caller with non-default settings — which is
/// every project carrying a `[format]` table — had to bypass it, so the
/// one path that formats on a keystroke was the one path that never
/// cached.
#[salsa::interned]
pub struct FormatConfig<'db> {
    #[returns(ref)]
    pub options: FormatOptions,
}

/// Salsa-tracked formatter entry point. Re-runs when
/// [`leek_parser::query::parse_query`]'s result changes — which
/// itself only re-runs when the input file's text changes — or when
/// `config` names different settings.
#[salsa::tracked]
pub fn format_query<'db>(
    db: &'db dyn leek_query::salsa::Db,
    file: leek_query::salsa::SourceFile,
    config: FormatConfig<'db>,
) -> FormatQueryResult {
    use leek_query::salsa::ProgramClasses;

    let parsed = leek_parser::query::parse_query(db, file, ProgramClasses::none(db));
    let version = version_from_byte(file.version_byte(db));
    let text = crate::format(&parsed.green, version, config.options(db));
    FormatQueryResult {
        text: Arc::new(text),
    }
}

#[cfg(test)]
mod tests {
    use leek_query::salsa::{LeekDb, SourceFile};

    use super::{FormatConfig, format_query};
    use crate::{FormatOptions, IndentStyle};

    const SRC: &str = "function f() {\nreturn 1;\n}\n";

    fn db_with(src: &str) -> (LeekDb, SourceFile) {
        let db = LeekDb::default();
        let file = SourceFile::new(
            &db,
            "/fmt-query-tests/main.leek".to_string(),
            1,
            src.into(),
            4,
            false,
            false,
            0,
        );
        (db, file)
    }

    fn format_with(db: &LeekDb, file: SourceFile, opts: FormatOptions) -> String {
        format_query(db, file, FormatConfig::new(db, opts))
            .text
            .as_ref()
            .clone()
    }

    /// The settings are part of the key, not a value the query ignores.
    ///
    /// Before they were interned in, `format_query` formatted with
    /// `FormatOptions::default()` whatever the caller asked for, so every
    /// caller with a `[format]` table had to bypass the cache entirely.
    /// A query that quietly returned default-formatted text would pass a
    /// "does it compile" check and fail every user with non-default
    /// settings, so this asserts on the output rather than the call.
    #[test]
    fn two_configs_over_one_file_format_differently() {
        let (db, file) = db_with(SRC);

        let two = FormatOptions {
            indent: 2,
            ..FormatOptions::default()
        };
        let eight = FormatOptions {
            indent: 8,
            ..FormatOptions::default()
        };

        let narrow = format_with(&db, file, two);
        let wide = format_with(&db, file, eight);

        assert!(narrow.contains("\n  return"), "2-space indent: {narrow:?}");
        assert!(
            wide.contains("\n        return"),
            "8-space indent: {wide:?}"
        );
    }

    /// Two callers that assemble equal settings share one memo: the
    /// interned key is the settings' *value*, not the caller's instance.
    #[test]
    fn equal_configs_intern_to_one_key() {
        let (db, _) = db_with(SRC);
        let a = FormatConfig::new(&db, FormatOptions::default());
        let b = FormatConfig::new(&db, FormatOptions::default());
        assert!(a == b, "equal settings intern to one handle");

        let tabs = FormatConfig::new(
            &db,
            FormatOptions {
                indent_style: IndentStyle::Tabs,
                ..FormatOptions::default()
            },
        );
        assert!(a != tabs, "different settings are a different key");
    }
}
