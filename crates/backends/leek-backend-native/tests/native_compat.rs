//! `check_native_compat` agrees with a compile (NATIVE-15 / #173).
//!
//! The check exists so an author sees "the native backend can't run this"
//! while editing instead of when the fight starts. That is only worth
//! anything if it is *right*: a diagnostic under a construct the backend
//! actually supports is worse than no check at all, because the author
//! rewrites working code. So the first test here is the one that fails on
//! every cheap implementation — a program full of exactly the shapes a
//! module-less translation walk fabricates errors for.
//!
//! Every test in this file also exercises the "never defines a function"
//! invariant for free: `check_native_compat` drives a `CheckModule`, whose
//! `define_function` is `unreachable!`, so a regression that reintroduces
//! codegen panics here rather than quietly costing a compile per invocation.

use leek_backend_native::{NativeOptions, check_native_compat, compile_program};
use leek_diagnostics::Diagnostic;
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

fn hir(src: &str) -> leek_hir::HirFile {
    let parsed = parse_with_features(
        src,
        source(),
        Version::V4,
        leek_parser::ParseFeatures::default(),
    );
    let file = SourceFile::cast(SyntaxNode::new_root(parsed.green.clone())).expect("parse");
    lower_file_versioned(&file, source(), 4).0
}

fn opts() -> NativeOptions {
    NativeOptions::debug().with_lang(4, false)
}

fn check(src: &str) -> Vec<Diagnostic> {
    check_native_compat(&hir(src), &opts())
}

fn line_of(diag: &Diagnostic, src: &str) -> u32 {
    leek_span::LineTable::new(src)
        .line_col(diag.span.start)
        .line
}

/// The anti-fabrication test.
///
/// User calls, a composite, a string literal, a lambda and `**` are precisely
/// what fails under a `module: None` walk ("call in text-dump emit mode",
/// "runtime shim leek_… not declared"). A check built on that path reports
/// five errors here; the real one reports none, and the compile agrees.
#[test]
fn a_clean_program_reports_nothing() {
    let src = "\
function twice(n) {
  return n * 2
}
var label = \"total\"
var xs = [1, 2, 3]
var f = function(v) { return v + 1 }
var total = 0
for (var i = 0; i < count(xs); i++) {
  total = total + twice(f(xs[i]))
}
var m = [label : total]
return m[label] + 2 ** 3
";
    let diags = check(src);
    assert!(
        diags.is_empty(),
        "a compilable program must produce no compat diagnostics, got: {:#?}",
        diags.iter().map(|d| &d.message).collect::<Vec<_>>()
    );
    // …and the reason it must be empty: this program really does compile.
    assert!(compile_program(&hir(src), &opts()).is_ok());
}

#[test]
fn an_unsupported_construct_is_reported_with_its_span() {
    // `2 ** b` with a non-constant exponent is outside the subset, on line 3.
    let src = "var b = 3\nvar out = 0\nout = 2 ** b\nreturn out\n";
    let diags = check(src);
    assert_eq!(diags.len(), 1, "{diags:#?}");
    assert_eq!(diags[0].code.id(), "E0600", "{:?}", diags[0]);
    assert_eq!(line_of(&diags[0], src), 3, "{:?}", diags[0]);
    // The check and the compile see the same construct, from the same walk.
    assert!(compile_program(&hir(src), &opts()).is_err());
}

/// Stopping at the first failure would make the check a strictly worse
/// version of "run it and see": fix one, rerun, discover the next.
#[test]
fn the_check_reports_every_bad_function_not_just_the_first() {
    let src = "\
function first(b) {
  return 2 ** b
}
function second(c) {
  return 3 ** c
}
return first(2) + second(3)
";
    let diags = check(src);
    assert_eq!(diags.len(), 2, "{diags:#?}");
    let mut lines: Vec<u32> = diags.iter().map(|d| line_of(d, src)).collect();
    lines.sort_unstable();
    assert_eq!(lines, vec![2, 5], "one span per offending function");
    for diag in &diags {
        assert_eq!(diag.code.id(), "E0600");
    }
    let notes: Vec<&str> = diags
        .iter()
        .flat_map(|d| d.notes.iter().map(String::as_str))
        .collect();
    assert!(
        notes.iter().any(|n| n.contains("`first`")) && notes.iter().any(|n| n.contains("`second`")),
        "each diagnostic names its function: {notes:?}"
    );
}

/// The whole-program gate has no per-function granularity to collect at, so
/// it stays one diagnostic — and it must not be swallowed by the collecting
/// walk that follows it.
#[test]
fn the_whole_program_gate_is_reported_once() {
    let src = "function p(@x) { q(x) }\nfunction q(y) { return y }\nvar n = 5\np(n)\nreturn n\n";
    let parsed = parse_with_features(
        src,
        source(),
        Version::V1,
        leek_parser::ParseFeatures::default(),
    );
    let file = SourceFile::cast(SyntaxNode::new_root(parsed.green.clone())).expect("parse");
    let hir = lower_file_versioned(&file, source(), 1).0;
    let diags = check_native_compat(&hir, &NativeOptions::debug().with_lang(1, false));
    assert_eq!(diags.len(), 1, "{diags:#?}");
    assert_eq!(diags[0].code.id(), "E0600");
}

/// A program the frontend already rejects reaches the backend as a lowering
/// failure. The check must hand back the lowerer's own diagnostics (each with
/// its own code), not one "native compilation failed" line.
#[test]
fn a_lowering_failure_reports_the_lowerers_diagnostics() {
    // `undefinedFunction` has no definition anywhere.
    let diags = check("return thisFunctionDoesNotExist(1)\n");
    assert!(!diags.is_empty());
    assert!(
        diags.iter().all(|d| d.code.id() != "E0601"),
        "lowering diagnostics keep their own codes: {diags:#?}"
    );
}
