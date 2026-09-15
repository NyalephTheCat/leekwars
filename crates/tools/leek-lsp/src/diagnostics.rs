//! The published diagnostic set, and its conversion to `lsp_types`.

use leek_diagnostics::{Diagnostic as LeekDiagnostic, Severity};
use leek_lint::LintGroups;
use leek_lint::pipeline::program_diagnostics_with_lints;
use leek_pipeline::salsa::SourceFile;
use leek_span::SourceId;
use leek_syntax::pipeline::version_from_byte;
use tower_lsp::lsp_types as lsp;

use crate::util::position::PosMap;
use crate::workspace::Workspace;

/// The `source` field we stamp on every diagnostic we publish. Handlers
/// use it to tell our own diagnostics apart from other servers' when a
/// client hands some back (see `code_action`).
pub const SOURCE: &str = "leek";

/// This repository's web address, read from the manifest rather than written
/// out here.
///
/// `leek-lsp`'s `[package]` inherits `repository` from `[workspace.package]`
/// precisely so this is non-empty. A literal is what made every "explain"
/// link in the Problems panel 404: it named a repository this code has not
/// lived in for a long time, and nothing could notice.
const REPOSITORY: &str = env!("CARGO_PKG_REPOSITORY");

/// Directory, relative to the repository root, holding the extended
/// write-ups. `miku explain` prints the same files from the copy
/// `leek-diagnostics` embeds at build time.
const EXPLAIN_DIR: &str = "crates/core/leek-diagnostics/explain";

/// Web address of the extended write-up for diagnostic `code`.
///
/// Only meaningful for a code that has one ([`leek_diagnostics::Code::explain`]
/// returns `Some`); nothing here checks, and a code without a write-up would
/// get a link to a file that does not exist.
fn explain_href(code: &str) -> String {
    format!("{REPOSITORY}/blob/main/{EXPLAIN_DIR}/{code}.md")
}

/// The diagnostic set for one file — the single source of truth behind
/// push (`publishDiagnostics`), pull (`textDocument/diagnostic`) and
/// `textDocument/codeAction`.
///
/// Analysis covers the whole include closure — lint findings included —
/// and the result is filtered to spans belonging to `source_file` itself:
/// a diagnostic raised inside an include belongs to that include's own
/// document.
///
/// Handlers must not re-derive this set. A per-file run reports
/// undefined-symbol and type errors for everything an include provides,
/// so code actions built on one offer quick fixes — and let
/// `source.fixAll` apply them on save — for problems the editor never
/// showed.
///
/// One tracked query, not a planned pipeline: every stage underneath is
/// memoized, so the handlers serving one edit share the work and only the
/// returned `Vec` is freshly allocated. The closure it walks is
/// [`WorkspaceFiles`](leek_db::WorkspaceFiles), which
/// [`Workspace::resync`](crate::workspace::Workspace) keeps closed under
/// includes — without that the query layer would see a smaller closure
/// than the include folder does and quietly drop an unindexed include's
/// diagnostics.
///
/// `LintGroups::default()` matches what `leek_session::lsp_params()` asked
/// for: the opt-in groups stay off until the server reads them from the
/// manifest.
pub fn file_diagnostics(ws: &Workspace, source_file: SourceFile) -> Vec<LeekDiagnostic> {
    leek_db::queries::for_source(&program_stream(ws, source_file), source_file.source(&ws.db))
}

/// The whole program's stream, before it is narrowed to one file.
///
/// Split out so a test can tell "the include's diagnostic never reached
/// us" apart from "it reached us and the filter dropped it" — two very
/// different bugs that look identical from `file_diagnostics` alone.
pub(crate) fn program_stream(
    ws: &Workspace,
    source_file: SourceFile,
) -> std::sync::Arc<Vec<LeekDiagnostic>> {
    program_diagnostics_with_lints(
        &ws.db,
        ws.files(),
        source_file,
        version_from_byte(source_file.version_byte(&ws.db)),
        LintGroups::default(),
    )
}

/// Every file a label may point into, by `SourceId`.
///
/// A diagnostic's labels are not confined to the document it was raised in
/// — a "previously declared here" label can name a symbol in an included
/// file — so each one needs *its* file's URI and *its* file's text to
/// convert a byte span to a UTF-16 range. Anchoring them all to the open
/// document sent the client to a line number from the wrong file.
#[derive(Default)]
pub struct LabelSources<'a> {
    rows: Vec<(SourceId, &'a lsp::Url, PosMap<'a>)>,
}

impl<'a> LabelSources<'a> {
    /// Every document the workspace analyses — open buffers and indexed
    /// project files alike, which is exactly the include closure the
    /// diagnostics were produced from.
    #[must_use]
    pub fn from_workspace(ws: &'a Workspace) -> Self {
        let mut rows = Vec::new();
        for target in ws.analysis_targets() {
            // `PosMap::new` from the target's own borrows, not
            // `target.pos_map()`, which would borrow the loop temporary.
            rows.push((
                target.source_file.source(&ws.db),
                &target.uri,
                PosMap::new(&target.line_table, &target.text),
            ));
        }
        Self { rows }
    }

    pub fn push(&mut self, source: SourceId, uri: &'a lsp::Url, pm: PosMap<'a>) {
        self.rows.push((source, uri, pm));
    }

    fn get(&self, source: SourceId) -> Option<(&'a lsp::Url, PosMap<'a>)> {
        self.rows
            .iter()
            .find(|(id, _, _)| *id == source)
            .map(|(_, uri, pm)| (*uri, *pm))
    }
}

/// Map a Leek diagnostic to the LSP wire shape, including catalog
/// metadata and secondary labels as `relatedInformation`.
///
/// `pm`/`uri` describe the document the diagnostic's *primary* span belongs
/// to; each label is resolved through `labels` instead, and one whose file
/// the workspace cannot name is dropped rather than pinned to `uri` at a
/// range computed from the wrong text.
pub fn to_lsp(
    diag: &LeekDiagnostic,
    pm: PosMap<'_>,
    uri: Option<&lsp::Url>,
    labels: &LabelSources<'_>,
) -> lsp::Diagnostic {
    // Link the code to its extended write-up when one exists — the
    // `explain/<ID>.md` file the build embeds (and `miku explain`
    // prints). Codes without a write-up get no link.
    let code_description = diag
        .code
        .explain()
        .and_then(|_| lsp::Url::parse(&explain_href(diag.code.id())).ok())
        .map(|href| lsp::CodeDescription { href });

    let related_information = uri.and_then(|doc_uri| {
        if diag.labels.is_empty() {
            return None;
        }
        let related: Vec<lsp::DiagnosticRelatedInformation> = diag
            .labels
            .iter()
            .filter_map(|label| {
                // The open document is the answer only when the label
                // really belongs to it.
                let (label_uri, label_pm) = labels
                    .get(label.span.source)
                    .or_else(|| (label.span.source == diag.span.source).then_some((doc_uri, pm)))?;
                Some(lsp::DiagnosticRelatedInformation {
                    location: lsp::Location {
                        uri: label_uri.clone(),
                        range: label_pm.span_range(label.span),
                    },
                    message: label.message.clone(),
                })
            })
            .collect();
        if related.len() != diag.labels.len() {
            tracing::debug!(
                dropped = diag.labels.len() - related.len(),
                code = diag.code.id(),
                "dropped label(s) with no known source file"
            );
        }
        (!related.is_empty()).then_some(related)
    });

    lsp::Diagnostic {
        range: pm.span_range(diag.span),
        severity: Some(match diag.severity {
            Severity::Error => lsp::DiagnosticSeverity::ERROR,
            Severity::Warning => lsp::DiagnosticSeverity::WARNING,
            Severity::Info => lsp::DiagnosticSeverity::INFORMATION,
            Severity::Hint => lsp::DiagnosticSeverity::HINT,
        }),
        code: Some(lsp::NumberOrString::String(diag.code.id().to_string())),
        code_description,
        source: Some(SOURCE.into()),
        message: diag.message.clone(),
        related_information,
        tags: None,
        data: None,
    }
}

#[cfg(test)]
mod tests {
    use super::{EXPLAIN_DIR, LabelSources, explain_href, file_diagnostics, to_lsp};
    use crate::util::position::PosMap;
    use crate::workspace::Workspace;
    use leek_diagnostics::{Code, Diagnostic};
    use leek_span::{LineTable, SourceId, Span};
    use std::fs;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};
    use tower_lsp::lsp_types as lsp;

    fn url(name: &str) -> lsp::Url {
        lsp::Url::parse(&format!("file:///tmp/{name}")).unwrap()
    }

    fn temp_root() -> PathBuf {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let suffix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock should be after the Unix epoch")
            .as_nanos();
        let seq = NEXT.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!(
            "leek-lsp-diagnostics-{}-{suffix}-{seq}",
            std::process::id()
        ))
    }

    fn codes(diagnostics: &[Diagnostic]) -> Vec<&'static str> {
        diagnostics.iter().map(|d| d.code.id()).collect()
    }

    /// The whole point of analysing the closure rather than the file: a
    /// function an include provides is *defined*, so the entry does not
    /// report it undefined.
    #[test]
    fn a_symbol_an_include_provides_is_not_reported_undefined() {
        let root = temp_root();
        fs::create_dir_all(&root).expect("create project");
        fs::write(
            root.join("main.leek"),
            "include(\"util\")\nreturn helper();\n",
        )
        .expect("write entry");
        fs::write(
            root.join("util.leek"),
            "function helper() {\n\treturn 1;\n}\n",
        )
        .expect("write include");

        let mut ws = Workspace::default();
        ws.index_project_at(&root);
        let path = root.join("main.leek").canonicalize().expect("canonical");
        let entry = ws.indexed[&path].source_file;

        let diagnostics = file_diagnostics(&ws, entry);
        fs::remove_dir_all(&root).ok();

        assert!(
            !codes(&diagnostics).iter().any(|c| c.starts_with("E02")),
            "the closure declares `helper`: {diagnostics:?}"
        );
    }

    /// An include that no project index ever saw still contributes its
    /// diagnostics.
    ///
    /// This is the regression guard for the file set. The query layer
    /// resolves an include name against `WorkspaceFiles` and has no disk
    /// fallback, so an include reaching outside the indexed project is
    /// invisible to it unless `Workspace::resync` has registered the file
    /// as an input. When it is not, this file's errors simply vanish —
    /// and a document with no diagnostics looks exactly like a clean one,
    /// which is why this is a test and not a comment.
    #[test]
    fn an_include_outside_the_indexed_project_still_reports() {
        let base = temp_root();
        let root = base.join("project");
        let outside = base.join("shared");
        fs::create_dir_all(&root).expect("create project");
        fs::create_dir_all(&outside).expect("create shared dir");
        fs::write(
            root.join("main.leek"),
            "include(\"../shared/lib\")\nreturn 1;\n",
        )
        .expect("write entry");
        // `£` is a stray character: a lex error, from a file the index
        // never walked.
        fs::write(outside.join("lib.leek"), "var bad = \u{a3};\n").expect("write include");

        let mut ws = Workspace::default();
        ws.index_project_at(&root);
        let path = root.join("main.leek").canonicalize().expect("canonical");
        let entry = ws.indexed[&path].source_file;

        let all = crate::diagnostics::program_stream(&ws, entry);
        let own = file_diagnostics(&ws, entry);
        fs::remove_dir_all(&base).ok();

        assert!(
            codes(&all).contains(&"E0003"),
            "the unindexed include's lex error reaches the program stream: {all:?}"
        );
        assert!(
            !codes(&own).contains(&"E0003"),
            "and is filtered out of the entry's own slice: {own:?}"
        );
    }

    /// A label pointing into an included file must carry that file's URI
    /// and a range converted against *its* text — not the open document's.
    #[test]
    fn a_label_in_an_include_gets_the_includes_uri_and_range() {
        let main_text = "include('inc')\nvar x = 1\n";
        let inc_text = "// a much longer first line in the include\nvar x = 2\n";
        let (main_lt, inc_lt) = (LineTable::new(main_text), LineTable::new(inc_text));
        let (main_uri, inc_uri) = (url("main.leek"), url("inc.leek"));
        let (main_id, inc_id) = (SourceId::new(1).unwrap(), SourceId::new(2).unwrap());

        let mut labels = LabelSources::default();
        labels.push(main_id, &main_uri, PosMap::new(&main_lt, main_text));
        labels.push(inc_id, &inc_uri, PosMap::new(&inc_lt, inc_text));

        let at_inc_x = leek_span::offset(inc_text.rfind('x').unwrap());
        let diag = Diagnostic::error(Code("E0200"), Span::new(main_id, 19, 20), "redeclared")
            .with_label(Span::new(inc_id, at_inc_x, at_inc_x + 1), "first here");

        let out = to_lsp(
            &diag,
            PosMap::new(&main_lt, main_text),
            Some(&main_uri),
            &labels,
        );
        let related = out.related_information.expect("related information");
        assert_eq!(related[0].location.uri, inc_uri);
        assert_eq!(related[0].location.range.start.line, 1);
        assert_eq!(related[0].location.range.start.character, 4);
    }

    /// A label whose file the workspace cannot name is dropped: a wrong
    /// location sends the editor somewhere real and wrong, which is worse
    /// than no location at all.
    #[test]
    fn a_label_with_an_unknown_source_is_dropped_not_reanchored() {
        let main_text = "var x = 1\n";
        let main_lt = LineTable::new(main_text);
        let main_uri = url("main.leek");
        let main_id = SourceId::new(1).unwrap();
        let mut labels = LabelSources::default();
        labels.push(main_id, &main_uri, PosMap::new(&main_lt, main_text));

        let diag = Diagnostic::error(Code("E0200"), Span::new(main_id, 4, 5), "bad")
            .with_label(Span::new(SourceId::new(9).unwrap(), 0, 1), "elsewhere");
        let out = to_lsp(
            &diag,
            PosMap::new(&main_lt, main_text),
            Some(&main_uri),
            &labels,
        );
        assert!(out.related_information.is_none());
    }

    /// The "explain" link an editor opens has to be a real address on the
    /// real forge. It used to name `chloe/leekscript-rs`, so every link in
    /// the Problems panel 404'd and nothing said so.
    #[test]
    fn the_explain_link_points_at_this_repository_on_github() {
        let url = lsp::Url::parse(&explain_href("E0001")).expect("a parseable URL");
        assert_eq!(url.scheme(), "https");
        assert_eq!(url.host_str(), Some("github.com"));
        assert_eq!(
            url.path(),
            format!("/NyalephTheCat/leekwars/blob/main/{EXPLAIN_DIR}/E0001.md"),
        );
    }

    /// The other half of the link, which the URL shape alone cannot pin:
    /// [`EXPLAIN_DIR`] must still be where the write-ups live. Moving them
    /// without editing the constant would leave a URL that parses and 404s.
    #[test]
    fn the_explain_dir_is_where_the_write_ups_actually_live() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../..")
            .canonicalize()
            .expect("the workspace root is three levels above this crate");
        let file = root.join(EXPLAIN_DIR).join("E0001.md");
        assert!(
            file.is_file(),
            "{} does not exist; EXPLAIN_DIR is stale",
            file.display(),
        );
    }

    /// A code with a write-up gets the link; one without gets none, so the
    /// editor never offers an "explain" that leads nowhere.
    #[test]
    fn only_codes_with_a_write_up_carry_a_code_description() {
        let text = "var x = 1\n";
        let lt = LineTable::new(text);
        let id = SourceId::new(1).unwrap();
        let uri = url("main.leek");
        let convert = |code: Code| {
            to_lsp(
                &Diagnostic::error(code, Span::new(id, 0, 3), "boom"),
                PosMap::new(&lt, text),
                Some(&uri),
                &LabelSources::default(),
            )
            .code_description
        };

        let explained = Code("E0001");
        assert!(explained.explain().is_some(), "E0001 has a write-up");
        let href = convert(explained)
            .expect("E0001 links to its write-up")
            .href;
        assert!(href.as_str().ends_with("/E0001.md"), "got {href}");

        // Not in the catalog at all, so certainly no write-up.
        let unexplained = Code("E9999");
        assert!(unexplained.explain().is_none());
        assert!(convert(unexplained).is_none());
    }
}
