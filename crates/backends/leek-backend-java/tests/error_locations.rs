//! The Java emitter points at source when it cannot translate something
//! (JAVA-06 / #152).
//!
//! Every assertion here is on the *rendered* diagnostic, not on the value:
//! "the emitter recorded a span" is only worth anything if the span survives
//! into what the user is shown. An unsupported construct used to reach the
//! user as a javac error on a line of generated Java — true, and useless on a
//! 400-line AI, if it reached them at all.

use leek_backend_java::{Options, emit};
use leek_diagnostics::{Renderer, Severity, Sources};
use leek_parser::{ast::AstNode, parse};
use leek_span::SourceId;
use leek_syntax::{SyntaxNode, Version};

fn source() -> SourceId {
    SourceId::new(1).unwrap()
}

fn hir(src: &str) -> leek_hir::HirFile {
    let parsed = parse(src, source(), Version::V4);
    let root = SyntaxNode::new_root(parsed.green);
    let sf = leek_parser::ast::SourceFile::cast(root).expect("parse");
    leek_hir::lower_file(&sf, source()).0
}

/// The diagnostics from emitting `src`, and what `miku build` prints for them.
fn emit_diagnostics(src: &str) -> (Vec<leek_diagnostics::Diagnostic>, String) {
    let out = emit(&hir(src), &Options::clean(Version::V4, 1));
    let sources = Sources::single(source(), "main.leek", src);
    let rendered = out
        .diagnostics
        .iter()
        .map(|d| Renderer::default().render(d, &sources))
        .collect();
    (out.diagnostics, rendered)
}

#[test]
fn a_call_to_an_unknown_function_renders_with_its_own_line_and_caret() {
    // `nosuchfn` is in neither the builtin table nor a host environment, so
    // the emitter falls back to a bare `nosuchfn(1)` — not a method on the
    // generated class. It is on line 3.
    let src = "var a = 1\nvar out = 0\nout = nosuchfn(a)\nreturn out\n";
    let (diags, out) = emit_diagnostics(src);

    assert_eq!(diags.len(), 1, "{diags:?}");
    assert_eq!(diags[0].severity, Severity::Error);
    let table = leek_span::LineTable::new(src);
    let pos = table.line_col(diags[0].span.start);
    assert_eq!(pos.line, 3, "span is on line 3, not {pos:?}");

    assert!(out.contains("error[E0610]"), "{out}");
    assert!(
        out.contains("does not support a call to `nosuchfn`"),
        "{out}"
    );
    assert!(out.contains("main.leek:3:"), "{out}");
    // The caret is drawn under the callee token, not the whole statement.
    assert!(out.contains("out = nosuchfn(a)"), "{out}");
    assert!(out.contains('^'), "{out}");
}

#[test]
fn a_first_class_reference_to_an_unknown_name_renders_with_a_location() {
    // Reaches the *other* fallback: not a call, so it goes through
    // `write_name`'s terminal arm after `lookup_constant` and
    // `builtin_fn_wrapper` have both missed.
    let src = "var f = nosuchfn\nreturn f\n";
    let (diags, out) = emit_diagnostics(src);

    assert_eq!(diags.len(), 1, "{diags:?}");
    let table = leek_span::LineTable::new(src);
    assert_eq!(table.line_col(diags[0].span.start).line, 1);
    assert!(out.contains("error[E0610]"), "{out}");
    assert!(
        out.contains("does not support a first-class reference to `nosuchfn`"),
        "{out}"
    );
    assert!(out.contains("main.leek:1:"), "{out}");
}

#[test]
fn an_unsupported_construct_inside_a_lambda_is_not_swallowed() {
    // A block-bodied lambda is emitted through a scratch `Emitter`, which has
    // its own diagnostics buffer. Without the hand-off back to the parent the
    // complaint is raised and then dropped on the floor — exactly the silent
    // failure this is meant to end.
    let src = "var f = function(x) { return nosuchfn(x); }\nreturn f(1)\n";
    let (diags, out) = emit_diagnostics(src);

    assert!(!diags.is_empty(), "the lambda body's complaint was lost");
    assert!(out.contains("error[E0610]"), "{out}");
    assert!(out.contains("main.leek:1:"), "{out}");
}

#[test]
fn a_program_the_backend_fully_supports_reports_nothing() {
    // The no-false-positive guard. The bare-name fallback sits after the
    // builtin table, the environment catalog *and* the built-in-class
    // constructors, so none of these may reach it — if one does, every
    // `miku build` of a working AI starts failing.
    for src in [
        "return abs(-1)\n",
        "var a = [3, 1, 2]\nsort(a)\nreturn a\n",
        "return arrayMap([1, 2], cos)\n",
        "var m = Map()\nm[1] = 2\nreturn m\n",
        "return Array(1, 2, 3)\n",
        "function f(n) { return n * 2 }\nreturn f(21)\n",
        "class C { v = 1\npublic get() { return this.v } }\nreturn (new C()).get()\n",
    ] {
        let (diags, out) = emit_diagnostics(src);
        assert!(diags.is_empty(), "false positive on `{src}`:\n{out}");
    }
}

#[test]
fn a_receiver_shaped_catalog_function_is_not_diagnosed() {
    // A `receiver` entry deliberately falls through to the bare-name arm: it
    // is an instance method on the AI base class, so `getLife(…)` is the
    // *correct* Java. Keying the diagnostic on "the emitter took the fallback
    // path" without this exemption would fail every fight AI build.
    let src = "return getLife()\n";
    let catalog = "namespace = com.leekwars.generator.classes.*\n\
        getLife\tEntityClass\treceiver\t0\t1\t50\n";
    let opts = Options::clean(Version::V4, 1).with_environment(std::sync::Arc::new(
        leek_environment::FileCatalog::parse(catalog).expect("catalog"),
    ));
    let out = emit(&hir(src), &opts);
    assert!(out.diagnostics.is_empty(), "{:?}", out.diagnostics);
    assert!(out.java.contains("getLife("), "{}", out.java);
}

#[test]
fn emitted_java_is_still_produced_alongside_the_diagnostics() {
    // `emit` stays infallible: the front end decides what to do with a
    // diagnosed file, and the parity harness still gets something to diff.
    let out = emit(
        &hir("return nosuchfn(1)\n"),
        &Options::clean(Version::V4, 1),
    );
    assert!(out.has_errors());
    assert!(out.java.contains("class AI_1"), "{}", out.java);
}
