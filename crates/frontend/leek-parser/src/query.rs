//! The parser as a tracked query.
//!
//! Two parse paths exist — the pure [`parse_file_with`] entry point and
//! [`parse_query`] — and they differ only in *who lexed the text*, never
//! in how the resulting diagnostics are ordered. A path that lexes its
//! own text reports the lexer's diagnostics ahead of the parser's; a path
//! handed tokens somebody else lexed reports the parser's only, because
//! that somebody already emitted the lexer's. The [entry module
//! docs](crate::entry) state the convention in full.

use leek_diagnostics::Diagnostic;
use leek_query::salsa::ProgramClasses;
use leek_syntax::language::GreenNode;
use leek_syntax::version::version_from_byte;

use crate::parse_tokens_with_classes;

/// The parse entry points live in the crate's `entry` module and are
/// re-exported here so importers of `leek_parser::query::parse_file*`
/// keep compiling. New callers should use [`parse_file_with`], which takes
/// its version, [`ParseFeatures`](crate::ParseFeatures) and class names in
/// a [`ParseOptions`] instead of defaulting them off the environment; the
/// two `parse_file*` shims are deprecated for exactly that reason.
pub use crate::entry::{ParseOptions, ParsedFile, parse_file_with};
#[expect(
    deprecated,
    reason = "re-exporting the deprecation shims is the point of this statement"
)]
pub use crate::entry::{parse_file, parse_file_with_classes};

/// Tracked return value for [`parse_query`]: the green tree plus the
/// parser's own diagnostics. Lex diagnostics are not in here — a caller
/// assembling a stream reports
/// [`lex_query`](leek_lexer::query::lex_query)'s first, which is what
/// `leek_db::queries::file_diagnostics_upto` does.
#[derive(salsa::Update, Debug, Clone, PartialEq, Eq)]
pub struct ParseQueryResult {
    pub green: GreenNode,
    pub diagnostics: Vec<Diagnostic>,
}

/// The one parse entry point every driver reaches the tree through.
///
/// Re-runs when the upstream
/// [`lex_query`](leek_lexer::query::lex_query) result changes, when
/// any input field this body reads off the
/// [`SourceFile`](leek_query::salsa::SourceFile) changes (`text`,
/// `version_byte`, `flags_bits`; `strict` is *not* read here — it only
/// reaches the type checker), or when it is asked for a different
/// [`ProgramClasses`] set.
///
/// `classes` is a **key**, not a field on the file. The program-wide
/// defined-class set belongs to the program, so one leaf has one parse
/// per program that includes it, and an edit that leaves a program's
/// class set alone re-parses only the file that changed. Assembling the
/// set is `leek_db::queries::program_classes`; a file parsed on its own
/// passes [`ProgramClasses::none`].
///
/// Exactly which of those changes re-run this query is pinned by
/// `program_queries.rs` in `leek-db`'s tests, which watches salsa's
/// event stream: the contract now spans an input, a key and two crates,
/// so it is no longer something this crate can test on its own.
///
/// Deliberately *not* [`crate::parse_file_with`]:
///
/// * it lexes through [`leek_lexer::query::lex_query`], so the
///   memoized lex is shared with every other query over the same file
///   rather than repeated here;
/// * it returns the parser's diagnostics *only*, because a caller
///   assembling a stream already reported the lexer's.
///   `parse_file_with` prepends them, so routing this query through it
///   would double-report every lex diagnostic.
///
/// It serves an indexed on-disk file exactly as it serves an editor
/// buffer: both are one
/// [`SourceFile`](leek_query::salsa::SourceFile).
#[salsa::tracked]
pub fn parse_query<'db>(
    db: &'db dyn leek_query::salsa::Db,
    file: leek_query::salsa::SourceFile,
    classes: ProgramClasses<'db>,
) -> ParseQueryResult {
    let lex = leek_lexer::query::lex_query(db, file);
    let text = file.text(db);
    let source = file.source(db);
    let version = version_from_byte(file.version_byte(db));
    let features =
        crate::ParseFeatures::from(leek_span::FeatureFlags::from_bits(file.flags_bits(db)));
    let result = parse_tokens_with_classes(
        text,
        source,
        &lex.tokens,
        version,
        features,
        classes.names(db),
    );
    ParseQueryResult {
        green: result.green,
        diagnostics: result.diagnostics,
    }
}

/// The two parse paths agree on the tree, and disagree about lex
/// diagnostics only where the [entry module docs](crate::entry) say they
/// should.
#[cfg(test)]
mod parse_path_agreement_tests {
    use leek_diagnostics::codes;
    use leek_query::salsa::{LeekDb, ProgramClasses, SourceFile};
    use leek_syntax::version::version_from_byte;

    use super::parse_query;

    /// A clean file, and one that needs a program class set to parse its
    /// declaration *and* trips the parser on a second statement — so the
    /// comparison covers both a green tree built from cross-file inputs
    /// and one built through error recovery.
    const CASES: [(&str, &[&str]); 2] = [
        ("class c {}\nc x = new c();\n", &[]),
        ("fromAnotherFile y = 1;\nvar = ;\n", &["fromAnotherFile"]),
    ];

    /// `parse_file_with` and `parse_query` are two doors onto one grammar:
    /// the LSP reaches the tree through the query and `leekc` through the
    /// pure entry point, and a user who saw different syntax errors from
    /// the editor and the compiler would rightly call it a bug.
    #[test]
    fn the_pure_entry_point_and_the_tracked_query_build_the_same_tree() {
        for (text, classes) in CASES {
            let classes: Vec<String> = classes.iter().map(|&c| c.to_string()).collect();
            let db = LeekDb::default();
            let file = SourceFile::new(&db, String::new(), 1, text.into(), 4, false, false, 0);
            let tracked = parse_query(&db, file, ProgramClasses::new(&db, classes.clone()));
            let pure = crate::parse_file_with(
                text,
                file.source(&db),
                &crate::ParseOptions::new(version_from_byte(4)).with_extra_classes(&classes),
            );
            assert_eq!(
                tracked.green, pure.green,
                "same inputs, same tree: {text:?}"
            );
            // Neither fixture lexes badly, so the one documented
            // difference between the two paths doesn't show here.
            assert_eq!(
                tracked.diagnostics, pure.diagnostics,
                "no lex diagnostics to disagree about: {text:?}"
            );
        }
    }

    /// An indexed on-disk file used to have a parse query of its own
    /// that re-lexed and merged the lexer's diagnostics in, because
    /// nothing else reported them for a file with no editor buffer. It is
    /// gone, and this is the guard on what replaced it: the indexed file
    /// is an ordinary [`SourceFile`], its lex diagnostics come from
    /// `lex_query`, and `parse_query` reports none of them — so a stream
    /// that concatenates the two has each exactly once.
    #[test]
    fn the_indexed_path_still_reports_each_lex_diagnostic_once() {
        let text = "var s = \"unclosed;\n";
        let db = LeekDb::default();
        let file = SourceFile::new(
            &db,
            "/project/a.leek".to_string(),
            1,
            text.into(),
            4,
            false,
            false,
            0,
        );
        let lexed = leek_lexer::query::lex_query(&db, file);
        let parsed = parse_query(&db, file, ProgramClasses::none(&db));
        let stream: Vec<_> = lexed
            .diagnostics
            .iter()
            .chain(&parsed.diagnostics)
            .filter(|d| d.code == codes::STRING_NOT_CLOSED)
            .collect();
        assert_eq!(
            stream.len(),
            1,
            "the lex query reports it once, and the parse query not at all"
        );
    }

    /// The counterpart: `parse_query` leaves lex diagnostics to
    /// `lex_query`, so it must *not* report them itself. Were it routed
    /// through `parse_file_with` every stream would show each of them
    /// twice.
    #[test]
    fn the_buffer_query_leaves_lex_diagnostics_to_the_lex_step() {
        let text = "var s = \"unclosed;\n";
        let db = LeekDb::default();
        let file = SourceFile::new(&db, String::new(), 1, text.into(), 4, false, false, 0);
        let out = parse_query(&db, file, ProgramClasses::none(&db));
        assert!(
            !out.diagnostics
                .iter()
                .any(|d| d.code == codes::STRING_NOT_CLOSED),
            "the lex query owns this one: {:?}",
            out.diagnostics
        );
    }
}
