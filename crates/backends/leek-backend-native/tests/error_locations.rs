//! Native failures point at source (NATIVE-15 / #173).
//!
//! Every assertion here is on the *rendered* diagnostic, not on the error
//! value: "the error carries a span" is only worth anything if the span
//! survives into what the user is shown. A construct outside the native
//! subset used to reach the user as `error: unsupported: switch on real` —
//! true, and useless on a 400-line AI.

use leek_backend_native::{NativeError, NativeErrorKind, NativeOptions, compile_program, run};
use leek_diagnostics::{Renderer, Sources};
use leek_hir::lower_file_versioned;
use leek_parser::{
    ast::{AstNode, SourceFile},
    parse_with_features,
};
use leek_span::SourceId;
use leek_syntax::{SyntaxNode, Version};

fn source() -> SourceId {
    SourceId::new(1).unwrap()
}

fn hir(src: &str, version: u8) -> leek_hir::HirFile {
    let syntax = match version {
        1 => Version::V1,
        _ => Version::V4,
    };
    let parsed = parse_with_features(src, source(), syntax, leek_parser::ParseFeatures::default());
    let file = SourceFile::cast(SyntaxNode::new_root(parsed.green.clone())).expect("parse");
    lower_file_versioned(&file, source(), version).0
}

/// The error from compiling `src`, which the test expects to fail.
fn compile_err(src: &str, version: u8) -> NativeError {
    let opts = NativeOptions::debug().with_lang(version, false);
    compile_program(&hir(src, version), &opts).expect_err("outside the native subset")
}

/// What `miku run` prints for this error.
fn rendered(err: &NativeError, src: &str) -> String {
    let sources = Sources::single(source(), "main.leek", src);
    err.diagnostics()
        .into_iter()
        .map(|d| Renderer::default().render(&d, &sources))
        .collect()
}

#[test]
fn an_unsupported_construct_renders_with_its_own_line_and_caret() {
    // `2 ** b` with a non-constant exponent is outside the subset; it is on
    // line 3.
    let src = "var b = 3\nvar out = 0\nout = 2 ** b\nreturn out\n";
    let err = compile_err(src, 4);
    assert_eq!(err.kind, NativeErrorKind::Unsupported);
    let span = err.span.expect("the statement span is carried");
    let table = leek_span::LineTable::new(src);
    assert_eq!(table.line_col(span.start).line, 3, "span is on line 3");

    let out = rendered(&err, src);
    assert!(out.contains("error[E0600]"), "{out}");
    assert!(out.contains("--> main.leek:3:"), "{out}");
    assert!(out.contains("out = 2 ** b"), "{out}");
    assert!(out.contains('^'), "{out}");
}

#[test]
fn unsupported_inside_a_named_function_names_the_function() {
    let src = "function helper(b) {\n  return 2 ** b\n}\nreturn helper(3)\n";
    let err = compile_err(src, 4);
    assert_eq!(err.function.as_deref(), Some("helper"), "{err:?}");
    let out = rendered(&err, src);
    assert!(out.contains("while compiling `helper`"), "{out}");
    assert!(out.contains("--> main.leek:2:"), "{out}");
}

#[test]
fn the_cell_semantics_gate_points_at_the_by_ref_parameter() {
    // A v1 `@x` parameter passed onward to another user function: the *whole
    // program* is rejected, so the message has to say which parameter did it.
    let src = "function p(@x) { q(x) }\nfunction q(y) { return y }\nvar n = 5\np(n)\nreturn n\n";
    let err = compile_err(src, 1);
    assert_eq!(err.kind, NativeErrorKind::Unsupported);
    assert!(err.span.is_some(), "the gate names a location: {err:?}");
    let out = rendered(&err, src);
    assert!(out.contains("by-reference parameter"), "{out}");
    assert!(out.contains("--> main.leek:1:"), "{out}");
    assert!(
        out.contains("while compiling `p`"),
        "the offending function is named:\n{out}"
    );
}

/// A net against a future span-less `unsupported` site: whatever the backend
/// declines in these fixtures, it declines *somewhere*.
#[test]
fn every_unsupported_from_a_translation_path_carries_a_span() {
    let fixtures = [
        "var b = 3\nreturn 2 ** b\n",
        "function p(@x) { q(x) }\nfunction q(y) { return y }\nvar n = 5\np(n)\nreturn n\n",
        "class A { m(@x) { x = 9 } }\nvar o = new A()\nvar n = 5\no.m(n)\nreturn n\n",
    ];
    for src in fixtures {
        let opts = NativeOptions::debug().with_lang(1, false);
        let Err(err) = compile_program(&hir(src, 1), &opts) else {
            continue; // supported after all — nothing to assert
        };
        if err.kind != NativeErrorKind::Unsupported {
            continue;
        }
        assert!(
            err.span.is_some() || err.function.is_some(),
            "span-less and anonymous unsupported for:\n{src}\n{err:?}"
        );
    }
}

#[test]
fn a_runtime_trap_keeps_the_bare_fight_error_code() {
    // The generator logs this string verbatim as the fight's error key, so it
    // must stay unprefixed and unadorned however much location the other
    // kinds grow.
    let src = "var a = 0 for (var i = 0; i < 100000000; ++i) a = a + 1 return a";
    let opts = NativeOptions::release()
        .with_lang(4, false)
        .with_op_limit(10_000);
    let err = run(&hir(src, 4), &opts).expect_err("the budget trips");
    assert_eq!(err.runtime_code(), Some("TOO_MUCH_OPERATIONS"));
    assert_eq!(err.reason(), "TOO_MUCH_OPERATIONS");
    assert_eq!(err.to_string(), "runtime error: TOO_MUCH_OPERATIONS");
}

#[test]
fn a_mir_lowering_failure_keeps_every_diagnostic() {
    // Constructed directly: what matters is that the carrier does not collapse
    // a set of lowering diagnostics down to its first message.
    let a = leek_diagnostics::Diagnostic::error(
        leek_diagnostics::codes::LOWERING_UNSUPPORTED,
        leek_span::Span::new(source(), 0, 3),
        "first problem",
    );
    let b = leek_diagnostics::Diagnostic::error(
        leek_diagnostics::codes::LOWERING_UNSUPPORTED,
        leek_span::Span::new(source(), 10, 13),
        "second problem",
    );
    let err = NativeError::compile("MIR lowering failed: first problem")
        .at(a.span)
        .with_diagnostics(vec![a, b]);

    let out = rendered(&err, "abc\ndef\nghi\n");
    assert!(out.contains("first problem"), "{out}");
    assert!(out.contains("second problem"), "{out}");
    assert_eq!(
        out.matches("-->").count(),
        2,
        "each keeps its own span:\n{out}"
    );
}

#[test]
fn display_stays_byte_identical() {
    // Fight logs use this string as an `AI_INTERRUPTED` parameter and the
    // corpus runner buckets skips on it, so growing a location into it would
    // move both. The location lives in the diagnostic instead.
    let span = leek_span::Span::new(source(), 4, 5);
    let err = NativeError::unsupported("switch on real")
        .at(span)
        .in_fn("helper");
    assert_eq!(err.to_string(), "unsupported: switch on real");
    assert_eq!(
        NativeError::compile("boom").to_string(),
        "compile error: boom"
    );
    assert_eq!(
        NativeError::runtime("TOO_MUCH_OPERATIONS").to_string(),
        "runtime error: TOO_MUCH_OPERATIONS"
    );
}

#[test]
fn or_span_does_not_overwrite_a_statement_span() {
    let stmt = leek_span::Span::new(source(), 10, 12);
    let decl = leek_span::Span::new(source(), 0, 1);
    let err = NativeError::unsupported("x").at(stmt).or_span(decl);
    assert_eq!(err.span, Some(stmt));
    let err = NativeError::unsupported("x").or_span(decl);
    assert_eq!(err.span, Some(decl));
}
