//! A bad `Miku.toml` must point at the key or value that is bad.
//!
//! Every case here asserts on the *text the span covers*, sliced out of the
//! source the parser was given. A span that drifts to the wrong key, or
//! collapses to `0..0` the way the old fabricated one did, fails here — which
//! is the point: nothing else in the suite can tell a right caret from a
//! wrong one.

use leek_diagnostics::{IntoDiagnostic, Severity};
use leek_manifest::{ManifestErrorKind, ManifestWarningKind};

/// The source text the error's span covers.
fn error_text(src: &str) -> String {
    let err = leek_manifest::load_str(src).expect_err("should not parse");
    let span = err
        .span
        .unwrap_or_else(|| panic!("{:?} carries no span", err.kind));
    src[span.start as usize..span.end as usize].to_string()
}

/// The source text the first warning's span covers.
fn warning_text(src: &str) -> String {
    let (_, warnings) = leek_manifest::load_str(src).expect("should parse");
    let w = warnings.first().expect("expected a warning");
    let span = w
        .span
        .unwrap_or_else(|| panic!("{:?} carries no span", w.kind));
    src[span.start as usize..span.end as usize].to_string()
}

const HEAD: &str = "[project]\nname = \"demo\"\nversion = \"0.1.0\"\n";

#[test]
fn an_unknown_top_level_key_points_at_the_key() {
    let src = format!("{HEAD}\n[moonbeam]\nx = 1\n");
    assert_eq!(error_text(&src), "moonbeam");
}

#[test]
fn an_unknown_field_points_at_the_field() {
    let src = format!("{HEAD}\n[fight]\nreports_dir = \"out\"\nworker_count = 4\n");
    assert_eq!(warning_text(&src), "worker_count");
}

#[test]
fn a_deferred_table_points_at_the_table_name() {
    let src = format!("{HEAD}\n[workspace]\nmembers = [\"a\"]\n");
    assert_eq!(warning_text(&src), "workspace");
}

#[test]
fn a_wrongly_typed_path_points_at_the_value() {
    let src = format!("{HEAD}\n[paths]\nbuild = 3\n");
    assert_eq!(error_text(&src), "3");
}

#[test]
fn a_rejected_path_points_at_the_value() {
    let src = format!("{HEAD}\n[paths]\nbuild = \"../elsewhere\"\n");
    assert_eq!(error_text(&src), "\"../elsewhere\"");
}

#[test]
fn a_non_integer_jobs_points_at_the_value() {
    let src = format!("{HEAD}\n[fight]\njobs = \"eight\"\n");
    assert_eq!(error_text(&src), "\"eight\"");
}

#[test]
fn an_out_of_range_language_points_at_the_value() {
    let src = "[project]\nname = \"demo\"\nversion = \"0.1.0\"\nlanguage = 9\n";
    assert_eq!(error_text(src), "9");
}

#[test]
fn a_bad_format_enum_points_at_the_value() {
    let src = format!("{HEAD}\n[format]\nindent_style = \"wide\"\n");
    assert_eq!(error_text(&src), "\"wide\"");
}

#[test]
fn a_bad_backend_mode_points_at_the_value() {
    let src = format!("{HEAD}\n[backend.java]\nmode = \"sloppy\"\n");
    assert_eq!(error_text(&src), "\"sloppy\"");
}

#[test]
fn a_bad_lint_entry_keeps_the_element_span() {
    let src = format!("{HEAD}\n[lint]\ndeny = [\"E0240\", \"NOPE9999\"]\n");
    let (manifest, _) = leek_manifest::load_str(&src).expect("should parse");
    let span = manifest
        .lint
        .span_for("NOPE9999")
        .expect("the array element has a span");
    assert_eq!(&src[span.start as usize..span.end as usize], "\"NOPE9999\"");
}

#[test]
fn a_missing_project_table_has_no_span_and_does_not_invent_one() {
    // Nothing in the document to point at, so the error is honestly detached
    // rather than pointing at byte 0 of some unrelated file.
    let err = leek_manifest::load_str("").expect_err("should not parse");
    assert!(matches!(
        err.kind,
        ManifestErrorKind::MissingTable { name: "project" }
    ));
    assert!(err.span.is_none());
}

#[test]
fn a_dotted_key_still_spans_its_value() {
    // Dotted keys synthesize their parent table, which has no span of its
    // own. The value does, so the diagnostic is not location-less.
    let src = "project.name = \"demo\"\nproject.version = \"0.1.0\"\nfight.jobs = 0\n";
    assert_eq!(error_text(src), "0");
}

#[test]
fn the_diagnostic_lands_in_the_manifest_source_not_the_entry_file() {
    // The regression this whole change exists for: manifest errors used to be
    // built at `SourceId(1)`, which is the entry `.leek` file, so a bad
    // `Miku.toml` rendered a caret at byte 0 of `main.leek`.
    let src = format!("{HEAD}\n[moonbeam]\nx = 1\n");
    let err = leek_manifest::load_str(&src).expect_err("should not parse");
    let diag = err.into_diagnostic();
    assert_eq!(diag.span.source, leek_span::Span::MANIFEST_SOURCE);
    assert_ne!(diag.span.source, leek_span::SourceId::new(1).unwrap());
    assert_ne!(diag.span.start, diag.span.end, "a real, non-empty span");
    assert_eq!(diag.code.id(), "E0401");
}

#[test]
fn warnings_and_errors_carry_distinguishable_codes() {
    let src = format!("{HEAD}\n[fight]\nworker_count = 4\n");
    let (_, warnings) = leek_manifest::load_str(&src).expect("should parse");
    let diag = warnings[0].clone().into_diagnostic();
    assert_eq!(diag.code.id(), "W0400");
    assert_eq!(diag.severity, Severity::Warning);
    assert!(matches!(
        warnings[0].kind,
        ManifestWarningKind::UnknownField { .. }
    ));
}
