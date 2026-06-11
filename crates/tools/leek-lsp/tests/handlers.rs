//! End-to-end exercise of each handler against an in-memory
//! Workspace. Avoids the JSON-RPC layer so we can assert on
//! concrete LSP types directly.

use leek_lsp::handlers::{
    call_hierarchy, code_action, code_lens, completion, definition, document_color,
    document_highlight, document_link, execute_command, file_operations, folding, formatting,
    hover, implementation, inlay_hints, inline_values, linked_editing, on_type_formatting,
    prepare_rename, pull_diagnostics, references, rename, selection_range, semantic_tokens,
    symbols, type_definition, type_hierarchy, workspace_symbols,
};
use leek_lsp::workspace::Workspace;
use tower_lsp::lsp_types as lsp;

fn url() -> lsp::Url {
    lsp::Url::parse("file:///fixture.leek").unwrap()
}

fn open(text: &str) -> Workspace {
    let mut ws = Workspace::default();
    ws.open(url(), text.to_string());
    ws
}

#[test]
fn document_symbol_lists_top_level_decls() {
    let text = "var x = 5;\nfunction foo() {}\nclass Bar {}\n";
    let ws = open(text);
    let res = symbols::handle(&ws, &url()).expect("response");
    let lsp::DocumentSymbolResponse::Nested(syms) = res else {
        panic!("expected nested response");
    };
    let names: Vec<_> = syms.iter().map(|s| s.name.clone()).collect();
    assert!(names.contains(&"x".to_string()));
    assert!(names.contains(&"foo".to_string()));
    assert!(names.contains(&"Bar".to_string()));
    let kinds: Vec<_> = syms.iter().map(|s| s.kind).collect();
    assert!(kinds.contains(&lsp::SymbolKind::VARIABLE));
    assert!(kinds.contains(&lsp::SymbolKind::FUNCTION));
    assert!(kinds.contains(&lsp::SymbolKind::CLASS));
}

#[test]
fn hover_on_integer_literal_returns_integer() {
    let text = "var x = 5;";
    let ws = open(text);
    // Cursor on the `5`.
    let pos = lsp::Position {
        line: 0,
        character: 8,
    };
    let hover = hover::handle(&ws, &url(), pos).expect("hover");
    let lsp::HoverContents::Markup(m) = hover.contents else {
        panic!("expected markdown contents");
    };
    assert!(
        m.value.contains("integer"),
        "expected 'integer' in hover, got: {}",
        m.value
    );
}

#[test]
fn hover_on_function_decl_renders_full_signature() {
    let text = "function add(integer a, integer b) -> integer { return a + b }\n";
    let ws = open(text);
    // Cursor on the `add` name (after `function `).
    let pos = lsp::Position {
        line: 0,
        character: 9,
    };
    let hover = hover::handle(&ws, &url(), pos).expect("hover");
    let lsp::HoverContents::Markup(m) = hover.contents else {
        panic!("expected markdown contents");
    };
    assert!(
        m.value
            .contains("function add(integer a, integer b) -> integer"),
        "expected full signature, got: {}",
        m.value
    );
}

#[test]
fn hover_builtin_call_shows_library_signature() {
    // A builtin call resolves to a `Builtin` symbol with no user
    // declaration; hover should pull its typed signature from the
    // embedded `stdlib.leek` header rather than show `builtin count`.
    let src = "var a = [1, 2, 3]\nvar n = count(a)\n";
    let v = hover_text(src, "count(a)", 1);
    assert!(
        v.contains("function count("),
        "builtin should show its .leek signature, got: {v}"
    );
    assert!(
        !v.contains("builtin count"),
        "should not fall back to the bare `builtin` label, got: {v}"
    );
}

#[test]
fn hover_leekwars_function_shows_library_signature() {
    // leek-wars game functions live in `leekwars.leek`; hovering a call
    // should surface the typed signature, same as for stdlib builtins.
    let src = "var l = getLife()\n";
    let v = hover_text(src, "getLife()", 1);
    assert!(
        v.contains("getLife"),
        "leekwars function should show its .leek signature, got: {v}"
    );
    assert!(
        !v.contains("<NONE>"),
        "leekwars function hover should not be empty, got: {v}"
    );
}

#[test]
fn hover_overloaded_builtin_shows_all_overloads() {
    // `abs` has real and integer overloads in stdlib.leek; both lines
    // should appear in the hover code block.
    let src = "var n = abs(-3)\n";
    let v = hover_text(src, "abs(-3)", 1);
    assert!(
        v.matches("function abs(").count() >= 2,
        "expected both abs overloads, got: {v}"
    );
}

#[test]
fn hover_binary_over_builtin_infers_real_type() {
    // With the `.leek` signatures seeded for the LSP, `count(a)` infers
    // `integer`, so the binary expression types as integer rather than
    // falling back to `any`.
    let src = "var a = [1, 2, 3]\nvar n = count(a) + 1\n";
    let v = hover_text(src, "+ 1", 1);
    assert!(
        v.contains("integer"),
        "binary over a builtin should infer integer, got: {v}"
    );
}

#[test]
fn hover_on_function_decl_appends_complexity_row() {
    let text = "function sum(arr) {\n    var t = 0\n    for (var x in arr) { t = t + x }\n    return t\n}\n";
    let ws = open(text);
    // Cursor on `sum` on line 0, column 9.
    let pos = lsp::Position {
        line: 0,
        character: 9,
    };
    let hover = hover::handle(&ws, &url(), pos).expect("hover");
    let lsp::HoverContents::Markup(m) = hover.contents else {
        panic!("expected markdown contents");
    };
    assert!(
        m.value.contains("Complexity:"),
        "expected Complexity row, got: {}",
        m.value
    );
    assert!(
        m.value.contains("O(arr)"),
        "expected O(arr) in hover, got: {}",
        m.value
    );
}

#[test]
fn hover_on_function_call_renders_signature_and_return_type() {
    let text =
        "function add(integer a, integer b) -> integer { return a + b }\nvar n = add(1, 2)\n";
    let ws = open(text);
    // Cursor on the `add` USE on line 2.
    let pos = lsp::Position {
        line: 1,
        character: 9,
    };
    let hover = hover::handle(&ws, &url(), pos).expect("hover");
    let lsp::HoverContents::Markup(m) = hover.contents else {
        panic!("expected markdown contents");
    };
    assert!(
        m.value.contains("function add"),
        "expected signature for call site, got: {}",
        m.value
    );
}

#[test]
fn hover_on_class_decl_renders_class_signature() {
    let text = "class Cat extends Animal {}\n";
    let ws = open(text);
    let pos = lsp::Position {
        line: 0,
        character: 7,
    }; // on `Cat`
    let hover = hover::handle(&ws, &url(), pos).expect("hover");
    let lsp::HoverContents::Markup(m) = hover.contents else {
        panic!("expected markdown contents");
    };
    assert!(
        m.value.contains("class Cat extends Animal"),
        "expected class signature, got: {}",
        m.value
    );
}

#[test]
fn hover_on_class_instance_local_shows_class_type() {
    let text = "class Cat {}\nvar pet = new Cat()\n";
    let ws = open(text);
    // Cursor on the `pet` local (line 2).
    let pos = lsp::Position {
        line: 1,
        character: 4,
    };
    let hover = hover::handle(&ws, &url(), pos).expect("hover");
    let lsp::HoverContents::Markup(m) = hover.contents else {
        panic!("expected markdown contents");
    };
    // The signature is `var pet`; the type carries the class name.
    assert!(
        m.value.contains("Cat"),
        "expected class name in hover, got: {}",
        m.value
    );
}

#[test]
fn hover_includes_doc_comment_above_function() {
    let text = "// Adds two integers.\n// Returns the sum.\nfunction add(integer a, integer b) -> integer { return a + b }\n";
    let ws = open(text);
    // Cursor on the `add` name (line 3, after `function `).
    let pos = lsp::Position {
        line: 2,
        character: 9,
    };
    let hover = hover::handle(&ws, &url(), pos).expect("hover");
    let lsp::HoverContents::Markup(m) = hover.contents else {
        panic!("expected markdown contents");
    };
    assert!(
        m.value.contains("Adds two integers."),
        "expected first doc line, got: {}",
        m.value
    );
    assert!(
        m.value.contains("Returns the sum."),
        "expected second doc line, got: {}",
        m.value
    );
}

#[test]
fn hover_includes_javadoc_block_above_class() {
    let text = "/**\n * A feline pet.\n */\nclass Cat {}\n";
    let ws = open(text);
    // Cursor on `Cat`.
    let pos = lsp::Position {
        line: 3,
        character: 7,
    };
    let hover = hover::handle(&ws, &url(), pos).expect("hover");
    let lsp::HoverContents::Markup(m) = hover.contents else {
        panic!("expected markdown contents");
    };
    assert!(
        m.value.contains("A feline pet."),
        "expected javadoc body, got: {}",
        m.value
    );
}

#[test]
fn hover_on_typed_var_shows_declared_type() {
    let text = "integer apples = 5\n";
    let ws = open(text);
    let pos = lsp::Position {
        line: 0,
        character: 9,
    }; // on `apples`
    let hover = hover::handle(&ws, &url(), pos).expect("hover");
    let lsp::HoverContents::Markup(m) = hover.contents else {
        panic!("expected markdown contents");
    };
    assert!(
        m.value.contains("integer apples"),
        "expected typed-var signature, got: {}",
        m.value
    );
}

#[test]
fn definition_jumps_to_var_declaration() {
    let text = "var apple = 5;\nvar n = apple;\n";
    let ws = open(text);
    // Cursor on the use of `apple` on line 2.
    let pos = lsp::Position {
        line: 1,
        character: 10,
    };
    let resp = definition::handle(&ws, &url(), pos).expect("definition");
    let lsp::GotoDefinitionResponse::Scalar(loc) = resp else {
        panic!("expected scalar response");
    };
    // The decl's identifier sits on line 0 starting at character 4
    // (after `var `).
    assert_eq!(loc.uri, url());
    assert_eq!(loc.range.start.line, 0);
    assert_eq!(loc.range.start.character, 4);
}

#[test]
fn definition_on_decl_itself_returns_same_location() {
    let text = "var apple = 5;\n";
    let ws = open(text);
    let pos = lsp::Position {
        line: 0,
        character: 4,
    };
    let resp = definition::handle(&ws, &url(), pos).expect("definition");
    let lsp::GotoDefinitionResponse::Scalar(loc) = resp else {
        panic!("expected scalar response");
    };
    assert_eq!(loc.range.start.line, 0);
    assert_eq!(loc.range.start.character, 4);
}

#[test]
fn references_lists_decl_and_uses() {
    let text = "var apple = 5;\nvar a = apple;\nvar b = apple + 1;\n";
    let ws = open(text);
    let pos = lsp::Position {
        line: 1,
        character: 10,
    }; // on `apple` use
    let locs = references::handle(&ws, &url(), pos, /* include_decl */ true).expect("references");
    // 1 declaration + 2 uses
    assert_eq!(locs.len(), 3, "got {locs:#?}");
}

#[test]
fn rename_produces_edit_for_each_occurrence() {
    let text = "var apple = 5;\nvar n = apple;\n";
    let ws = open(text);
    let pos = lsp::Position {
        line: 0,
        character: 4,
    };
    let edit = rename::handle(&ws, &url(), pos, "pear").expect("rename");
    let edits = edit.changes.unwrap().remove(&url()).unwrap();
    assert_eq!(edits.len(), 2, "decl + 1 use");
    assert!(edits.iter().all(|e| e.new_text == "pear"));
}

#[test]
fn completion_lists_in_scope_locals_and_keywords() {
    let text = "var apple = 5;\n";
    let ws = open(text);
    let resp = completion::handle(
        &ws,
        &url(),
        lsp::Position {
            line: 1,
            character: 0,
        },
    )
    .expect("completion");
    let lsp::CompletionResponse::Array(items) = resp else {
        panic!("expected array");
    };
    let labels: Vec<_> = items.iter().map(|i| i.label.clone()).collect();
    assert!(labels.contains(&"apple".to_string()));
    assert!(labels.contains(&"if".to_string()));
    assert!(labels.contains(&"function".to_string()));
}

#[test]
fn folding_finds_function_body() {
    let text = "function add(a, b) {\n  return a + b;\n}\n";
    let ws = open(text);
    let folds = folding::handle(&ws, &url()).expect("folding");
    // The function body spans lines 0..2 (or 1..2 depending on
    // where the Block starts). Just assert at least one fold exists.
    assert!(!folds.is_empty(), "expected at least one fold");
}

#[test]
fn semantic_tokens_emits_at_least_one_token() {
    let text = "var apple = 5;\nvar n = apple;\n";
    let mut ws = open(text);
    let resp = semantic_tokens::handle(&mut ws, &url()).expect("semanticTokens");
    let lsp::SemanticTokensResult::Tokens(t) = resp else {
        panic!("expected raw tokens");
    };
    // decl(apple) + decl(n) + ref(apple) = 3 tokens
    assert_eq!(t.data.len(), 3, "got {:#?}", t.data);
}

#[test]
fn workspace_symbols_searches_open_docs() {
    let text = "var apple = 5;\nfunction harvest() {}\nclass Orchard {}\n";
    let ws = open(text);
    let syms = workspace_symbols::handle(&ws, "ar").expect("workspace symbol");
    // Substring "ar" matches "harvest" and "Orchard".
    let names: Vec<_> = syms.iter().map(|s| s.name.clone()).collect();
    assert!(names.contains(&"harvest".to_string()), "got {names:?}");
    assert!(names.contains(&"Orchard".to_string()), "got {names:?}");
}

// ─── new handlers (slice 3) ────────────────────────────────────────

#[test]
fn completion_after_dot_resolves_typed_variable_to_class_members() {
    let text = "class Cat {\n    integer age\n    meow() { return 1 }\n}\nvar c = new Cat()\nc.\n";
    let ws = open(text);
    // Cursor right after `c.` on line 5 col 2.
    let pos = lsp::Position {
        line: 5,
        character: 2,
    };
    let resp = completion::handle(&ws, &url(), pos).expect("completions");
    let items = match resp {
        lsp::CompletionResponse::Array(v) => v,
        lsp::CompletionResponse::List(l) => l.items,
    };
    let labels: Vec<&str> = items.iter().map(|i| i.label.as_str()).collect();
    assert!(labels.contains(&"age"), "expected `age` in: {labels:?}");
    assert!(labels.contains(&"meow"), "expected `meow` in: {labels:?}");
}

#[test]
fn type_definition_jumps_to_class_decl() {
    let text = "class Cat {}\nvar c = new Cat()\nreturn c\n";
    let ws = open(text);
    // Cursor on the `c` USE on line 2, col 7.
    let pos = lsp::Position {
        line: 2,
        character: 7,
    };
    let resp = type_definition::handle(&ws, &url(), pos).expect("got resp");
    let lsp::request::GotoTypeDefinitionResponse::Scalar(loc) = resp else {
        panic!("expected scalar response");
    };
    // Cat is declared on line 0.
    assert_eq!(loc.range.start.line, 0);
}

#[test]
fn selection_range_chain_walks_outward() {
    let text = "function f() {\n    return 1 + 2\n}\n";
    let ws = open(text);
    // Cursor on the `1`.
    let pos = lsp::Position {
        line: 1,
        character: 11,
    };
    let chain = selection_range::handle(&ws, &url(), vec![pos]).expect("chain");
    let head = &chain[0];
    // Walk parents; the chain should hit at least 3 distinct
    // ranges (token, expr, stmt, fn, file).
    let mut count = 1usize;
    let mut cur = head.parent.as_deref();
    while let Some(p) = cur {
        count += 1;
        cur = p.parent.as_deref();
    }
    assert!(count >= 3, "expected ≥3 nested ranges, got {count}");
}

#[test]
fn document_link_surfaces_include_targets() {
    let text = "include(\"helpers\")\nreturn 0\n";
    let ws = open(text);
    let links = document_link::handle(&ws, &url()).expect("links");
    assert!(!links.is_empty());
    assert!(
        links[0]
            .tooltip
            .as_deref()
            .unwrap_or("")
            .contains("helpers"),
        "tooltip: {:?}",
        links[0].tooltip,
    );
}

#[test]
fn prepare_rename_returns_range_on_ident() {
    let text = "function foo() {}\nfoo()\n";
    let ws = open(text);
    // Cursor on the `foo` call site, line 1 col 1.
    let pos = lsp::Position {
        line: 1,
        character: 1,
    };
    let resp = prepare_rename::handle(&ws, &url(), pos).expect("prepare");
    match resp {
        lsp::PrepareRenameResponse::Range(_) => {}
        other => panic!("expected Range, got {other:?}"),
    }
}

#[test]
fn prepare_rename_refuses_on_keyword() {
    let text = "var x = 1\n";
    let ws = open(text);
    // Cursor on `var`.
    let pos = lsp::Position {
        line: 0,
        character: 1,
    };
    assert!(prepare_rename::handle(&ws, &url(), pos).is_none());
}

#[test]
fn call_hierarchy_prepare_finds_function() {
    let text = "function inner() { return 1 }\nfunction outer() { return inner() }\n";
    let ws = open(text);
    let pos = lsp::Position {
        line: 1,
        character: 9,
    }; // on `outer`
    let items = call_hierarchy::prepare(&ws, &url(), pos).expect("items");
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].name, "outer");
}

#[test]
fn call_hierarchy_incoming_finds_callers() {
    let text = "function inner() { return 1 }\nfunction outer() { return inner() + inner() }\n";
    let ws = open(text);
    let pos = lsp::Position {
        line: 0,
        character: 9,
    }; // on `inner`
    let items = call_hierarchy::prepare(&ws, &url(), pos).expect("items");
    let calls = call_hierarchy::incoming(&ws, &url(), &items[0]).expect("incoming");
    assert!(
        calls.iter().any(|c| c.from.name == "outer"),
        "incoming: {calls:?}"
    );
}

#[test]
fn call_hierarchy_outgoing_finds_callees() {
    let text = "function helper() { return 1 }\nfunction main() { return helper() }\n";
    let ws = open(text);
    let pos = lsp::Position {
        line: 1,
        character: 9,
    }; // on `main`
    let items = call_hierarchy::prepare(&ws, &url(), pos).expect("items");
    let calls = call_hierarchy::outgoing(&ws, &url(), &items[0]).expect("outgoing");
    assert!(
        calls.iter().any(|c| c.to.name == "helper"),
        "outgoing: {calls:?}"
    );
}

#[test]
fn type_hierarchy_walks_extends_chain() {
    let text = "class Animal {}\nclass Cat extends Animal {}\n";
    let ws = open(text);
    let pos = lsp::Position {
        line: 1,
        character: 6,
    }; // on `Cat`
    let items = type_hierarchy::prepare(&ws, &url(), pos).expect("prepare");
    assert_eq!(items[0].name, "Cat");
    let supers = type_hierarchy::supertypes(&ws, &url(), &items[0]).expect("supers");
    assert!(
        supers.iter().any(|c| c.name == "Animal"),
        "supers: {supers:?}"
    );

    // Subtypes from Animal should include Cat.
    let animal_pos = lsp::Position {
        line: 0,
        character: 6,
    };
    let animal_items = type_hierarchy::prepare(&ws, &url(), animal_pos).expect("prepare animal");
    let subs = type_hierarchy::subtypes(&ws, &url(), &animal_items[0]).expect("subs");
    assert!(subs.iter().any(|c| c.name == "Cat"), "subs: {subs:?}");
}

#[test]
fn code_lens_emits_reference_count_and_cost() {
    let text = "function foo() { return 1 }\nfoo()\nfoo()\n";
    let ws = open(text);
    let lenses = code_lens::handle(&ws, &url()).expect("lenses");
    // Expect a "N references" lens and a complexity-derived lens. `foo`
    // is constant-cost, so the latter shows the operation count rather
    // than `Complexity: O(1)`.
    let has_ref_lens = lenses.iter().any(|l| {
        l.command
            .as_ref()
            .is_some_and(|c| c.title.contains("references"))
    });
    let cost = lenses
        .iter()
        .find_map(|l| {
            l.command
                .as_ref()
                .filter(|c| c.command == "leek.showComplexity")
        })
        .expect("a complexity-derived lens");
    assert!(has_ref_lens, "no reference lens: {lenses:?}");
    assert!(
        cost.title.contains("Cost:") && cost.title.contains("operations"),
        "constant fn should show ops cost, got: {}",
        cost.title
    );
}

#[test]
fn code_lens_shows_big_o_for_non_constant() {
    let text =
        "function sum(arr) {\n  var t = 0\n  for (var x in arr) { t = t + x }\n  return t\n}\n";
    let ws = open(text);
    let lenses = code_lens::handle(&ws, &url()).expect("lenses");
    let lens = lenses
        .iter()
        .find_map(|l| {
            l.command
                .as_ref()
                .filter(|c| c.command == "leek.showComplexity")
        })
        .expect("complexity lens");
    assert!(
        lens.title.contains("Complexity:") && lens.title.contains("O(arr)"),
        "non-constant fn should show big-O, got: {}",
        lens.title
    );
}

#[test]
fn document_color_surfaces_color_calls() {
    let text = "var c = color(255, 128, 0)\n";
    let ws = open(text);
    let colors = document_color::handle(&ws, &url()).expect("colors");
    assert!(!colors.is_empty(), "expected at least one color");
    // Red ≈ 1.0, green ≈ 0.5, blue ≈ 0.
    let c = &colors[0].color;
    assert!((c.red - 1.0).abs() < 0.01, "red: {}", c.red);
    assert!((c.blue - 0.0).abs() < 0.01, "blue: {}", c.blue);
}

#[test]
fn document_color_surfaces_hex_literals() {
    let text = "var c = 0xFF00FF\n";
    let ws = open(text);
    let colors = document_color::handle(&ws, &url()).expect("colors");
    assert!(!colors.is_empty(), "expected one hex color");
    let c = &colors[0].color;
    assert!((c.red - 1.0).abs() < 0.01);
    assert!((c.green - 0.0).abs() < 0.01);
    assert!((c.blue - 1.0).abs() < 0.01);
}

#[test]
fn color_presentation_offers_two_forms() {
    let ws = open("");
    let color = lsp::Color {
        red: 1.0,
        green: 0.5,
        blue: 0.0,
        alpha: 1.0,
    };
    let range = lsp::Range {
        start: lsp::Position {
            line: 0,
            character: 0,
        },
        end: lsp::Position {
            line: 0,
            character: 0,
        },
    };
    let presentations = document_color::presentations(&ws, color, range);
    assert_eq!(presentations.len(), 2);
    assert!(presentations[0].label.starts_with("color("));
    assert!(presentations[1].label.starts_with("0x"));
}

// ─── slice 4: more LSP features ────────────────────────────────────

#[test]
fn implementation_lists_subclass_method_overrides() {
    let text = "\
class Animal {\n    speak() { return 0 }\n}\n\
class Cat extends Animal {\n    speak() { return 1 }\n}\n\
class Dog extends Animal {\n    speak() { return 2 }\n}\n";
    let ws = open(text);
    // Cursor on the `speak` method of Animal (line 1, col 6).
    let pos = lsp::Position {
        line: 1,
        character: 6,
    };
    let resp = implementation::handle(&ws, &url(), pos).expect("impl resp");
    let lsp::request::GotoImplementationResponse::Array(locs) = resp else {
        panic!("expected array");
    };
    // Expect two overrides — one for Cat, one for Dog.
    assert!(locs.len() >= 2, "expected ≥2 overrides, got {locs:?}");
}

#[test]
fn implementation_on_class_lists_subclasses() {
    let text = "class Animal {}\nclass Cat extends Animal {}\nclass Dog extends Animal {}\n";
    let ws = open(text);
    // Cursor on `Animal` (line 0, col 6).
    let pos = lsp::Position {
        line: 0,
        character: 6,
    };
    let resp = implementation::handle(&ws, &url(), pos).expect("impl resp");
    let lsp::request::GotoImplementationResponse::Array(locs) = resp else {
        panic!("expected array");
    };
    assert_eq!(locs.len(), 2, "expected 2 subclasses, got {locs:?}");
}

#[test]
fn on_type_formatting_returns_edits_for_close_brace() {
    let text = "function f() {\n    return 1\n    }\n";
    let ws = open(text);
    // Cursor right after the `}` on line 2.
    let pos = lsp::Position {
        line: 2,
        character: 5,
    };
    let edits = on_type_formatting::handle(&ws, &url(), pos, "}");
    // Empty edits is acceptable (already well-formed); we mainly
    // assert we don't panic and the response is Some(_).
    assert!(edits.is_some());
}

#[test]
fn execute_command_show_complexity_returns_formula() {
    let text = "function sum(arr) {\n    var t = 0\n    for (var x in arr) { t = t + x }\n    return t\n}\n";
    let ws = open(text);
    let result = execute_command::handle(
        &ws,
        "leek.showComplexity",
        &[
            serde_json::Value::String(url().to_string()),
            serde_json::Value::String("sum".into()),
        ],
    );
    let s = result.expect("got response").as_str().unwrap().to_string();
    assert!(s.contains("O(arr)"), "expected complexity in: {s}");
    assert!(s.contains("ops"), "expected ops in: {s}");
}

#[test]
fn execute_command_analyze_returns_array() {
    let text = "function f() { return 1 }\n";
    let ws = open(text);
    let result = execute_command::handle(
        &ws,
        "leek.analyze",
        &[serde_json::Value::String(url().to_string())],
    );
    let arr = result.expect("got response");
    assert!(arr.is_array(), "expected array, got {arr}");
}

#[test]
fn execute_command_unknown_returns_none() {
    let ws = open("");
    let r = execute_command::handle(&ws, "leek.unknown", &[]);
    assert!(r.is_none());
}

#[test]
fn pull_diagnostics_reports_parse_errors() {
    // Use a body that produces a real diagnostic.
    let text = "function f( { return 1 }\n";
    let ws = open(text);
    let report = pull_diagnostics::handle_textdoc(&ws, &url());
    let lsp::DocumentDiagnosticReportResult::Report(lsp::DocumentDiagnosticReport::Full(full)) =
        report
    else {
        panic!("expected full report");
    };
    assert!(
        !full.full_document_diagnostic_report.items.is_empty(),
        "expected ≥1 diagnostic"
    );
    let item = &full.full_document_diagnostic_report.items[0];
    assert_eq!(item.code, Some(lsp::NumberOrString::String("E0100".into())));
    assert!(item.code_description.is_some());
    assert_eq!(item.source.as_deref(), Some("leek"));
}

#[test]
fn pull_diagnostics_maps_catalog_code_description() {
    let text = "class C { m() { return __miku_test_missing_symbol; } }\n";
    let ws = open(text);
    let report = pull_diagnostics::handle_textdoc(&ws, &url());
    let lsp::DocumentDiagnosticReportResult::Report(lsp::DocumentDiagnosticReport::Full(full)) =
        report
    else {
        panic!("expected full report");
    };
    let items = &full.full_document_diagnostic_report.items;
    let unknown = items
        .iter()
        .find(|d| d.code == Some(lsp::NumberOrString::String("E0200".into())))
        .expect("expected UnknownVariable diagnostic");
    let desc = unknown.code_description.as_ref().expect("code_description");
    assert!(
        desc.href.as_str().contains("diagnostics"),
        "unexpected href: {}",
        desc.href
    );
}

#[test]
fn pull_diagnostics_workspace_enumerates_open_docs() {
    let ws = open("function ok() { return 1 }\n");
    let report = pull_diagnostics::handle_workspace(&ws);
    let lsp::WorkspaceDiagnosticReportResult::Report(rep) = report else {
        panic!("expected report");
    };
    assert_eq!(rep.items.len(), 1);
}

#[test]
fn code_lens_now_carries_clickable_command() {
    let text = "function foo() { return 1 }\nfoo()\nfoo()\n";
    let ws = open(text);
    let lenses = code_lens::handle(&ws, &url()).expect("lenses");
    // Both the "X references" and "Complexity: O(...)" lenses
    // should now have a non-empty `command.command`.
    let mut saw_show_refs = false;
    let mut saw_show_complexity = false;
    for lens in &lenses {
        let Some(cmd) = &lens.command else { continue };
        if cmd.command == "leek.showReferences" {
            saw_show_refs = true;
        }
        if cmd.command == "leek.showComplexity" {
            saw_show_complexity = true;
        }
    }
    assert!(saw_show_refs, "lenses: {lenses:?}");
    assert!(saw_show_complexity, "lenses: {lenses:?}");
}

// ─── slice 5: linked editing, semantic range/delta, resolve, file ops ──

#[test]
fn linked_editing_groups_symbol_occurrences() {
    let text = "var total = 0\ntotal = total + 1\nreturn total\n";
    let ws = open(text);
    // Cursor on the declaration `total`.
    let res = linked_editing::handle(
        &ws,
        &url(),
        lsp::Position {
            line: 0,
            character: 4,
        },
    )
    .expect("linked ranges");
    // decl + lhs + rhs + return = 4 occurrences.
    assert_eq!(res.ranges.len(), 4, "ranges: {:?}", res.ranges);
    assert!(res.word_pattern.is_some());
}

#[test]
fn semantic_tokens_range_only_returns_requested_lines() {
    let text = "var a = 1\nvar b = 2\nvar c = 3\n";
    let ws = open(text);
    let range = lsp::Range {
        start: lsp::Position {
            line: 1,
            character: 0,
        },
        end: lsp::Position {
            line: 2,
            character: 0,
        },
    };
    let lsp::SemanticTokensRangeResult::Tokens(t) =
        semantic_tokens::handle_range(&ws, &url(), range).expect("range tokens")
    else {
        panic!("expected raw tokens");
    };
    assert_eq!(t.data.len(), 1, "only `b` on line 1: {:?}", t.data);
}

#[test]
fn semantic_tokens_delta_emits_edits_after_change() {
    let text = "var apple = 5\nvar n = apple\n";
    let mut ws = open(text);
    let lsp::SemanticTokensResult::Tokens(first) =
        semantic_tokens::handle(&mut ws, &url()).expect("full")
    else {
        panic!("tokens");
    };
    let id = first.result_id.expect("result id");
    // Re-querying the delta with no change yields no edits.
    let delta = semantic_tokens::handle_delta(&mut ws, &url(), &id).expect("delta");
    let lsp::SemanticTokensFullDeltaResult::TokensDelta(d) = delta else {
        panic!("expected delta");
    };
    assert!(d.edits.is_empty(), "unchanged → no edits: {:?}", d.edits);
}

#[test]
fn completion_resolve_adds_documentation() {
    let text = "// Doubles its argument.\nfunction twice(integer x) -> integer { return x * 2 }\n";
    let ws = open(text);
    let resp = completion::handle(
        &ws,
        &url(),
        lsp::Position {
            line: 2,
            character: 0,
        },
    )
    .expect("completion");
    let items = match resp {
        lsp::CompletionResponse::Array(v) => v,
        lsp::CompletionResponse::List(l) => l.items,
    };
    let twice = items
        .iter()
        .find(|i| i.label == "twice")
        .cloned()
        .expect("twice item");
    assert!(twice.documentation.is_none(), "docs deferred");
    let resolved = completion::resolve(&ws, twice);
    let lsp::Documentation::MarkupContent(m) = resolved.documentation.expect("docs") else {
        panic!("expected markup");
    };
    assert!(m.value.contains("Doubles its argument."), "{}", m.value);
}

#[test]
fn inlay_hint_resolve_adds_tooltip() {
    let text = "var n = 1 + 2\n";
    let ws = open(text);
    let range = lsp::Range {
        start: lsp::Position {
            line: 0,
            character: 0,
        },
        end: lsp::Position {
            line: 10,
            character: 0,
        },
    };
    let hint = inlay_hints::handle(&ws, &url(), range)
        .expect("hints")
        .remove(0);
    assert!(hint.tooltip.is_none());
    let resolved = inlay_hints::resolve(hint);
    assert!(resolved.tooltip.is_some(), "tooltip filled on resolve");
}

#[test]
fn will_rename_rewrites_include_references() {
    let mut ws = Workspace::default();
    let main = lsp::Url::parse("file:///proj/main.leek").unwrap();
    ws.open(main.clone(), "include(\"helpers\")\nreturn 0\n".to_string());
    let renames = vec![(
        "file:///proj/helpers.leek".to_string(),
        "file:///proj/util.leek".to_string(),
    )];
    let edit = file_operations::will_rename(&ws, &renames).expect("workspace edit");
    let edits = edit.changes.unwrap().remove(&main).unwrap();
    assert_eq!(edits.len(), 1);
    assert_eq!(edits[0].new_text, "util");
}

// ─── slice 6: workspace-wide references & rename ───────────────────

/// Open several named files (siblings under `/proj/`) in one workspace.
fn open_files(files: &[(&str, &str)]) -> Workspace {
    let mut ws = Workspace::default();
    for (name, src) in files {
        let uri = lsp::Url::parse(&format!("file:///proj/{name}")).unwrap();
        ws.open(uri, src.to_string());
    }
    ws
}

fn proj(name: &str) -> lsp::Url {
    lsp::Url::parse(&format!("file:///proj/{name}")).unwrap()
}

#[test]
fn rename_top_level_function_edits_every_file() {
    // `help` is defined in util.leek and called (via include) in main.leek.
    let ws = open_files(&[
        ("util.leek", "function help() { return 1 }\n"),
        ("main.leek", "include(\"util\")\nvar n = help()\n"),
    ]);
    // Rename from the declaration in util.leek.
    let edit = rename::handle(
        &ws,
        &proj("util.leek"),
        lsp::Position {
            line: 0,
            character: 9,
        },
        "assist",
    )
    .expect("rename");
    let changes = edit.changes.expect("changes");
    // The declaration in util.leek AND the call in main.leek are edited.
    let util_edits = changes.get(&proj("util.leek")).expect("util edited");
    let main_edits = changes.get(&proj("main.leek")).expect("main edited");
    assert_eq!(util_edits.len(), 1);
    assert_eq!(main_edits.len(), 1);
    assert!(util_edits.iter().all(|e| e.new_text == "assist"));
    assert!(main_edits.iter().all(|e| e.new_text == "assist"));
}

#[test]
fn rename_does_not_touch_local_shadow_in_other_file() {
    // a.leek declares top-level `help`; b.leek has an unrelated *local*
    // named `help`. Renaming the top-level function must not edit b.leek.
    let ws = open_files(&[
        ("a.leek", "function help() { return 1 }\n"),
        ("b.leek", "function other() { var help = 2\nreturn help }\n"),
    ]);
    let edit = rename::handle(
        &ws,
        &proj("a.leek"),
        lsp::Position {
            line: 0,
            character: 9,
        },
        "assist",
    )
    .expect("rename");
    let changes = edit.changes.expect("changes");
    assert!(changes.contains_key(&proj("a.leek")), "a.leek edited");
    assert!(
        !changes.contains_key(&proj("b.leek")),
        "the local shadow in b.leek must be left alone: {changes:?}"
    );
}

#[test]
fn rename_does_not_touch_member_access_in_other_file() {
    // a.leek declares top-level `help`; b.leek calls `c.help()` — a
    // method access that is unrelated to the free function.
    let ws = open_files(&[
        ("a.leek", "function help() { return 1 }\n"),
        (
            "b.leek",
            "class C { help() { return 2 } }\nvar c = new C()\nvar r = c.help()\n",
        ),
    ]);
    let edit = rename::handle(
        &ws,
        &proj("a.leek"),
        lsp::Position {
            line: 0,
            character: 9,
        },
        "assist",
    )
    .expect("rename");
    let changes = edit.changes.expect("changes");
    assert!(
        !changes.contains_key(&proj("b.leek")),
        "method + member access in b.leek must be untouched: {changes:?}"
    );
}

#[test]
fn references_for_top_level_symbol_span_files() {
    let ws = open_files(&[
        ("util.leek", "function help() { return 1 }\n"),
        (
            "main.leek",
            "include(\"util\")\nvar n = help()\nvar m = help()\n",
        ),
    ]);
    let locs = references::handle(
        &ws,
        &proj("util.leek"),
        lsp::Position {
            line: 0,
            character: 9,
        },
        /* include_declaration */ true,
    )
    .expect("references");
    // 1 declaration (util) + 2 calls (main) = 3, across two files.
    assert_eq!(locs.len(), 3, "locs: {locs:#?}");
    assert!(locs.iter().any(|l| l.uri == proj("util.leek")));
    assert_eq!(
        locs.iter().filter(|l| l.uri == proj("main.leek")).count(),
        2
    );
}

#[test]
fn local_rename_stays_single_file() {
    // A local inside a function must never escape its file even when a
    // same-named identifier exists elsewhere.
    let ws = open_files(&[
        ("a.leek", "function f() { var x = 1\nreturn x }\n"),
        ("b.leek", "var x = 99\n"),
    ]);
    // Rename the local `x` in a.leek.
    let edit = rename::handle(
        &ws,
        &proj("a.leek"),
        lsp::Position {
            line: 0,
            character: 19,
        },
        "y",
    )
    .expect("rename");
    let changes = edit.changes.expect("changes");
    assert!(
        !changes.contains_key(&proj("b.leek")),
        "local rename must not touch b.leek: {changes:?}"
    );
    assert!(changes.contains_key(&proj("a.leek")));
}

// ─── slice 16: lint autofix as LSP code action ─────────────────────

#[test]
fn code_action_offers_redundant_boolean_autofix() {
    // The `RedundantBoolean` lint ships a machine-applicable fix; it
    // should surface as a quick-fix code action over its range.
    let ws = open("function f(boolean b) {\n  return b == true\n}\n");
    let whole_line2 = lsp::Range {
        start: lsp::Position {
            line: 1,
            character: 0,
        },
        end: lsp::Position {
            line: 1,
            character: 40,
        },
    };
    let ctx = lsp::CodeActionContext::default();
    let actions = code_action::handle(&ws, &url(), whole_line2, &ctx).expect("actions");
    let has_fix = actions.iter().any(|a| match a {
        lsp::CodeActionOrCommand::CodeAction(ca) => {
            ca.kind == Some(lsp::CodeActionKind::QUICKFIX) && ca.edit.is_some()
        }
        lsp::CodeActionOrCommand::Command(_) => false,
    });
    assert!(
        has_fix,
        "expected a quick-fix code action, got: {actions:#?}"
    );
}

#[test]
fn code_action_offers_fix_all_source_action() {
    // Two independent machine-applicable fixes in one file should be
    // bundled into a single `source.fixAll` action covering both.
    let ws =
        open("function f(boolean b, boolean c) {\n  var x = b == true\n  return c == true\n}\n");
    let whole = lsp::Range {
        start: lsp::Position {
            line: 0,
            character: 0,
        },
        end: lsp::Position {
            line: 3,
            character: 0,
        },
    };
    let ctx = lsp::CodeActionContext::default();
    let actions = code_action::handle(&ws, &url(), whole, &ctx).expect("actions");
    let fix_all = actions.iter().find_map(|a| match a {
        lsp::CodeActionOrCommand::CodeAction(ca)
            if ca.kind == Some(lsp::CodeActionKind::SOURCE_FIX_ALL) =>
        {
            Some(ca)
        }
        _ => None,
    });
    let fix_all = fix_all.expect("expected a source.fixAll action");
    let edits = fix_all
        .edit
        .as_ref()
        .and_then(|e| e.changes.as_ref())
        .and_then(|c| c.values().next())
        .expect("fix-all should carry edits");
    assert!(
        edits.len() >= 2,
        "fix-all should bundle both `== true` fixes, got {edits:#?}"
    );
}

#[test]
fn code_action_source_only_filter_excludes_quickfix() {
    // A client requesting only `source.fixAll` must not receive the
    // range-scoped quick fixes.
    let ws = open("function f(boolean b) {\n  return b == true\n}\n");
    let whole_line2 = lsp::Range {
        start: lsp::Position {
            line: 1,
            character: 0,
        },
        end: lsp::Position {
            line: 1,
            character: 40,
        },
    };
    let ctx = lsp::CodeActionContext {
        only: Some(vec![lsp::CodeActionKind::SOURCE_FIX_ALL]),
        ..Default::default()
    };
    let actions = code_action::handle(&ws, &url(), whole_line2, &ctx).expect("actions");
    assert!(
        actions.iter().all(|a| match a {
            lsp::CodeActionOrCommand::CodeAction(ca) =>
                ca.kind != Some(lsp::CodeActionKind::QUICKFIX),
            lsp::CodeActionOrCommand::Command(_) => true,
        }),
        "source-only request should not include quickfixes: {actions:#?}"
    );
}

// ─── slice 15: inline values (debug) ───────────────────────────────

#[test]
fn inline_values_emit_variable_lookups() {
    let ws = open("function f() {\n  var total = 0\n  total = total + 5\n}\n");
    let range = lsp::Range {
        start: lsp::Position {
            line: 0,
            character: 0,
        },
        end: lsp::Position {
            line: 4,
            character: 0,
        },
    };
    // Stopped on the assignment line (line 2).
    let stopped = lsp::Range {
        start: lsp::Position {
            line: 2,
            character: 0,
        },
        end: lsp::Position {
            line: 2,
            character: 20,
        },
    };
    let vals = inline_values::handle(&ws, &url(), range, stopped).expect("inline values");
    assert!(!vals.is_empty(), "should surface `total` lookups");
    assert!(vals.iter().all(|v| matches!(
        v,
        lsp::InlineValue::VariableLookup(l) if l.range.start.line <= 2
    )));
}

// ─── slice 14: cross-file goto-implementation ──────────────────────

fn impl_locs(resp: lsp::request::GotoImplementationResponse) -> Vec<lsp::Location> {
    match resp {
        lsp::request::GotoImplementationResponse::Array(v) => v,
        lsp::request::GotoImplementationResponse::Scalar(l) => vec![l],
        lsp::request::GotoImplementationResponse::Link(_) => Vec::new(),
    }
}

#[test]
fn implementation_lists_subclasses_across_include() {
    let ws = open_files(&[
        ("animal.leek", "class Animal {}\n"),
        (
            "cat.leek",
            "include(\"animal\")\nclass Cat extends Animal {}\n",
        ),
        (
            "dog.leek",
            "include(\"animal\")\nclass Dog extends Animal {}\n",
        ),
    ]);
    // Cursor on `Animal` in animal.leek.
    let resp = implementation::handle(
        &ws,
        &proj("animal.leek"),
        lsp::Position {
            line: 0,
            character: 6,
        },
    )
    .expect("impl");
    let locs = impl_locs(resp);
    let uris: Vec<&lsp::Url> = locs.iter().map(|l| &l.uri).collect();
    assert!(uris.contains(&&proj("cat.leek")), "Cat subclass: {uris:?}");
    assert!(uris.contains(&&proj("dog.leek")), "Dog subclass: {uris:?}");
}

#[test]
fn implementation_lists_method_overrides_across_include() {
    let ws = open_files(&[
        (
            "animal.leek",
            "class Animal {\n    speak() { return 0 }\n}\n",
        ),
        (
            "cat.leek",
            "include(\"animal\")\nclass Cat extends Animal {\n    speak() { return 1 }\n}\n",
        ),
    ]);
    // Cursor on the `speak` method of Animal (line 1, col 6).
    let resp = implementation::handle(
        &ws,
        &proj("animal.leek"),
        lsp::Position {
            line: 1,
            character: 6,
        },
    )
    .expect("impl");
    let locs = impl_locs(resp);
    assert!(
        locs.iter().any(|l| l.uri == proj("cat.leek")),
        "Cat's override should be found in cat.leek: {locs:?}"
    );
}

#[test]
fn implementation_does_not_cross_into_independent_program() {
    // Two unrelated trees that both extend a same-named Base. Asking for
    // implementations of a.leek's Base must not reach b.leek's tree.
    let ws = open_files(&[
        ("a.leek", "class Base {}\nclass A extends Base {}\n"),
        ("b.leek", "class Base {}\nclass B extends Base {}\n"),
    ]);
    let resp = implementation::handle(
        &ws,
        &proj("a.leek"),
        lsp::Position {
            line: 0,
            character: 6,
        },
    )
    .expect("impl");
    let locs = impl_locs(resp);
    assert!(
        locs.iter().any(|l| l.uri == proj("a.leek")),
        "a's own subclass A: {locs:?}"
    );
    assert!(
        locs.iter().all(|l| l.uri != proj("b.leek")),
        "must not reach b.leek's independent tree: {locs:?}"
    );
}

// ─── slice 13: cross-file call & type hierarchy ────────────────────

#[test]
fn call_hierarchy_incoming_crosses_include() {
    let ws = open_files(&[
        ("util.leek", "function helper() { return 1 }\n"),
        (
            "main.leek",
            "include(\"util\")\nfunction caller() { return helper() }\n",
        ),
    ]);
    // Prepare on the `helper` declaration in util.
    let items = call_hierarchy::prepare(
        &ws,
        &proj("util.leek"),
        lsp::Position {
            line: 0,
            character: 9,
        },
    )
    .expect("prepare");
    let calls = call_hierarchy::incoming(&ws, &proj("util.leek"), &items[0]).expect("incoming");
    let from = calls
        .iter()
        .find(|c| c.from.name == "caller")
        .expect("caller across include");
    assert_eq!(from.from.uri, proj("main.leek"), "caller lives in main");
    assert!(!from.from_ranges.is_empty(), "call-site ranges present");
}

#[test]
fn call_hierarchy_outgoing_crosses_include() {
    let ws = open_files(&[
        ("util.leek", "function helper() { return 1 }\n"),
        (
            "main.leek",
            "include(\"util\")\nfunction runner() { return helper() }\n",
        ),
    ]);
    // Prepare on `runner` in main.
    let items = call_hierarchy::prepare(
        &ws,
        &proj("main.leek"),
        lsp::Position {
            line: 1,
            character: 9,
        },
    )
    .expect("prepare");
    let calls = call_hierarchy::outgoing(&ws, &proj("main.leek"), &items[0]).expect("outgoing");
    let to = calls
        .iter()
        .find(|c| c.to.name == "helper")
        .expect("cross-file callee");
    assert_eq!(to.to.uri, proj("util.leek"), "callee declared in util");
}

#[test]
fn type_hierarchy_supertypes_cross_include() {
    let ws = open_files(&[
        ("animal.leek", "class Animal {}\n"),
        (
            "cat.leek",
            "include(\"animal\")\nclass Cat extends Animal {}\n",
        ),
    ]);
    // Prepare on `Cat` in cat.leek.
    let items = type_hierarchy::prepare(
        &ws,
        &proj("cat.leek"),
        lsp::Position {
            line: 1,
            character: 6,
        },
    )
    .expect("prepare");
    let supers = type_hierarchy::supertypes(&ws, &proj("cat.leek"), &items[0]).expect("supers");
    let animal = supers
        .iter()
        .find(|s| s.name == "Animal")
        .expect("parent across include");
    assert_eq!(
        animal.uri,
        proj("animal.leek"),
        "parent lives in animal.leek"
    );
}

#[test]
fn type_hierarchy_subtypes_cross_include() {
    let ws = open_files(&[
        ("animal.leek", "class Animal {}\n"),
        (
            "cat.leek",
            "include(\"animal\")\nclass Cat extends Animal {}\n",
        ),
    ]);
    // Prepare on `Animal` in animal.leek; the subclass lives in cat.leek
    // (which includes animal, so it's in animal's program).
    let items = type_hierarchy::prepare(
        &ws,
        &proj("animal.leek"),
        lsp::Position {
            line: 0,
            character: 6,
        },
    )
    .expect("prepare");
    let subs = type_hierarchy::subtypes(&ws, &proj("animal.leek"), &items[0]).expect("subs");
    let cat = subs
        .iter()
        .find(|s| s.name == "Cat")
        .expect("subclass across include");
    assert_eq!(cat.uri, proj("cat.leek"), "subclass lives in cat.leek");
}

#[test]
fn cross_program_hierarchy_stays_separate() {
    // Two independent class trees that never include each other must not
    // see each other's subclasses.
    let ws = open_files(&[
        ("a.leek", "class Base {}\nclass A extends Base {}\n"),
        ("b.leek", "class Base {}\nclass B extends Base {}\n"),
    ]);
    let items = type_hierarchy::prepare(
        &ws,
        &proj("a.leek"),
        lsp::Position {
            line: 0,
            character: 6,
        },
    )
    .expect("prepare");
    let subs = type_hierarchy::subtypes(&ws, &proj("a.leek"), &items[0]).expect("subs");
    let names: Vec<&str> = subs.iter().map(|s| s.name.as_str()).collect();
    assert!(names.contains(&"A"), "a's own subclass: {names:?}");
    assert!(
        !names.contains(&"B"),
        "b's subclass is a different program: {names:?}"
    );
}

// ─── slice 12: workspace-symbol & document-highlight parity ────────

#[test]
fn document_highlight_highlights_cross_file_use_sites() {
    let ws = open_files(&[
        ("util.leek", "function helper() { return 1 }\n"),
        (
            "main.leek",
            "include(\"util\")\nvar n = helper()\nvar m = helper()\n",
        ),
    ]);
    // Cursor on a `helper` use in main (unresolved locally).
    let hls = document_highlight::handle(
        &ws,
        &proj("main.leek"),
        lsp::Position {
            line: 1,
            character: 9,
        },
    )
    .expect("highlights");
    // Both uses in main are highlighted; the declaration (in util) is
    // not, because documentHighlight is document-local.
    assert_eq!(hls.len(), 2, "expected both main uses, got {hls:?}");
    assert!(
        hls.iter()
            .all(|h| h.kind == Some(lsp::DocumentHighlightKind::READ)),
        "cross-file uses are reads: {hls:?}"
    );
}

#[test]
fn document_highlight_ignores_unknown_identifier() {
    let ws = open_files(&[("main.leek", "var n = mystery()\n")]);
    // `mystery` resolves to nothing anywhere → no highlights.
    let hls = document_highlight::handle(
        &ws,
        &proj("main.leek"),
        lsp::Position {
            line: 0,
            character: 9,
        },
    );
    assert!(hls.is_none_or(|h| h.is_empty()), "no highlights");
}

#[test]
fn document_highlight_still_works_single_file() {
    // Regression: a locally-declared symbol still highlights decl+uses.
    let ws = open_files(&[("a.leek", "var counter = 0\ncounter = counter + 1\n")]);
    let hls = document_highlight::handle(
        &ws,
        &proj("a.leek"),
        lsp::Position {
            line: 0,
            character: 4,
        },
    )
    .expect("highlights");
    assert!(hls.len() >= 3, "decl + uses: {hls:?}");
    assert_eq!(
        hls.iter()
            .filter(|h| h.kind == Some(lsp::DocumentHighlightKind::WRITE))
            .count(),
        1,
        "exactly one declaration"
    );
}

#[test]
fn workspace_symbol_sets_container_for_class_members() {
    let ws = open_files(&[(
        "a.leek",
        "class Cat {\n    integer age\n    meow() { return 1 }\n}\n",
    )]);
    let syms = workspace_symbols::handle(&ws, "meow").expect("symbols");
    let meow = syms.iter().find(|s| s.name == "meow").expect("meow");
    assert_eq!(
        meow.container_name.as_deref(),
        Some("Cat"),
        "method should carry its class as container"
    );
}

#[test]
fn workspace_symbol_top_level_has_no_container() {
    let ws = open_files(&[("a.leek", "function harvest() { return 1 }\n")]);
    let syms = workspace_symbols::handle(&ws, "harvest").expect("symbols");
    let f = syms.iter().find(|s| s.name == "harvest").expect("harvest");
    assert_eq!(f.container_name, None, "top-level fn has no container");
}

#[test]
fn workspace_symbol_searches_across_files() {
    let ws = open_files(&[
        ("util.leek", "function helper() { return 1 }\n"),
        ("main.leek", "class Widget {}\n"),
    ]);
    let syms = workspace_symbols::handle(&ws, "e").expect("symbols");
    let names: Vec<&str> = syms.iter().map(|s| s.name.as_str()).collect();
    assert!(names.contains(&"helper"), "from util: {names:?}");
    assert!(names.contains(&"Widget"), "from main: {names:?}");
}

// ─── slice 11: did_change_configuration wiring ─────────────────────

#[test]
fn formatting_uses_configured_indent_style() {
    let mut ws = open("function f() {\nreturn 1\n}\n");
    ws.settings = leek_lsp::settings::Settings::from_value(&serde_json::json!({
        "leek": { "format": { "indentStyle": "tabs" } }
    }));
    let edits = formatting::handle(&ws, &url()).expect("format");
    let out: String = edits.iter().map(|e| e.new_text.clone()).collect();
    assert!(
        out.contains("\treturn"),
        "configured tab indent should be used, got: {out:?}"
    );
}

#[test]
fn inlay_hints_can_be_disabled_via_settings() {
    let mut ws = open("var n = 1 + 2\n");
    let whole = lsp::Range {
        start: lsp::Position {
            line: 0,
            character: 0,
        },
        end: lsp::Position {
            line: 10,
            character: 0,
        },
    };
    // Default-on → at least one inferred-type hint.
    assert!(!inlay_hints::handle(&ws, &url(), whole).unwrap().is_empty());
    // Toggle off via settings → no hints, so the editor clears them.
    ws.settings.inlay_hints = false;
    assert!(inlay_hints::handle(&ws, &url(), whole).unwrap().is_empty());
}

// ─── slice 10: diagnostics version threading ───────────────────────

#[test]
fn document_version_is_tracked_for_diagnostics() {
    let mut ws = Workspace::default();
    let u = url();
    ws.open(u.clone(), "var x = 1\n".to_string());
    // Fresh open defaults to 0 until the client's version is recorded.
    assert_eq!(ws.docs.get(&u).unwrap().version, 0);
    ws.set_doc_version(&u, 7);
    assert_eq!(ws.docs.get(&u).unwrap().version, 7);
    // An incremental edit re-seeds the text; the server bumps the
    // version alongside it (here to 8).
    ws.update(&u, "var x = 2\n".to_string());
    ws.set_doc_version(&u, 8);
    let doc = ws.docs.get(&u).unwrap();
    assert_eq!(doc.version, 8);
    assert_eq!(&*doc.text, "var x = 2\n");
}

// ─── slice 9: use-site cross-file references & rename ──────────────

#[test]
fn references_from_a_cross_file_use_site() {
    // Cursor is on the `helper` *use* in main (which doesn't resolve
    // locally), not on the declaration in util.
    let ws = open_files(&[
        ("util.leek", "function helper() { return 1 }\n"),
        (
            "main.leek",
            "include(\"util\")\nvar n = helper()\nvar m = helper()\n",
        ),
    ]);
    let locs = references::handle(
        &ws,
        &proj("main.leek"),
        lsp::Position {
            line: 1,
            character: 9,
        },
        /* include_declaration */ true,
    )
    .expect("references");
    // Declaration in util + two uses in main.
    assert_eq!(locs.len(), 3, "locs: {locs:#?}");
    assert!(locs.iter().any(|l| l.uri == proj("util.leek")));
    assert_eq!(
        locs.iter().filter(|l| l.uri == proj("main.leek")).count(),
        2
    );
}

#[test]
fn rename_from_a_cross_file_use_site_edits_all_files() {
    let ws = open_files(&[
        ("util.leek", "function helper() { return 1 }\n"),
        ("main.leek", "include(\"util\")\nvar n = helper()\n"),
    ]);
    // Cursor on the use in main, not the declaration.
    let edit = rename::handle(
        &ws,
        &proj("main.leek"),
        lsp::Position {
            line: 1,
            character: 9,
        },
        "assist",
    )
    .expect("rename");
    let changes = edit.changes.expect("changes");
    assert!(changes.contains_key(&proj("util.leek")), "decl edited");
    assert!(changes.contains_key(&proj("main.leek")), "use edited");
    assert!(changes.values().flatten().all(|e| e.new_text == "assist"));
}

#[test]
fn rename_from_use_site_reaches_other_includers() {
    // helper lives in util; both ai1 and ai2 include util and use it.
    // Renaming from ai1's *use site* must still reach ai2, because we
    // anchor the search on util (the declaration), not on ai1's program.
    let ws = open_files(&[
        ("util.leek", "function helper() { return 1 }\n"),
        ("ai1.leek", "include(\"util\")\nvar a = helper()\n"),
        ("ai2.leek", "include(\"util\")\nvar b = helper()\n"),
    ]);
    let edit = rename::handle(
        &ws,
        &proj("ai1.leek"),
        lsp::Position {
            line: 1,
            character: 9,
        },
        "assist",
    )
    .expect("rename");
    let changes = edit.changes.expect("changes");
    assert!(changes.contains_key(&proj("util.leek")), "util decl");
    assert!(changes.contains_key(&proj("ai1.leek")), "ai1 use");
    assert!(
        changes.contains_key(&proj("ai2.leek")),
        "ai2 (another includer) must be reached from ai1's use site: {changes:?}"
    );
}

#[test]
fn prepare_rename_allows_cross_file_use_site() {
    let ws = open_files(&[
        ("util.leek", "function helper() { return 1 }\n"),
        ("main.leek", "include(\"util\")\nvar n = helper()\n"),
    ]);
    // Cursor on the `helper` use (line 1, col 8..14).
    let resp = prepare_rename::handle(
        &ws,
        &proj("main.leek"),
        lsp::Position {
            line: 1,
            character: 9,
        },
    )
    .expect("prepare allows it");
    let lsp::PrepareRenameResponse::Range(r) = resp else {
        panic!("expected a range");
    };
    // The reported range covers the use-site identifier on line 1.
    assert_eq!(r.start.line, 1);
    assert_eq!(r.start.character, 8);
    assert_eq!(r.end.character, 14);
}

#[test]
fn prepare_rename_rejects_cross_file_member_access() {
    // `c.helper()` is a member access — not the free function, so a
    // cross-file rename must not engage here.
    let ws = open_files(&[
        ("util.leek", "function helper() { return 1 }\n"),
        (
            "main.leek",
            "class C { helper() { return 2 } }\nvar c = new C()\nvar r = c.helper()\n",
        ),
    ]);
    // Cursor on `helper` in `c.helper()` (line 2).
    let resp = prepare_rename::handle(
        &ws,
        &proj("main.leek"),
        lsp::Position {
            line: 2,
            character: 11,
        },
    );
    // The method is locally resolved, so prepare returns *its* range —
    // but a rename here must stay the method, never the free function.
    // What we assert is that it does not point into util.leek's program
    // by checking the range is on line 2 (the member-access site) only.
    if let Some(lsp::PrepareRenameResponse::Range(r)) = resp {
        assert_eq!(r.start.line, 2, "must stay on the member-access site");
    }
}

// ─── slice 8: cross-file navigation (definition + hover) ───────────

#[test]
fn definition_jumps_across_include_to_home_file() {
    let ws = open_files(&[
        ("util.leek", "function helper() { return 1 }\n"),
        ("main.leek", "include(\"util\")\nvar n = helper()\n"),
    ]);
    // Cursor on the `helper` call in main.leek (line 1, col 8).
    let resp = definition::handle(
        &ws,
        &proj("main.leek"),
        lsp::Position {
            line: 1,
            character: 9,
        },
    )
    .expect("definition");
    let lsp::GotoDefinitionResponse::Scalar(loc) = resp else {
        panic!("expected scalar");
    };
    // It should point at the declaration in util.leek, not main.leek.
    assert_eq!(loc.uri, proj("util.leek"), "should cross into util.leek");
    assert_eq!(loc.range.start.line, 0);
}

#[test]
fn definition_prefers_local_symbol_over_cross_file() {
    // A local shadow in main must win over a same-named include symbol.
    let ws = open_files(&[
        ("util.leek", "function helper() { return 1 }\n"),
        (
            "main.leek",
            "include(\"util\")\nfunction f() { var helper = 2\nreturn helper }\n",
        ),
    ]);
    // Cursor on the `helper` use inside f (line 2, col 7).
    let resp = definition::handle(
        &ws,
        &proj("main.leek"),
        lsp::Position {
            line: 2,
            character: 7,
        },
    )
    .expect("definition");
    let lsp::GotoDefinitionResponse::Scalar(loc) = resp else {
        panic!("expected scalar");
    };
    // Resolves to the local in main.leek, never crossing into util.
    assert_eq!(loc.uri, proj("main.leek"), "local shadow must win");
}

#[test]
fn hover_resolves_cross_file_function_signature() {
    let ws = open_files(&[
        (
            "util.leek",
            "// Doubles its argument.\nfunction twice(integer x) -> integer { return x * 2 }\n",
        ),
        ("main.leek", "include(\"util\")\nvar n = twice(3)\n"),
    ]);
    // Hover the `twice` call in main.leek.
    let hover = hover::handle(
        &ws,
        &proj("main.leek"),
        lsp::Position {
            line: 1,
            character: 9,
        },
    )
    .expect("hover");
    let lsp::HoverContents::Markup(m) = hover.contents else {
        panic!("expected markup");
    };
    assert!(
        m.value.contains("function twice(integer x) -> integer"),
        "cross-file hover should show the signature, got: {}",
        m.value
    );
    assert!(
        m.value.contains("Doubles its argument."),
        "cross-file hover should include the doc-comment, got: {}",
        m.value
    );
    assert!(
        m.value.contains("util.leek"),
        "cross-file hover should note the defining file, got: {}",
        m.value
    );
}

#[test]
fn cross_file_hover_does_not_leak_across_independent_ais() {
    // ai2 defines `twice` but ai1 does not include it; hovering an
    // undefined `twice` in ai1 must NOT borrow ai2's signature.
    let ws = open_files(&[
        ("ai1.leek", "var n = twice(3)\n"),
        (
            "ai2.leek",
            "function twice(integer x) -> integer { return x * 2 }\n",
        ),
    ]);
    let hover = hover::handle(
        &ws,
        &proj("ai1.leek"),
        lsp::Position {
            line: 0,
            character: 9,
        },
    );
    // Either no hover, or one that does not contain ai2's signature.
    if let Some(h) = hover {
        let lsp::HoverContents::Markup(m) = h.contents else {
            return;
        };
        assert!(
            !m.value.contains("function twice("),
            "must not borrow an unrelated AI's signature, got: {}",
            m.value
        );
    }
}

// ─── slice 7: semantic program-scoped rename ───────────────────────

#[test]
fn rename_does_not_cross_into_independent_ai() {
    // Two AIs that never include each other, each with its own
    // top-level `tick`. Renaming one must NOT touch the other — they
    // are distinct symbols in distinct programs.
    let ws = open_files(&[
        ("ai1.leek", "function tick() { return 1 }\nvar a = tick()\n"),
        ("ai2.leek", "function tick() { return 2 }\nvar b = tick()\n"),
    ]);
    let edit = rename::handle(
        &ws,
        &proj("ai1.leek"),
        lsp::Position {
            line: 0,
            character: 9,
        },
        "step",
    )
    .expect("rename");
    let changes = edit.changes.expect("changes");
    assert!(changes.contains_key(&proj("ai1.leek")), "ai1 edited");
    assert!(
        !changes.contains_key(&proj("ai2.leek")),
        "ai2 is an independent program and must be untouched: {changes:?}"
    );
    // ai1's own decl + use are both renamed.
    assert_eq!(changes.get(&proj("ai1.leek")).unwrap().len(), 2);
}

#[test]
fn rename_shared_library_symbol_reaches_all_includers() {
    // util is included by two AIs; renaming a util symbol updates the
    // declaration AND every AI that uses it.
    let ws = open_files(&[
        ("util.leek", "function shared() { return 0 }\n"),
        ("ai1.leek", "include(\"util\")\nvar a = shared()\n"),
        ("ai2.leek", "include(\"util\")\nvar b = shared()\n"),
    ]);
    let edit = rename::handle(
        &ws,
        &proj("util.leek"),
        lsp::Position {
            line: 0,
            character: 9,
        },
        "common",
    )
    .expect("rename");
    let changes = edit.changes.expect("changes");
    assert!(changes.contains_key(&proj("util.leek")), "util decl");
    assert!(changes.contains_key(&proj("ai1.leek")), "ai1 use");
    assert!(changes.contains_key(&proj("ai2.leek")), "ai2 use");
    assert!(
        changes.values().flatten().all(|e| e.new_text == "common"),
        "every edit renames to `common`"
    );
}

#[test]
fn rename_ai_private_symbol_does_not_leak_through_shared_library() {
    // Both AIs include util, so they share a component — but a symbol
    // private to ai1 must still not reach ai2. This is the case a naive
    // connected-component scope would get wrong.
    let ws = open_files(&[
        ("util.leek", "function shared() { return 0 }\n"),
        (
            "ai1.leek",
            "include(\"util\")\nfunction priv() { return shared() }\nvar a = priv()\n",
        ),
        (
            "ai2.leek",
            "include(\"util\")\nfunction priv() { return shared() }\nvar b = priv()\n",
        ),
    ]);
    let edit = rename::handle(
        &ws,
        &proj("ai1.leek"),
        lsp::Position {
            line: 1,
            character: 9,
        },
        "helper1",
    )
    .expect("rename");
    let changes = edit.changes.expect("changes");
    assert!(changes.contains_key(&proj("ai1.leek")), "ai1 edited");
    assert!(
        !changes.contains_key(&proj("ai2.leek")),
        "ai2's same-named-but-distinct `priv` must be untouched: {changes:?}"
    );
    // util has no `priv`, so even though it's in scope it gets no edits.
    assert!(!changes.contains_key(&proj("util.leek")));
}

/// Hover helper: position the cursor at the first byte of the `occ`-th
/// occurrence of `needle` and return the rendered markdown (or a
/// sentinel for no-hover / non-markup).
fn hover_text(src: &str, needle: &str, occ: usize) -> String {
    let mut idx = 0usize;
    let mut start = 0usize;
    for _ in 0..occ {
        let found = src[idx..].find(needle).map(|p| idx + p).expect("needle");
        start = found;
        idx = found + needle.len();
    }
    let prefix = &src[..=start];
    let line = u32::try_from(prefix.matches('\n').count()).unwrap();
    let line_start = prefix.rfind('\n').map_or(0, |p| p + 1);
    let character = u32::try_from(start - line_start).unwrap();
    match hover::handle(&open(src), &url(), lsp::Position { line, character }) {
        Some(h) => match h.contents {
            lsp::HoverContents::Markup(m) => m.value,
            _ => "<non-markup>".into(),
        },
        None => "<NONE>".into(),
    }
}

/// A class hierarchy with methods, fields, statics, functions, and
/// lambdas — the shared fixture for the edge-case hover tests below.
const EDGE_SRC: &str = "\
class Animal {\n\
\u{20}   protected string name\n\
\u{20}   constructor(string n) { this.name = n }\n\
\u{20}   string describe() { return this.name }\n\
\u{20}   static Animal make(string n) { return new Animal(n) }\n\
}\n\
class Cat extends Animal {\n\
\u{20}   integer lives\n\
\u{20}   constructor(string n) { super(n) this.lives = 9 }\n\
\u{20}   string describe() { return super.describe() + \" cat\" }\n\
\u{20}   Cat self() { return this }\n\
}\n\
function add(integer a, integer b) -> integer { return a + b }\n\
var inc = x -> x + 1\n\
var an = new Animal(\"rex\")\n\
var c = new Cat(\"felix\")\n\
c.describe()\n\
Animal.make(\"x\")\n";

#[test]
fn hover_super_is_parent_instance() {
    // `super` (bare and as a call/field receiver) is an *instance* of
    // the parent class — not the parent class declaration.
    for needle in ["super(n)", "super.describe"] {
        let v = hover_text(EDGE_SRC, needle, 1);
        assert!(
            v.contains("Animal") && v.contains("super") && v.contains("instance"),
            "hover on `{needle}` should be an Animal instance, got: {v}"
        );
    }
}

#[test]
fn hover_this_and_super_distinguish_instance_from_class() {
    // `this` renders as the bare class name (`Cat`) with an instance
    // note; a class name renders the `class` decl + a `Class<…>` type,
    // never as an instance.
    let this_hover = hover_text(EDGE_SRC, "this }", 1);
    assert!(
        this_hover.contains("Cat")
            && this_hover.contains("instance")
            && !this_hover.contains("class Cat"),
        "this should be an instance of Cat: {this_hover}"
    );
    let class_hover = hover_text(EDGE_SRC, "Animal {", 1);
    assert!(
        class_hover.contains("class Animal")
            && class_hover.contains("Class<Animal>")
            && !class_hover.contains("instance"),
        "a class name should render the class decl + Class<> type: {class_hover}"
    );
}

#[test]
fn hover_method_decl_shows_prefix_return_type() {
    // Methods use Leekscript's prefix-type syntax: `string describe()`.
    let v = hover_text(EDGE_SRC, "describe()", 1);
    assert!(
        v.contains("string describe()"),
        "method decl should show its return type prefix, got: {v}"
    );
}

#[test]
fn hover_instance_method_call_resolves_signature() {
    // `c.describe()` — receiver typed via its `new Cat()` initializer.
    let v = hover_text(EDGE_SRC, "describe()", 4);
    assert!(
        v.contains("string describe()"),
        "instance method call should resolve to the method sig, got: {v}"
    );
}

#[test]
fn hover_static_method_call_resolves_signature() {
    let v = hover_text(EDGE_SRC, "make(\"x\")", 1);
    assert!(
        v.contains("static Animal make(string n)"),
        "static method call should resolve to the method sig, got: {v}"
    );
}

#[test]
fn hover_extends_parent_resolves_class() {
    // Hovering `Animal` in `class Cat extends Animal`.
    let v = hover_text(EDGE_SRC, "Animal {", 2);
    assert!(
        v.contains("class Animal"),
        "extends parent should resolve to the class decl, got: {v}"
    );
}

#[test]
fn hover_instance_var_shows_instance_type() {
    // `var c = new Cat()` — the variable is an instance, shown via its
    // type (`Cat`), distinct from the `class Cat` form for class refs.
    let v = hover_text(EDGE_SRC, "c = new Cat", 1);
    assert!(
        v.contains("var c") && v.contains("Cat") && !v.contains("class Cat"),
        "instance var should show its instance type, got: {v}"
    );
}

#[test]
fn hover_function_shows_function_value_type() {
    // A function's value type renders as `Function<A, B => C>`, the same
    // vocabulary as every other type.
    let v = hover_text(EDGE_SRC, "add(integer", 1);
    assert!(
        v.contains("Function<integer, integer => integer>"),
        "function should show its value type, got: {v}"
    );
}

#[test]
fn hover_class_reference_shows_class_type() {
    // A class (not an instance) renders its type as `Class<Name>`.
    let v = hover_text(EDGE_SRC, "Animal {", 1);
    assert!(
        v.contains("Class<Animal>"),
        "a class should show its `Class<Name>` type, got: {v}"
    );
}

#[test]
fn hover_function_and_lambda_signatures() {
    let func = hover_text(EDGE_SRC, "add(integer", 1);
    assert!(
        func.contains("function add(integer a, integer b) -> integer"),
        "function decl signature, got: {func}"
    );
    let lambda = hover_text(EDGE_SRC, "inc =", 1);
    assert!(
        lambda.contains("var inc") && lambda.contains("Function<"),
        "lambda-bound var should show a function type, got: {lambda}"
    );
}

#[test]
fn hover_shows_backend_directives_in_signature_mode() {
    // In a signature file (`@experimental: function_signatures`), a
    // `@java-backend:` directive is pulled out of the prose and shown
    // under its own heading.
    let src = "// @experimental: function_signatures\n/**\n * Adds two integers.\n * @java-backend: Math.addExact(%0, %1)\n */\nfunction add(integer a, integer b) -> integer { return a + b }\n";
    let v = hover_text(src, "add(integer", 1);
    assert!(
        v.contains("Adds two integers."),
        "visible doc should remain, got: {v}"
    );
    assert!(
        !v.contains("@java-backend"),
        "raw directive line should not appear in prose, got: {v}"
    );
    assert!(
        v.contains("Backend implementations") && v.contains("Math.addExact(%0, %1)"),
        "java directive should be shown in its own section, got: {v}"
    );
}

#[test]
fn hover_directives_inert_in_normal_code() {
    // Without signature mode, a `@java-backend:` line is NOT recognized
    // as a directive — it stays as ordinary doc prose, no backend
    // section.
    let src = "/**\n * Adds two integers.\n * @java-backend: Math.addExact(%0, %1)\n */\nfunction add(integer a, integer b) -> integer { return a + b }\n";
    let v = hover_text(src, "add(integer", 1);
    assert!(
        !v.contains("Backend implementations"),
        "directives must be inert in normal code, got: {v}"
    );
}

#[test]
fn hover_narrowing_enables_member_resolution() {
    // After `a instanceof Cat`, the receiver narrows to a Cat instance,
    // so `a.purr()` resolves to Cat's method even though `a` was untyped.
    let src = "class Cat {\n  purr() { return 1 }\n}\nfunction f(a) {\n  if (a instanceof Cat) {\n    a.purr()\n  }\n}\n";
    let v = hover_text(src, "purr()", 2);
    assert!(
        v.contains("purr()"),
        "narrowed receiver should resolve its method, got: {v}"
    );
}

#[test]
fn hover_field_access_shows_field_type() {
    // `this.age` member access resolves to the declared field. Hover on
    // the `age` part (the 2nd `age` — the 1st is the field declaration).
    let src = "class Cat {\n  integer age\n  bday() { return this.age }\n}\n";
    let v = hover_text(src, "age", 2);
    assert!(
        v.contains("integer age"),
        "field access should resolve the field decl, got: {v}"
    );
}

#[test]
fn hover_untyped_field_resolves_type_from_constructor() {
    // `name` has no type annotation, but the constructor assigns it from
    // a `string` parameter — hover should report `string name` rather
    // than `any name`.
    let src = "class Cat {\n  name\n  constructor(string n) { this.name = n }\n  describe() { return this.name }\n}\n";
    let v = hover_text(src, "name }", 1);
    assert!(
        v.contains("string name"),
        "untyped field should infer its type from the constructor, got: {v}"
    );
}

#[test]
fn hover_unannotated_method_decl_shows_return_type() {
    // An unannotated method should still render a return type (`any`),
    // matching the `function … -> any` form rather than dropping it.
    let src = "class Cat {\n  meow() { return 1 }\n}\n";
    let v = hover_text(src, "meow()", 1);
    assert!(
        v.contains("any meow()"),
        "unannotated method should show a return type, got: {v}"
    );
}

#[test]
fn hover_on_this_shows_enclosing_class() {
    // `this` inside a method should resolve to the enclosing class,
    // not fall back to `any`.
    let src = "class Logger {\n  string prefix\n  Logger log(any msg) {\n    return this\n  }\n}\n";
    let value = hover_text(src, "this", 1);
    assert!(
        value.contains("Logger"),
        "hover on `this` should mention the class, got: {value}"
    );
    assert!(
        !value.contains("any"),
        "hover on `this` should not be `any`, got: {value}"
    );
}

#[test]
fn hover_on_class_name_in_type_position_resolves_class() {
    // A class name used as a return type (a `TypeRef`, not an
    // expression) should still resolve to the class declaration.
    let src = "class Logger {\n  Logger log(any msg) { return this }\n}\n";
    let value = hover_text(src, "Logger log", 1);
    assert!(
        value.contains("class Logger"),
        "hover on a class name in a type position should render its \
         declaration, got: {value}"
    );
}

#[test]
fn hover_on_class_name_in_new_resolves_class() {
    let src = "class Cat {}\nvar c = new Cat()\n";
    let value = hover_text(src, "Cat()", 1);
    assert!(
        value.contains("class Cat"),
        "hover on `Cat` in `new Cat()` should render the class decl, \
         got: {value}"
    );
}

// ===================================================================
// Extended hover coverage — type-display vocabulary, format_type
// composites, member resolution, and edge cases. One assertion focus
// per test so a regression points at the exact missing case.
// ===================================================================

// ---- Function value type: Function<P0, … => R> shapes ----

#[test]
fn hover_function_value_type_no_params() {
    let src = "function ping() -> boolean { return true }\n";
    let v = hover_text(src, "ping()", 1);
    assert!(
        v.contains("Function< => boolean>"),
        "zero-param function value type, got: {v}"
    );
}

#[test]
fn hover_function_value_type_single_param() {
    let src = "function neg(integer x) -> integer { return 0 - x }\n";
    let v = hover_text(src, "neg(integer", 1);
    assert!(
        v.contains("Function<integer => integer>"),
        "single-param function value type, got: {v}"
    );
}

#[test]
fn hover_function_value_type_untyped_params() {
    let src = "function combine(a, b) { return a }\n";
    let v = hover_text(src, "combine(a", 1);
    assert!(
        v.contains("Function<any, any => any>"),
        "untyped params should widen to any, got: {v}"
    );
}

#[test]
fn hover_function_used_as_value_shows_signature_and_type() {
    let src = "function add(integer a, integer b) -> integer { return a + b }\nvar f = add\n";
    // The `add` reference on line 2 (`= add`), not the declaration.
    let v = hover_text(src, "add\n", 1);
    assert!(
        v.contains("function add(") && v.contains("Function<integer, integer => integer>"),
        "function used as a value shows signature + value type, got: {v}"
    );
}

// ---- Class<Name> across positions ----

#[test]
fn hover_class_decl_shows_class_type() {
    let src = "class Cat {}\n";
    let v = hover_text(src, "Cat {", 1);
    assert!(
        v.contains("class Cat") && v.contains("Class<Cat>"),
        "class decl shows decl + Class<> type, got: {v}"
    );
}

#[test]
fn hover_class_in_new_shows_class_type() {
    let src = "class Cat {}\nvar c = new Cat()\n";
    let v = hover_text(src, "Cat()", 1);
    assert!(
        v.contains("Class<Cat>"),
        "class in `new Cat()` shows Class<> type, got: {v}"
    );
}

// ---- this / super render as the bare class name ----

#[test]
fn hover_this_in_method_is_bare_class_name() {
    let src = "class Dog {\n  bark() { return this }\n}\n";
    let v = hover_text(src, "this }", 1);
    assert!(
        v.contains("Dog") && v.contains("instance") && !v.contains("class Dog"),
        "this should render as the class name, got: {v}"
    );
}

// ---- format_type composites via inference ----

#[test]
fn hover_array_literal_infers_element_type() {
    let v = hover_text("var nums = [1, 2, 3]\n", "[1, 2, 3]", 1);
    assert!(v.contains("Array<integer>"), "array element type, got: {v}");
}

#[test]
fn hover_nested_array_literal() {
    let v = hover_text("var n = [[1, 2], [3, 4]]\n", "[[1", 1);
    assert!(
        v.contains("Array<Array<integer>>"),
        "nested array type, got: {v}"
    );
}

#[test]
fn hover_map_literal_infers_kv_types() {
    let v = hover_text("var m = [\"a\": 1, \"b\": 2]\n", "[\"a\"", 1);
    assert!(
        v.contains("Map<string, integer>"),
        "map key/value types, got: {v}"
    );
}

// ---- literal types ----

#[test]
fn hover_real_literal_is_real() {
    let v = hover_text("var x = 3.14\n", "3.14", 1);
    assert!(v.contains("real"), "real literal, got: {v}");
}

#[test]
fn hover_string_literal_is_string() {
    let v = hover_text("var x = \"hi\"\n", "\"hi\"", 1);
    assert!(v.contains("string"), "string literal, got: {v}");
}

#[test]
fn hover_boolean_literal_is_boolean() {
    let v = hover_text("var x = true\n", "true", 1);
    assert!(v.contains("boolean"), "boolean literal, got: {v}");
}

// ---- builtin used as a value (not called) ----

#[test]
fn hover_builtin_used_as_value_shows_signature() {
    let v = hover_text("var f = count\n", "count\n", 1);
    assert!(
        v.contains("function count("),
        "builtin name as a value resolves its signature, got: {v}"
    );
}

// ---- field-type inference edge cases ----

#[test]
fn hover_untyped_field_from_literal_assignment() {
    let src = "class Counter {\n  total\n  constructor() { this.total = 0 }\n  get() { return this.total }\n}\n";
    let v = hover_text(src, "total }", 1);
    assert!(
        v.contains("integer total"),
        "field assigned an int literal infers integer, got: {v}"
    );
}

#[test]
fn hover_unassigned_untyped_field_stays_any() {
    let src = "class Box {\n  contents\n  peek() { return this.contents }\n}\n";
    let v = hover_text(src, "contents }", 1);
    assert!(
        v.contains("any contents"),
        "a field with no annotation or assignment stays any, got: {v}"
    );
}

#[test]
fn hover_nullable_field_type_preserved() {
    let src = "class Node {\n  integer? value\n  get() { return this.value }\n}\n";
    let v = hover_text(src, "value }", 1);
    assert!(
        v.contains("integer?"),
        "nullable field type preserved, got: {v}"
    );
}

// ---- inherited / super member resolution ----

#[test]
fn hover_super_method_resolves_parent_method() {
    // The `super.describe()` call inside Cat resolves to Animal::describe.
    let v = hover_text(EDGE_SRC, "describe()", 3);
    assert!(
        v.contains("string describe()"),
        "super.describe() resolves the parent method, got: {v}"
    );
}

// ---- parameters ----

#[test]
fn hover_on_parameter_decl_shows_typed_param() {
    let src = "function f(integer count) { return count }\n";
    let v = hover_text(src, "count)", 1);
    assert!(
        v.contains("integer count"),
        "parameter decl shows its typed signature, got: {v}"
    );
}

#[test]
fn hover_on_parameter_use_shows_type() {
    let src = "function f(integer count) { return count }\n";
    let v = hover_text(src, "count }", 1);
    assert!(
        v.contains("count") && v.contains("integer"),
        "parameter use resolves to its declared type, got: {v}"
    );
}

// ---- no-hover position ----

#[test]
fn hover_on_keyword_returns_none() {
    // The `var` keyword carries neither a symbol nor a type.
    let v = hover_text("var x = 5\n", "var", 1);
    assert_eq!(v, "<NONE>", "unhandled position should produce no hover");
}

#[test]
fn hover_method_not_confused_with_same_named_function() {
    // A top-level `describe` and a method `describe` share a name; the
    // method hover must show the method signature, not the top-level
    // function's value type.
    let src = "function describe(integer n) -> integer { return n }\nclass Cat {\n  string describe() { return \"x\" }\n}\n";
    let v = hover_text(src, "describe()", 1);
    assert!(
        v.contains("string describe()") && !v.contains("Function<integer"),
        "method hover must not borrow the function's value type, got: {v}"
    );
}

#[test]
fn hover_function_side_of_name_collision_keeps_its_type() {
    // Mirror of the method-collision test: the top-level `describe`
    // function must still show its own value type + complexity.
    let src = "function describe(integer n) -> integer { return n }\nclass Cat {\n  string describe() { return \"x\" }\n}\n";
    let v = hover_text(src, "describe(integer", 1);
    assert!(
        v.contains("function describe(integer n) -> integer")
            && v.contains("Function<integer => integer>"),
        "function hover keeps its own signature + value type, got: {v}"
    );
}

#[test]
fn hover_user_var_shadows_builtin_name() {
    // A local named like a builtin must hover as the local, not the
    // builtin signature.
    let src = "var count = 5\nvar n = count\n";
    let v = hover_text(src, "count\n", 1);
    assert!(
        v.contains("var count") && !v.contains("function count("),
        "user var should shadow the builtin, got: {v}"
    );
}

#[test]
fn hover_user_function_shadows_builtin_name() {
    let src = "function count(x) { return x }\nvar n = count(3)\n";
    let v = hover_text(src, "count(3)", 1);
    assert!(
        v.contains("function count(x)") && !v.contains("function count(any value)"),
        "user function should shadow the builtin, got: {v}"
    );
}

#[test]
fn hover_member_on_function_return_receiver() {
    // `make()` returns a Cat, so `make().purr()` should resolve purr.
    let src = "class Cat {\n  purr() { return 1 }\n}\nfunction make() -> Cat { return new Cat() }\nfunction f() {\n  make().purr()\n}\n";
    let v = hover_text(src, "purr()", 2);
    assert!(
        v.contains("purr()"),
        "member on a function-return receiver should resolve, got: {v}"
    );
}

#[test]
fn hover_member_on_typed_param_receiver() {
    let src = "class Cat {\n  purr() { return 1 }\n}\nfunction f(Cat c) {\n  c.purr()\n}\n";
    let v = hover_text(src, "purr()", 2);
    assert!(
        v.contains("purr()"),
        "member on a typed-param receiver should resolve, got: {v}"
    );
}

#[test]
fn hover_member_on_nullable_param_receiver() {
    let src = "class Cat {\n  purr() { return 1 }\n}\nfunction f(Cat? c) {\n  c.purr()\n}\n";
    let v = hover_text(src, "purr()", 2);
    assert!(
        v.contains("purr()"),
        "member on a nullable class receiver should resolve, got: {v}"
    );
}

#[test]
fn hover_inherited_field_resolves_via_parent_chain() {
    let src = "class Animal {\n  string name\n}\nclass Cat extends Animal {}\nvar c = new Cat()\nc.name\n";
    let v = hover_text(src, "name", 2);
    assert!(
        v.contains("string name"),
        "inherited field should resolve via the parent chain, got: {v}"
    );
}

#[test]
fn hover_overloaded_builtin_as_value_shows_overloads() {
    let v = hover_text("var f = abs\n", "abs\n", 1);
    assert!(
        v.matches("function abs(").count() >= 2,
        "builtin-as-value should show all overloads, got: {v}"
    );
}

#[test]
fn hover_complexity_substitutes_called_function() {
    // `caller` calls `inner` (linear in its arg). File-level analysis
    // substitutes the callee's formula, so hover shows a real class —
    // the old standalone path collapsed any user call to `O(?)`.
    let src = "function inner(arr) {\n  var t = 0\n  for (var x in arr) { t = t + x }\n  return t\n}\nfunction caller(arr) {\n  return inner(arr)\n}\n";
    let v = hover_text(src, "caller(arr", 1);
    assert!(
        v.contains("Complexity:") && !v.contains("O(?)"),
        "caller should get a substituted complexity, not O(?), got: {v}"
    );
}

#[test]
fn hover_constant_function_shows_operation_cost_not_o1() {
    // A constant-cost function shows its operation count, not `O(1)`.
    let src = "function add(integer a, integer b) -> integer { return a + b }\n";
    let v = hover_text(src, "add(integer", 1);
    assert!(
        !v.contains("O(1)"),
        "constant fn should not display O(1), got: {v}"
    );
    assert!(
        v.contains("Cost:") && v.contains("operations"),
        "constant fn should show its operation cost, got: {v}"
    );
}

#[test]
fn hover_var_inference_flows_into_return_expression() {
    // The user's `smoothstep` case: `var u = real / real` resolves `u`
    // to real, so the return expression `u * u * …` infers real instead
    // of `any`. (LSP-only; the corpus path keeps `var` dynamic.)
    let src = "function f(real a, real b) -> real {\n  var u = (a - b) / (a + b)\n  return u * u * 2\n}\n";
    let v = hover_text(src, "* 2", 1);
    assert!(
        v.contains("real") && !v.contains("any"),
        "return expression over a real `var` should infer real, got: {v}"
    );
}

/// Chained member access: `fm.leek.<member>` — the receiver of the
/// final access is the *intermediate* field's class (Entity), not the
/// class of the chain's first link (FM).
const CHAIN_SRC: &str = "\
class Entity {\n\
\u{20}   integer cell = 1\n\
\u{20}   integer getCell() { return this.cell }\n\
}\n\
class FM {\n\
\u{20}   Entity leek = new Entity()\n\
}\n\
var fm = new FM()\n\
var x = fm.leek.cell\n\
fm.leek.getCell()\n";

#[test]
fn hover_chained_field_access_resolves_declaration() {
    // Regression: `base_class_name` used a point query at the base's
    // *start*, landing on `fm` (class FM) instead of `fm.leek`
    // (Entity) — the member branch failed and hover degraded to the
    // bare inferred type with no field signature.
    // 1st `cell` = field decl, 2nd = `this.cell`, 3rd = `fm.leek.cell`.
    let v = hover_text(CHAIN_SRC, "cell", 3);
    assert!(
        v.contains("integer cell"),
        "chained field access should resolve the field decl, got: {v}"
    );
}

#[test]
fn hover_chained_method_call_resolves_signature() {
    // 2nd `getCell` is the chained call site `fm.leek.getCell()`.
    let v = hover_text(CHAIN_SRC, "getCell", 2);
    assert!(
        v.contains("integer getCell()"),
        "chained method call should resolve the method decl, got: {v}"
    );
}

#[test]
fn hover_chained_field_access_via_this() {
    let src = "class Entity {\n  integer cell = 1\n}\nclass FM {\n  Entity leek = new Entity()\n  m() { return this.leek.cell }\n}\n";
    let v = hover_text(src, "cell }", 1);
    assert!(
        v.contains("integer cell"),
        "`this.leek.cell` should resolve through the chain, got: {v}"
    );
}

#[test]
fn hover_annotated_decl_name_shows_declared_type() {
    // Hover on `myLeek` in `Entity myLeek = fm.leek` renders the
    // annotated declaration, never a bare `any`.
    let src = "class Entity {\n  integer cell = 1\n}\nclass FM {\n  Entity leek = new Entity()\n}\nvar fm = new FM()\nEntity myLeek = fm.leek\n";
    let v = hover_text(src, "myLeek", 1);
    assert!(
        v.contains("Entity myLeek"),
        "annotated decl hover should show the declared type, got: {v}"
    );
}

#[test]
fn hover_chained_method_return_receiver() {
    // The chain link can be a method call: `fm.getLeek().cell` resolves
    // the final field through the method\'s declared return type.
    let src = "class Entity {\n  integer cell = 1\n}\nclass FM {\n  Entity leek = new Entity()\n  Entity getLeek() { return this.leek }\n}\nvar fm = new FM()\nvar x = fm.getLeek().cell\n";
    let v = hover_text(src, "cell\n", 1);
    assert!(
        v.contains("integer cell"),
        "method-return chain should resolve the field decl, got: {v}"
    );
}
