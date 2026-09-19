//! What a name resolves to across the checker's scope boundaries
//! (#192).
//!
//! Two rules, recorded here as type-table assertions because that is
//! what the LSP renders on hover:
//!
//! - a lambda body *captures*, so a typed local of the enclosing
//!   function keeps its type inside the closure;
//! - a named function body does **not** see the main block's locals,
//!   which LeekScript functions genuinely cannot access — only
//!   `global`s cross that boundary.

use leek_parser::ast::{AstNode, SourceFile};
use leek_parser::{ParseFeatures, parse_with_features};
use leek_span::SourceId;
use leek_syntax::{SyntaxNode, Version};
use leek_types::{Options, Type, TypeCheckResult, check_collecting};

const SOURCE: SourceId = match SourceId::new(1) {
    Some(id) => id,
    None => unreachable!(),
};

fn check(src: &str) -> TypeCheckResult {
    check_with(src, Options::default())
}

fn check_with(src: &str, opts: Options) -> TypeCheckResult {
    let parsed = parse_with_features(src, SOURCE, Version::V4, ParseFeatures::default());
    let file = SourceFile::cast(SyntaxNode::new_root(parsed.green)).expect("fixture parses");
    check_collecting(&file, SOURCE, Version::V4, opts)
}

/// The recorded type of `expr`, located at the occurrence of `expr`
/// inside the (unique) fixture slice `within`.
fn ty_at(r: &TypeCheckResult, src: &str, within: &str, expr: &str) -> Option<Type> {
    let slice = src.find(within).expect("fixture contains the slice");
    let offset = within.find(expr).expect("the slice contains the expr");
    let start = u32::try_from(slice + offset).expect("fixture offsets fit in u32");
    let end = start + u32::try_from(expr.len()).expect("fixture offsets fit in u32");
    r.table
        .exprs
        .iter()
        .find(|t| t.span.start == start && t.span.end == end)
        .map(|t| t.ty.clone())
}

// ---- Lambdas capture ----

#[test]
fn a_lambda_inside_a_function_reads_a_captured_typed_local() {
    // `push_function` for the lambda body made `x` resolve to nothing,
    // so the capture hovered as `any` (#192).
    let src = "function g() {\n\tinteger x = 5\n\tvar f = -> x + 1\n}\n";
    let r = check(src);
    assert_eq!(
        ty_at(&r, src, "-> x + 1", "x"),
        Some(Type::Integer),
        "{:?}",
        r.table.exprs
    );
}

#[test]
fn a_lambda_in_the_main_block_reads_a_captured_typed_local() {
    let src = "integer n = 5\nvar f = -> n + 1\n";
    let r = check(src);
    assert_eq!(
        ty_at(&r, src, "-> n + 1", "n"),
        Some(Type::Integer),
        "{:?}",
        r.table.exprs
    );
}

#[test]
fn a_lambda_param_shadows_the_captured_local_of_the_same_name() {
    let src = "function g() {\n\tinteger x = 5\n\tvar f = (string x) -> x\n}\n";
    let r = check(src);
    assert_eq!(
        ty_at(&r, src, "(string x) -> x", "-> x"),
        None,
        "the located span is the body, not the param"
    );
    assert_eq!(
        ty_at(&r, src, ") -> x", "x"),
        Some(Type::String),
        "{:?}",
        r.table.exprs
    );
}

// ---- Function bodies stop at the boundary ----

#[test]
fn a_function_does_not_see_a_main_block_local() {
    // `scopes.first()` is the file scope, which holds every top-level
    // binding and not just the globals, so the body used to pick up
    // `topv`'s `integer` (#192).
    let src = "integer topv = 3\nfunction f() {\n\tvar m = topv\n}\n";
    let r = check(src);
    assert_eq!(
        ty_at(&r, src, "var m = topv", "topv"),
        Some(Type::Any),
        "{:?}",
        r.table.exprs
    );
}

#[test]
fn a_lambda_inside_a_function_does_not_see_a_main_block_local() {
    // The two rules compose: the lambda scope is transparent, but the
    // enclosing function's boundary still stops the walk.
    let src = "integer topv = 3\nfunction f() {\n\tvar g = -> topv\n}\n";
    let r = check(src);
    assert_eq!(
        ty_at(&r, src, "-> topv", "topv"),
        Some(Type::Any),
        "{:?}",
        r.table.exprs
    );
}

#[test]
fn a_function_does_not_see_another_functions_local() {
    let src = "function a() {\n\tinteger inner = 3\n}\nfunction b() {\n\tvar m = inner\n}\n";
    let r = check(src);
    assert_eq!(
        ty_at(&r, src, "var m = inner", "inner"),
        Some(Type::Any),
        "{:?}",
        r.table.exprs
    );
}

// ---- …but globals still cross it ----

#[test]
fn a_function_sees_a_typed_global() {
    let src = "global integer gv = 3\nfunction f() {\n\tvar m = gv\n}\n";
    let r = check(src);
    assert_eq!(
        ty_at(&r, src, "var m = gv", "gv"),
        Some(Type::Integer),
        "{:?}",
        r.table.exprs
    );
}

#[test]
fn a_global_declared_inside_a_block_is_still_program_wide() {
    // `global` is program-wide wherever it textually sits, so the
    // binding is recorded on the file scope, not the enclosing block's.
    let src = "if (1) {\n\tglobal integer gv = 3\n}\nfunction f() {\n\tvar m = gv\n}\n";
    let r = check(src);
    assert_eq!(
        ty_at(&r, src, "var m = gv", "gv"),
        Some(Type::Integer),
        "{:?}",
        r.table.exprs
    );
}

#[test]
fn a_main_block_local_still_types_inside_the_main_block() {
    // The tightening is about function bodies only — nested blocks of
    // the main block keep reading its locals.
    let src = "integer topv = 3\nif (1) {\n\tvar m = topv\n}\n";
    let r = check(src);
    assert_eq!(
        ty_at(&r, src, "var m = topv", "topv"),
        Some(Type::Integer),
        "{:?}",
        r.table.exprs
    );
}

#[test]
fn a_captured_local_narrows_inside_a_lambda_under_strict() {
    // Capture restores more than hover: the null-check narrowing needs
    // the binding to be visible from inside the closure.
    let src = "function g() {\n\tstring? s = null\n\tvar f = -> s != null ? s : \"\"\n}\n";
    let r = check_with(
        src,
        Options {
            strict: true,
            ..Options::default()
        },
    );
    assert_eq!(
        ty_at(&r, src, "? s : ", "s"),
        Some(Type::String),
        "{:?}",
        r.table.exprs
    );
}
