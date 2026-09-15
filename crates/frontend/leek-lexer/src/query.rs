//! The lexer as a tracked query.

use leek_syntax::version::version_from_byte;

use crate::LexResult;

/// Salsa-tracked entry point. Re-runs only when the input
/// [`SourceFile`](leek_query::salsa::SourceFile)'s text or version
/// byte changes.
#[salsa::tracked]
pub fn lex_query(db: &dyn leek_query::salsa::Db, file: leek_query::salsa::SourceFile) -> LexResult {
    #[cfg(test)]
    crate::salsa_probe::LEX_QUERY_CALLS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let text = file.text(db);
    let source = file.source(db);
    let version = version_from_byte(file.version_byte(db));
    crate::lex(text, source, version)
}

#[cfg(test)]
mod salsa_tests {
    use std::sync::atomic::Ordering;

    use leek_query::salsa::{LeekDb, SourceFile};
    use salsa::Setter;

    use super::lex_query;
    use crate::salsa_probe::{LEX_QUERY_CALLS, SERIAL};

    /// Build a `SourceFile` from a text snippet at a fixed source id.
    fn source(db: &mut LeekDb, text: &str) -> SourceFile {
        SourceFile::new(db, String::new(), 1, text.into(), 4, false, false, 0)
    }

    #[test]
    fn identical_inputs_hit_cache() {
        let _guard = SERIAL.lock().unwrap();
        let mut db = LeekDb::default();
        let file = source(&mut db, "var x = 5;");

        let before = LEX_QUERY_CALLS.load(Ordering::Relaxed);
        let first = lex_query(&db, file);
        let after_first = LEX_QUERY_CALLS.load(Ordering::Relaxed);
        let second = lex_query(&db, file);
        let after_second = LEX_QUERY_CALLS.load(Ordering::Relaxed);

        assert_eq!(
            after_first - before,
            1,
            "first call should execute the query"
        );
        assert_eq!(
            after_second - after_first,
            0,
            "second identical call should hit the salsa cache"
        );
        assert_eq!(first.tokens, second.tokens);
    }

    #[test]
    fn changing_text_reruns_query() {
        let _guard = SERIAL.lock().unwrap();
        let mut db = LeekDb::default();
        let file = source(&mut db, "var x = 5;");

        let before = LEX_QUERY_CALLS.load(Ordering::Relaxed);
        let _ = lex_query(&db, file);
        let after_first = LEX_QUERY_CALLS.load(Ordering::Relaxed);

        // Mutating the salsa input invalidates any tracked-query
        // result that read it.
        file.set_text(&mut db).to("var y = 6;".into());

        let _ = lex_query(&db, file);
        let after_second = LEX_QUERY_CALLS.load(Ordering::Relaxed);

        assert_eq!(after_first - before, 1);
        assert_eq!(
            after_second - after_first,
            1,
            "changing input text must re-execute the query"
        );
    }
}
