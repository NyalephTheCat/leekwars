//! Type inference and assignment-compatibility checking.
//!
//! First-slice scope:
//! - Infer the type of each local variable from its initializer.
//! - On subsequent assignment, check RHS type against the LHS's
//!   recorded type. Mismatch emits `ASSIGNMENT_INCOMPATIBLE_TYPE`.
//! - Inference covers literals, identifier lookup, simple arithmetic
//!   binary operators, array/map/set/object literals, `new C(...)`.
//!
//! Types not yet inferred (function calls, complex postfix chains,
//! etc.) fall back to `Type::Any`, which is compatible with anything
//! to avoid false positives.
//!
//! Module layout:
//! - [`ty`] — the [`Type`] enum and conversions to/from the CST.
//! - [`builtins`] — per-builtin signature table for strict-mode
//!   WRONG_ARGUMENT_TYPE detection.
//! - [`checker`] — the walking `Checker` and its
//!   `check_*` / `infer_*` methods.
//!
//! Public entry: [`check`].

use leek_diagnostics::Diagnostic;
use leek_parser::ast::SourceFile;
use leek_span::SourceId;
use leek_syntax::Version;

mod builtins;
mod checker;
pub mod generic;
pub mod pipeline;
mod ty;

pub use ty::{Type, type_from_node};

pub mod index;
pub use index::{InferredSignatures, TypeTable, TypedExpr};

/// Diagnostic codes produced by the type checker. Re-exported from
/// the central catalog in [`leek_diagnostics::codes`].
pub use leek_diagnostics::codes;

/// Test-only probe: counts how many times `typecheck_query` actually
/// executed (vs cached). Used by the memoization smoke test.
#[cfg(all(test, feature = "salsa"))]
pub(crate) mod salsa_probe {
    use std::sync::Mutex;
    use std::sync::atomic::AtomicUsize;
    pub(crate) static TYPECHECK_QUERY_CALLS: AtomicUsize = AtomicUsize::new(0);
    pub(crate) static SERIAL: Mutex<()> = Mutex::new(());
}

// ---- Public entry points ----

/// Run the type checker at the latest version with default options.
pub fn check(file: &SourceFile, source: SourceId) -> Vec<Diagnostic> {
    check_with_version(file, source, Version::LATEST)
}

pub fn check_with_version(
    file: &SourceFile,
    source: SourceId,
    version: Version,
) -> Vec<Diagnostic> {
    check_with_options(file, source, version, Options::default())
}

/// Options influencing what diagnostics are emitted.
#[derive(Debug, Clone, Copy, Default)]
pub struct Options {
    /// Strict mode mirrors upstream's `// @strict` pragma — enables
    /// null-propagation diagnostics like compound-assign on a
    /// null-bound binding, return-type checking, and
    /// WRONG_ARGUMENT_TYPE on known builtins.
    pub strict: bool,
    /// **Experimental.** Resolve generic builtin signatures (e.g.
    /// `first(Array<T>) -> T`) during call inference. Off by default;
    /// the pipeline enables it via the `LEEK_EXPERIMENTAL_GENERICS`
    /// environment variable. See [`crate::generic`].
    pub experimental_generics: bool,
    /// Seed the typed standard-library + leek-wars signature headers
    /// (`stdlib.leek` / `leekwars.leek`) so calls to builtins and game
    /// functions infer their declared return types. Off by default to
    /// keep the corpus/driver baseline unchanged; the LSP turns it on
    /// process-wide via [`set_seed_library`] so hover and inference see
    /// real signatures without the experimental env var.
    pub seed_library: bool,
    /// **Experimental.** Seed the implicit standard-library prelude
    /// (`LEEK_EXPERIMENTAL_PRELUDE`). Threaded from [`leek_span::FeatureFlags`]
    /// by the pipeline rather than read from env inside the (salsa-tracked)
    /// type-check query.
    pub experimental_prelude: bool,
}

use std::sync::atomic::{AtomicBool, Ordering};

/// Process-global toggle for [`Options::seed_library`] used by the
/// pipeline. Set once by a tool (the LSP) at startup; never flipped
/// mid-run, so salsa memoization stays correct. Defaults off so the
/// driver / corpus path is unaffected.
static SEED_LIBRARY: AtomicBool = AtomicBool::new(false);

/// Enable (or disable) library-signature seeding for all subsequent
/// pipeline type-check runs in this process. The LSP calls this once at
/// workspace startup.
pub fn set_seed_library(on: bool) {
    SEED_LIBRARY.store(on, Ordering::Relaxed);
}

/// Whether the pipeline should seed library signatures (see
/// [`set_seed_library`]).
pub fn seed_library_enabled() -> bool {
    SEED_LIBRARY.load(Ordering::Relaxed)
}

pub fn check_with_options(
    file: &SourceFile,
    source: SourceId,
    version: Version,
    opts: Options,
) -> Vec<Diagnostic> {
    check_collecting(file, source, version, opts).diagnostics
}

/// Type-checker outcome carrying both diagnostics and the LSP-facing
/// [`TypeTable`]. Prefer this entry point over
/// [`check_with_options`] when downstream consumers (the LSP,
/// `miku watch`) need per-expression type data.
#[derive(Debug, Clone, Default)]
pub struct TypeCheckResult {
    pub diagnostics: Vec<Diagnostic>,
    pub table: TypeTable,
    /// Declared/inferred function-return and class-member types, so the
    /// LSP can render a signature's type even when the source omits the
    /// annotation.
    pub signatures: InferredSignatures,
}

pub fn check_collecting(
    file: &SourceFile,
    source: SourceId,
    version: Version,
    opts: Options,
) -> TypeCheckResult {
    let mut c = checker::Checker::new(source, version, opts);
    // Seed ambient library signatures (incl. generics) so calls to
    // standard-library + leek-wars functions infer precise returns.
    // On for the LSP (`opts.seed_library`) and for the experimental
    // env path; off for the corpus/driver baseline.
    if opts.seed_library || opts.experimental_prelude {
        c.seed_library_signatures();
    }
    c.check_file(file);
    let signatures = InferredSignatures {
        fn_returns: c.user_fn_return_type.clone(),
        fn_params: c.user_fn_param_types.clone(),
        field_types: c.class_field_types.clone(),
        method_returns: c.class_method_returns.clone(),
    };
    let mut exprs = c.typed_exprs;
    // Stable order: by span.start (and break ties by length so wider
    // spans come first — the LSP's `smallest_at` lookup tolerates
    // either order, but this is deterministic).
    exprs.sort_by_key(|t| (t.span.start, t.span.end - t.span.start));
    TypeCheckResult {
        diagnostics: c.diagnostics,
        table: TypeTable { exprs },
        signatures,
    }
}

#[cfg(test)]
mod index_tests {
    use leek_lexer::lex;
    use leek_parser::ast::{AstNode, SourceFile as AstSourceFile};
    use leek_parser::parse_tokens;
    use leek_span::SourceId;
    use leek_syntax::SyntaxNode;

    use super::*;

    fn run(text: &str) -> TypeCheckResult {
        let src = SourceId::new(1).unwrap();
        let lex_out = lex(text, src, Version::LATEST);
        let parse = parse_tokens(text, src, &lex_out.tokens, Version::LATEST);
        let ast = AstSourceFile::cast(SyntaxNode::new_root(parse.green))
            .expect("parse produced a SourceFile");
        check_collecting(&ast, src, Version::LATEST, Options::default())
    }

    fn run_seeded(text: &str) -> TypeCheckResult {
        let src = SourceId::new(1).unwrap();
        let lex_out = lex(text, src, Version::LATEST);
        let parse = parse_tokens(text, src, &lex_out.tokens, Version::LATEST);
        let ast = AstSourceFile::cast(SyntaxNode::new_root(parse.green)).expect("ast");
        check_collecting(
            &ast,
            src,
            Version::LATEST,
            Options {
                experimental_prelude: true,
                ..Default::default()
            },
        )
    }

    #[test]
    fn cached_library_seeding_is_stable_and_complete() {
        // Seeding the stdlib signatures must yield the same maps whether it
        // walks the headers fresh (cold cache) or clones the memoized snapshot
        // (warm cache). Two sequential seeded checks must agree, and a known
        // stdlib signature must survive caching.
        let first = run_seeded("var x = 1\n").signatures.fn_returns;
        let second = run_seeded("var x = 1\n").signatures.fn_returns;
        assert_eq!(first, second, "seeded signatures stable across runs");
        assert_eq!(
            first.get("floor"),
            Some(&Type::Integer),
            "stdlib `floor` signature present after (cached) seeding"
        );
    }

    #[test]
    fn integer_literal_at_cursor() {
        let text = "var x = 5;";
        let r = run(text);
        let offset = text.find('5').unwrap() as u32;
        let entry = r.table.smallest_at(offset).expect("found typed expr");
        assert_eq!(entry.ty, Type::Integer);
    }

    #[test]
    fn binary_expr_inferred_as_real_from_promotion() {
        // Real(f64) literal promotes the integer addition to Real.
        let text = "var n = 1 + 2.5;";
        let r = run(text);
        let plus_offset = text.find('+').unwrap() as u32;
        let entry = r.table.smallest_at(plus_offset);
        // The innermost span at `+` may be the BinaryExpr itself.
        // Either way the inferred type should be Real.
        if let Some(t) = entry {
            // Innermost surrounding expr; either the `+` site or the
            // whole `1 + 2.5`. Both are Real after promotion.
            assert_eq!(t.ty, Type::Real, "binary inferred type");
        }
    }

    #[test]
    fn this_is_typed_as_enclosing_class() {
        let r = run("class Cat {\n  meow() { return this }\n}\n");
        let offset = r"class Cat {
  meow() { return this"
            .len() as u32
            - 4; // start of `this`
        let entry = r.table.smallest_at(offset).expect("typed `this`");
        assert_eq!(entry.ty, Type::ClassInstance("Cat".into(), vec![]));
    }

    fn run_strict(text: &str) -> TypeCheckResult {
        let src = SourceId::new(1).unwrap();
        let lex_out = lex(text, src, Version::LATEST);
        let parse = parse_tokens(text, src, &lex_out.tokens, Version::LATEST);
        let ast = AstSourceFile::cast(SyntaxNode::new_root(parse.green)).expect("ast");
        check_collecting(
            &ast,
            src,
            Version::LATEST,
            Options {
                strict: true,
                ..Default::default()
            },
        )
    }

    fn run_generics(text: &str) -> TypeCheckResult {
        let src = SourceId::new(1).unwrap();
        let lex_out = lex(text, src, Version::LATEST);
        let parse = parse_tokens(text, src, &lex_out.tokens, Version::LATEST);
        let ast = AstSourceFile::cast(SyntaxNode::new_root(parse.green)).expect("ast");
        check_collecting(
            &ast,
            src,
            Version::LATEST,
            Options {
                strict: false,
                experimental_generics: true,
                ..Default::default()
            },
        )
    }

    /// Like [`run_generics`] but parses with the experimental generic
    /// *syntax* enabled, so user-defined `f<T>(…)` declarations produce a
    /// `TypeParamList` the checker can build a signature from.
    fn run_generics_parsed(text: &str) -> TypeCheckResult {
        run_generics_core(text, false)
    }

    /// Strict variant — needed when a plain `var x = …` must commit to
    /// its initializer's inferred type (non-strict `var` stays dynamic).
    fn run_generics_parsed_strict(text: &str) -> TypeCheckResult {
        run_generics_core(text, true)
    }

    fn run_generics_core(text: &str, strict: bool) -> TypeCheckResult {
        use leek_parser::{ParseFeatures, parse_tokens_with};
        let src = SourceId::new(1).unwrap();
        let lex_out = lex(text, src, Version::LATEST);
        let parse = parse_tokens_with(
            text,
            src,
            &lex_out.tokens,
            Version::LATEST,
            ParseFeatures {
                generics: true,
                ..Default::default()
            },
        );
        let ast = AstSourceFile::cast(SyntaxNode::new_root(parse.green)).expect("ast");
        check_collecting(
            &ast,
            src,
            Version::LATEST,
            Options {
                strict,
                experimental_generics: true,
                ..Default::default()
            },
        )
    }

    /// Like [`run_generics_parsed`] but first seeds the ambient typed
    /// library signatures (as the `LEEK_EXPERIMENTAL_PRELUDE` path does),
    /// so calls to standard-library functions resolve. `experimental_generics`
    /// is toggleable to prove the *library* generic path works on its own,
    /// independent of the hand-written `generic_builtin` table.
    fn run_with_library(text: &str, experimental_generics: bool) -> TypeCheckResult {
        let src = SourceId::new(1).unwrap();
        let lex_out = lex(text, src, Version::LATEST);
        let parse = parse_tokens(text, src, &lex_out.tokens, Version::LATEST);
        let ast = AstSourceFile::cast(SyntaxNode::new_root(parse.green)).expect("ast");
        let mut c = crate::checker::Checker::new(
            src,
            Version::LATEST,
            Options {
                strict: false,
                experimental_generics,
                ..Default::default()
            },
        );
        c.seed_library_signatures();
        c.check_file(&ast);
        let mut exprs = c.typed_exprs;
        exprs.sort_by_key(|t| (t.span.start, t.span.end - t.span.start));
        TypeCheckResult {
            diagnostics: c.diagnostics,
            table: TypeTable { exprs },
            signatures: InferredSignatures::default(),
        }
    }

    /// Type of the expr spanning the `occ`-th (1-based) occurrence of
    /// `needle` — the largest typed expr fully contained in its range.
    fn ty_at(r: &TypeCheckResult, text: &str, needle: &str, occ: usize) -> Type {
        let mut idx = 0;
        let mut start = 0;
        for _ in 0..occ {
            let p = text[idx..].find(needle).expect("needle") + idx;
            start = p;
            idx = p + needle.len();
        }
        let end = start + needle.len();
        r.table
            .exprs
            .iter()
            .filter(|e| e.span.start as usize >= start && e.span.end as usize <= end)
            .max_by_key(|e| e.span.end - e.span.start)
            .unwrap_or_else(|| panic!("no typed expr in {needle:?}"))
            .ty
            .clone()
    }

    #[test]
    fn infers_field_access_on_this() {
        let text = "class Cat {\n  integer age\n  bday() { return this.age }\n}\n";
        let r = run(text);
        assert_eq!(ty_at(&r, text, "this.age", 1), Type::Integer);
    }

    #[test]
    fn infers_method_call_return_on_this() {
        let text =
            "class Cat {\n  string speak() { return \"m\" }\n  go() { return this.speak() }\n}\n";
        let r = run(text);
        assert_eq!(ty_at(&r, text, "this.speak()", 1), Type::String);
    }

    #[test]
    fn infers_inherited_field_through_chain() {
        let text = "class Animal {\n  integer hp\n}\nclass Cat extends Animal {\n  get() { return this.hp }\n}\n";
        let r = run(text);
        assert_eq!(ty_at(&r, text, "this.hp", 1), Type::Integer);
    }

    #[test]
    fn infers_instance_field_in_strict() {
        let text = "class Cat {\n  integer age\n}\nvar c = new Cat()\nvar a = c.age\n";
        let r = run_strict(text);
        assert_eq!(ty_at(&r, text, "c.age", 1), Type::Integer);
    }

    #[test]
    fn infers_array_literal_index_element() {
        let text = "var a = [1, 2, 3]\nvar x = a[0]\n";
        let r = run_strict(text);
        assert_eq!(ty_at(&r, text, "a[0]", 1), Type::Integer);
    }

    #[test]
    fn infers_ternary_branch_unification() {
        let r = run("var x = true ? 1 : 2\n");
        assert_eq!(ty_at(&r, "var x = true ? 1 : 2\n", "true ? 1 : 2", 1), Type::Integer);
    }

    #[test]
    fn ternary_null_branch_is_nullable() {
        let text = "var x = true ? 5 : null\n";
        let r = run(text);
        assert_eq!(
            ty_at(&r, text, "true ? 5 : null", 1),
            Type::Nullable(Box::new(Type::Integer))
        );
    }

    #[test]
    fn narrows_instanceof_in_then_branch() {
        let text = "function f(a) {\n  if (a instanceof Cat) { return a }\n  return null\n}\n";
        let r = run(text);
        // The `a` inside the then-branch is narrowed to a Cat instance.
        assert_eq!(ty_at(&r, text, "return a", 1), {
            // `return a` — the `a` is the last occurrence.
            Type::ClassInstance("Cat".into(), vec![])
        });
    }

    #[test]
    fn narrows_not_null_strips_nullable() {
        let text = "class Cat {\n  integer age\n}\nfunction f(Cat? c) {\n  if (c != null) { return c.age }\n  return 0\n}\n";
        let r = run_strict(text);
        // Inside the guard, `c` is non-null `Cat`, so `c.age` is integer.
        assert_eq!(ty_at(&r, text, "c.age", 1), Type::Integer);
    }

    #[test]
    fn experimental_generics_off_by_default() {
        // Without the flag, a generic builtin's return stays `any`.
        let text = "var x = first([1, 2, 3])\n";
        let r = run(text);
        assert_eq!(ty_at(&r, text, "first([1, 2, 3])", 1), Type::Any);
    }

    #[test]
    fn experimental_generics_infers_array_element() {
        // With the flag, `first(Array<integer>)` resolves to `integer`.
        let text = "var x = first([1, 2, 3])\n";
        let r = run_generics(text);
        assert_eq!(ty_at(&r, text, "first([1, 2, 3])", 1), Type::Integer);
    }

    #[test]
    fn experimental_generics_infers_through_string_array() {
        let text = "var x = pop([\"a\", \"b\"])\n";
        let r = run_generics(text);
        assert_eq!(ty_at(&r, text, "pop([\"a\", \"b\"])", 1), Type::String);
    }

    #[test]
    fn user_generic_fn_resolves_return_from_args() {
        // A user-declared generic function: `first<T>(Array<T>) -> T`
        // resolves `T` to the element type at the call site.
        let text = "function first<T>(Array<T> a) -> T { return a[0] }\nvar x = first([1, 2, 3])\n";
        let r = run_generics_parsed(text);
        assert_eq!(ty_at(&r, text, "first([1, 2, 3])", 1), Type::Integer);
    }

    #[test]
    fn user_generic_identity_fn_threads_type() {
        // `id<T>(T) -> T` returns exactly what it's given.
        let text = "function id<T>(T v) -> T { return v }\nvar s = id(\"hi\")\n";
        let r = run_generics_parsed(text);
        assert_eq!(ty_at(&r, text, "id(\"hi\")", 1), Type::String);
    }

    #[test]
    fn library_generic_resolves_without_builtin_table() {
        // `pop<T>(Array<T>) -> T` comes from the seeded typed library.
        // With the `generic_builtin` table OFF (experimental_generics =
        // false), the precise `integer` result can only come from the
        // library's generic signature — proving the prelude path.
        let text = "var x = pop([1, 2, 3])\n";
        let r = run_with_library(text, false);
        assert_eq!(ty_at(&r, text, "pop([1, 2, 3])", 1), Type::Integer);
    }

    #[test]
    fn library_plain_return_type_is_seeded() {
        // `count(Array) -> integer` is a non-generic library signature;
        // seeding it lets a bare `count(...)` call infer `integer`
        // instead of widening to `any`.
        let text = "var n = count([1, 2, 3])\n";
        let r = run_with_library(text, false);
        assert_eq!(ty_at(&r, text, "count([1, 2, 3])", 1), Type::Integer);
    }

    #[test]
    fn library_signature_unseeded_widens_to_any() {
        // Control: without the library, the same call is `any`.
        let text = "var n = count([1, 2, 3])\n";
        let r = run(text);
        assert_eq!(ty_at(&r, text, "count([1, 2, 3])", 1), Type::Any);
    }

    #[test]
    fn user_redefinition_shadows_library_generic() {
        // A user `pop` with a concrete return shadows the seeded
        // `pop<T>` generic — the user's `string` return wins.
        let text =
            "function pop(Array a) -> string { return \"x\" }\nvar x = pop([1, 2, 3])\n";
        let r = run_with_library(text, false);
        assert_eq!(ty_at(&r, text, "pop([1, 2, 3])", 1), Type::String);
    }

    /// Resolve a type *annotation* (a `TypeRef`, not an expression) to a
    /// [`Type`] — used for the function-type forms, which only appear in
    /// annotation position and so never enter the expression type table.
    fn type_of_annotation(form: &str) -> Type {
        let text = format!("function f({form} a) {{ }}\n");
        let src = SourceId::new(1).unwrap();
        let lex_out = lex(&text, src, Version::LATEST);
        let parse = parse_tokens(&text, src, &lex_out.tokens, Version::LATEST);
        let node = SyntaxNode::new_root(parse.green);
        let tref = node
            .descendants()
            .find(|n| n.kind() == leek_syntax::SyntaxKind::TypeRef)
            .expect("a TypeRef");
        type_from_node(&tref)
    }

    #[test]
    fn function_type_tracks_params_and_return() {
        // `Function<integer, string => boolean>` records both params.
        assert_eq!(
            type_of_annotation("Function<integer, string => boolean>"),
            Type::function_with(vec![Type::Integer, Type::String], Type::Boolean)
        );
    }

    #[test]
    fn empty_param_function_type_has_no_params() {
        // `Function< => string>` → no params, string return.
        assert_eq!(
            type_of_annotation("Function< => string>"),
            Type::function_with(vec![], Type::String)
        );
    }

    #[test]
    fn map_literal_infers_key_and_value_types() {
        let text = "var m = [\"a\": 1, \"b\": 2]\nvar v = m\n";
        let r = run_strict(text);
        assert_eq!(
            ty_at(&r, text, "[\"a\": 1, \"b\": 2]", 1),
            Type::Map(Box::new(Type::String), Box::new(Type::Integer))
        );
    }

    #[test]
    fn set_literal_infers_element_type() {
        let text = "var s = <1, 2, 3>\nvar v = s\n";
        let r = run_strict(text);
        assert_eq!(
            ty_at(&r, text, "<1, 2, 3>", 1),
            Type::Set(Box::new(Type::Integer))
        );
    }

    #[test]
    fn arrow_lambda_infers_return_from_body() {
        // `x => x + 1` with no annotation: integer body → integer return.
        let text = "var f = (integer x) => x + 1\nvar v = f\n";
        let r = run_strict(text);
        assert_eq!(
            ty_at(&r, text, "(integer x) => x + 1", 1),
            Type::function_with(vec![Type::Integer], Type::Integer)
        );
    }

    #[test]
    fn leekwars_game_function_return_is_seeded() {
        // `getColor(integer, integer, integer) -> integer` is a leek-wars
        // game function; seeding LEEKWARS_SRC lets it infer in-script.
        let text = "var c = getColor(1, 2, 3)\n";
        let r = run_with_library(text, false);
        assert_eq!(ty_at(&r, text, "getColor(1, 2, 3)", 1), Type::Integer);
    }

    #[test]
    fn generic_class_field_resolves_from_instance_args() {
        // `Box<integer> b` → `b.value` is `integer`.
        let text = "class Box<T> { T value }\nBox<integer> b = new Box()\nvar x = b.value\n";
        let r = run_generics_parsed(text);
        assert_eq!(ty_at(&r, text, "b.value", 1), Type::Integer);
    }

    #[test]
    fn generic_class_method_return_resolves() {
        // `T get()` on a `Box<string>` returns `string`.
        let text = "class Box<T> { T value\n T get() { return value } }\nBox<string> b = new Box()\nvar x = b.get()\n";
        let r = run_generics_parsed(text);
        assert_eq!(ty_at(&r, text, "b.get()", 1), Type::String);
    }

    #[test]
    fn inherited_generic_field_remaps_parent_type_arg() {
        // `Box<T> { T value }`; `IntBox extends Box<integer>` inherits
        // `value` with `T` re-mapped to `integer`.
        let text = "class Box<T> { T value }\n\
            class IntBox extends Box<integer> {}\n\
            IntBox b = new IntBox()\nvar x = b.value\n";
        let r = run_generics_parsed(text);
        assert_eq!(ty_at(&r, text, "b.value", 1), Type::Integer);
    }

    #[test]
    fn inherited_generic_method_remaps_parent_type_arg() {
        // The inherited `T get()` returns the re-mapped `string`.
        let text = "class Box<T> { T value\n T get() { return value } }\n\
            class StrBox extends Box<string> {}\n\
            StrBox b = new StrBox()\nvar x = b.get()\n";
        let r = run_generics_parsed(text);
        assert_eq!(ty_at(&r, text, "b.get()", 1), Type::String);
    }

    #[test]
    fn inherited_generic_threads_child_type_var_to_parent() {
        // `Stack<T> extends Box<T>` — the child's own `T` is threaded into
        // the parent's `T`, so `Stack<integer>.value` is `integer`.
        let text = "class Box<T> { T value }\n\
            class Stack<T> extends Box<T> {}\n\
            Stack<integer> s = new Stack()\nvar x = s.value\n";
        let r = run_generics_parsed(text);
        assert_eq!(ty_at(&r, text, "s.value", 1), Type::Integer);
    }

    #[test]
    fn generic_class_binds_type_arg_from_constructor() {
        // `new Pair(5)` with `constructor(T v)` infers `Pair<integer>`,
        // so `p.value` is `integer` — no explicit type annotation.
        let text = "class Pair<T> { T value\n constructor(T v) { value = v } }\nvar p = new Pair(5)\nvar x = p.value\n";
        let r = run_generics_parsed_strict(text);
        assert_eq!(ty_at(&r, text, "p.value", 1), Type::Integer);
    }

    #[test]
    fn generic_method_own_type_param_instantiates() {
        // A method-level `<U>`: `pick<U>(U x) -> U` returns its arg type,
        // independent of the class's `T`.
        let text = "class Bag<T> { U pick<U>(U x) { return x } }\nBag<integer> g = new Bag()\nvar s = g.pick(\"hi\")\n";
        let r = run_generics_parsed(text);
        assert_eq!(ty_at(&r, text, "g.pick(\"hi\")", 1), Type::String);
    }

    #[test]
    fn user_generic_push_joins_element_type() {
        // `push<T>(Array<T>, T) -> Array<T>` joins the array element and
        // the pushed value, widening integer+real to a real array.
        let text = "function push<T>(Array<T> a, T v) -> Array<T> { return a }\nvar r = push([1, 2], 3.5)\n";
        let r = run_generics_parsed(text);
        assert_eq!(
            ty_at(&r, text, "push([1, 2], 3.5)", 1),
            Type::Array(Box::new(Type::Real))
        );
    }
}
