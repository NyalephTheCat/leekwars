//! The emitter's output-side state is one `Rc<Shared>` (#252).
//!
//! A block-bodied lambda body is not emitted by the `Emitter` that owns the
//! file — `render_block_to_string` forks a scratch one for it. Everything
//! that scratch produces *for the file* (outlined `__anon_<n>` factories,
//! hoisted `FunctionLeekValue` singletons, the diagnostics it raises) used to
//! be copied back field by field when the scratch returned, so a field added
//! without extending that list silently dropped whatever it held. The fork
//! now shares one allocation instead, and these tests pin the observable
//! consequences: nothing the scratch produces goes missing, nothing is
//! duplicated across the boundary, and the factory declarations keep the
//! order they had when the hand-off was manual.

use leek_backend_java::{Options, emit};
use leek_diagnostics::Severity;
use leek_parser::{ParseFeatures, ast::AstNode, parse_with_features};
use leek_span::SourceId;
use leek_syntax::{SyntaxNode, Version};

fn source() -> SourceId {
    SourceId::new(1).unwrap()
}

fn hir(src: &str) -> leek_hir::HirFile {
    let parsed = parse_with_features(src, source(), Version::V4, ParseFeatures::default());
    let root = SyntaxNode::new_root(parsed.green);
    let sf = leek_parser::ast::SourceFile::cast(root).expect("parse");
    leek_hir::lower_file(&sf, source()).0
}

fn emit_v4(src: &str) -> leek_backend_java::EmittedJava {
    emit(&hir(src), &Options::clean(Version::V4, 1))
}

/// The `n` of every `private FunctionLeekValue __anon_<n>(` declaration, in
/// the order the class body declares them.
fn outlined_decl_order(java: &str) -> Vec<u32> {
    java.split("private FunctionLeekValue __anon_")
        .skip(1)
        .map(|rest| {
            rest.split('(')
                .next()
                .expect("a factory name is followed by its parameter list")
                .parse::<u32>()
                .expect("the __anon_ suffix is numeric")
        })
        .collect()
}

/// A block-bodied lambda with no outer captures is emitted *inline*, through
/// a scratch emitter. An unsupported construct inside one must still reach
/// the caller: this is the exact hand-off the old field-by-field copy existed
/// to preserve, and dropping the shared `Rc` would leave `diagnostics` empty
/// here while the Java still rendered.
#[test]
fn a_diagnostic_raised_inside_an_inline_block_lambda_reaches_the_caller() {
    // `nosuchfn` is in neither the builtin table nor a host environment, so
    // the emitter has no faithful Java for it. It is on line 2, *inside* the
    // lambda body — nothing on line 1 or 3 can raise a diagnostic.
    let src = "var f = function() {\n  return nosuchfn(1)\n}\nreturn f()\n";
    let out = emit_v4(src);

    assert_eq!(out.diagnostics.len(), 1, "{:?}", out.diagnostics);
    assert_eq!(out.diagnostics[0].severity, Severity::Error);
    assert!(out.has_errors());
    assert!(
        out.diagnostics[0].message.contains("nosuchfn"),
        "{:?}",
        out.diagnostics[0]
    );
    let pos = leek_span::LineTable::new(src).line_col(out.diagnostics[0].span.start);
    assert_eq!(pos.line, 2, "span is inside the lambda body, not {pos:?}");
    // …and the body really did render through the scratch path.
    assert!(out.java.contains("nosuchfn(1"), "{}", out.java);
}

/// The other scratch path: a block-bodied lambda that captures an outer local
/// is *outlined* to an `__anon_<n>` factory, whose body is rendered through a
/// second scratch emitter one level down. A diagnostic raised there crosses
/// two forks before it reaches `EmittedJava::diagnostics`.
#[test]
fn a_diagnostic_raised_inside_an_outlined_block_lambda_reaches_the_caller() {
    // `n` is written after the lambda, so the capture cannot be read directly
    // by an anonymous class and the body goes through the outlined factory.
    let src = "var n = 1\nvar f = function() {\n  return nosuchfn(n)\n}\nn = 2\nreturn f()\n";
    let out = emit_v4(src);

    assert!(
        out.java.contains("private FunctionLeekValue __anon_0("),
        "expected the outlined-factory path; got:\n{}",
        out.java
    );
    assert_eq!(out.diagnostics.len(), 1, "{:?}", out.diagnostics);
    let pos = leek_span::LineTable::new(src).line_col(out.diagnostics[0].span.start);
    assert_eq!(pos.line, 3, "span is inside the lambda body, not {pos:?}");
}

/// One hoisted singleton per referenced function, even when the references
/// straddle the scratch boundary. The scratch now *sees* the parent's map, so
/// the second reference is deduped at insert time rather than by the
/// `BTreeMap::append` that used to merge the two maps on the way out.
#[test]
fn a_function_referenced_on_both_sides_of_a_lambda_hoists_one_singleton() {
    let src = "function test() { return 1 }\n\
               var g = test\n\
               var f = function() {\n  var h = test\n  return h\n}\n\
               return g == test\n";
    let out = emit_v4(src);

    let hoists = out
        .java
        .matches("private FunctionLeekValue ufunction_")
        .count();
    assert_eq!(hoists, 1, "one singleton per function; got:\n{}", out.java);
    assert!(out.diagnostics.is_empty(), "{:?}", out.diagnostics);
}

/// Outlined factories keep the declaration order the manual hand-off gave
/// them. Siblings stay in source order; a nested outline finishes before the
/// outline that encloses it, so it is declared first. The numbers come from
/// the shared `outline_counter`, which is why the nested case is not `0, 1`.
#[test]
fn outlined_factory_declarations_keep_their_order() {
    // `a` is reassigned after both lambdas, so both capture and both outline.
    let siblings = "var a = 1\n\
                    var p = function() {\n  return a + 1\n}\n\
                    var q = function() {\n  return a + 2\n}\n\
                    a = 3\nreturn p() + q()\n";
    assert_eq!(
        outlined_decl_order(&emit_v4(siblings).java),
        vec![0, 1],
        "siblings declare in source order"
    );

    // The inner lambda captures `a` too, so it outlines from *inside* the
    // outer factory's body render and completes first.
    let nested = "var a = 1\n\
                  var outer = function() {\n  \
                  var inner = function() {\n    return a\n  }\n  \
                  return inner()\n}\n\
                  a = 3\nreturn outer()\n";
    assert_eq!(
        outlined_decl_order(&emit_v4(nested).java),
        vec![1, 0],
        "a nested factory is declared before the one that encloses it"
    );
}
