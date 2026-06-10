//! Parser tests. Two layers:
//!
//! 1. **Round-trip** — the text reconstructed from the green tree equals
//!    the input source byte-for-byte. This is the lossless invariant.
//!
//! 2. **Shape** — for a handful of inputs, print the tree to an
//!    inline-asserted format. As coverage grows this should move to
//!    `insta` snapshots; for now plain `assert_eq` keeps the slice
//!    dependency-light.

use std::fmt::Write as _;

use leek_parser::ast::{AstNode, BinaryExpr, Expr, SourceFile, Stmt};
use leek_parser::parse;
use leek_span::SourceId;
use leek_syntax::{SyntaxElement, SyntaxNode, Version};

fn src() -> SourceId {
    SourceId::new(1).unwrap()
}

fn parse_str(text: &str) -> (SyntaxNode, Vec<leek_diagnostics::Diagnostic>) {
    let result = parse(text, src(), Version::LATEST);
    let node = SyntaxNode::new_root(result.green);
    (node, result.diagnostics)
}

/// Pretty-print a tree with one node/token per line for shape assertions.
fn dump(node: &SyntaxNode) -> String {
    let mut out = String::new();
    fn walk(node: &SyntaxNode, depth: usize, out: &mut String) {
        let _ = writeln!(out, "{}{:?}", "  ".repeat(depth), node.kind());
        for child in node.children_with_tokens() {
            match child {
                SyntaxElement::Node(n) => walk(&n, depth + 1, out),
                SyntaxElement::Token(t) => {
                    if t.kind().is_trivia() {
                        continue; // strip trivia for shape tests
                    }
                    let _ = writeln!(
                        out,
                        "{}{:?} {:?}",
                        "  ".repeat(depth + 1),
                        t.kind(),
                        t.text()
                    );
                }
            }
        }
    }
    walk(node, 0, &mut out);
    out
}

fn assert_round_trip(text: &str) {
    let (node, diags) = parse_str(text);
    assert_eq!(node.text().to_string(), text, "round-trip mismatch");
    assert!(
        diags.is_empty(),
        "unexpected diagnostics for {text:?}: {diags:?}",
    );
}

#[test]
fn empty_input() {
    let (node, _) = parse_str("");
    assert_eq!(node.kind(), leek_syntax::SyntaxKind::SourceFile);
    assert_eq!(node.children().count(), 0);
}

#[test]
fn bodiless_function_signature_errors_without_flag() {
    // Normal mode still requires a body.
    let (_, diags) = parse_str("function a() -> integer;\n");
    assert!(
        !diags.is_empty(),
        "bodiless function should error without the experimental flag"
    );
}

#[test]
fn bodiless_function_signature_parses_with_flag() {
    use leek_lexer::lex;
    use leek_parser::{ParseFeatures, parse_tokens_with};
    let text = "function a() -> integer;\nfunction b(integer x) -> string;\n";
    let lex_out = lex(text, src(), Version::LATEST);
    let features = ParseFeatures {
        function_signatures: true,
        ..Default::default()
    };
    let result = parse_tokens_with(text, src(), &lex_out.tokens, Version::LATEST, features);
    let node = SyntaxNode::new_root(result.green);
    assert!(
        result.diagnostics.is_empty(),
        "bodiless signatures should parse cleanly with the flag: {:?}",
        result.diagnostics
    );
    // Two FnDecls, neither with a body block.
    let fns: Vec<_> = node
        .children()
        .filter(|n| n.kind() == leek_syntax::SyntaxKind::FnDecl)
        .collect();
    assert_eq!(fns.len(), 2);
    for f in &fns {
        assert!(
            !f.children()
                .any(|c| c.kind() == leek_syntax::SyntaxKind::Block),
            "signature should have no body block"
        );
    }
    assert_eq!(node.text().to_string(), text, "round-trip mismatch");
}

fn parse_types_feature(text: &str) -> (SyntaxNode, Vec<leek_diagnostics::Diagnostic>) {
    use leek_lexer::lex;
    use leek_parser::{ParseFeatures, parse_tokens_with};
    let lex_out = lex(text, src(), Version::LATEST);
    let features = ParseFeatures {
        types: true,
        ..Default::default()
    };
    let r = parse_tokens_with(text, src(), &lex_out.tokens, Version::LATEST, features);
    (SyntaxNode::new_root(r.green), r.diagnostics)
}

#[test]
fn type_alias_parses_with_flag() {
    let text = "type Id = integer | string\nId x = 1\n";
    let (node, diags) = parse_types_feature(text);
    assert!(diags.is_empty(), "alias should parse cleanly: {diags:?}");
    assert_eq!(node.text().to_string(), text, "round-trip");
    let alias = node
        .children()
        .find(|n| n.kind() == leek_syntax::SyntaxKind::TypeAliasDecl)
        .expect("TypeAliasDecl node");
    assert!(
        alias
            .children()
            .any(|n| n.kind() == leek_syntax::SyntaxKind::TypeRef),
        "alias body should be a TypeRef"
    );
}

#[test]
fn type_alias_is_not_a_decl_without_flag() {
    // Without the flag `type` is an ordinary identifier; no
    // TypeAliasDecl node may appear.
    let (node, _) = parse_str("type Id = integer\n");
    assert!(
        !node
            .descendants()
            .any(|n| n.kind() == leek_syntax::SyntaxKind::TypeAliasDecl),
        "TypeAliasDecl must be feature-gated"
    );
}

#[test]
fn tuple_type_parses_with_flag() {
    let text = "Array[integer, boolean] t = [1, true]\n";
    let (node, diags) = parse_types_feature(text);
    assert!(diags.is_empty(), "tuple type should parse: {diags:?}");
    assert_eq!(node.text().to_string(), text, "round-trip");
    // The annotation is one TypeRef keeping its square brackets as
    // direct tokens (that is what distinguishes it from `<…>` args).
    let type_ref = node
        .descendants()
        .find(|n| n.kind() == leek_syntax::SyntaxKind::TypeRef)
        .expect("TypeRef");
    assert!(
        type_ref
            .children_with_tokens()
            .filter_map(leek_syntax::language::NodeOrToken::into_token)
            .any(|t| t.kind() == leek_syntax::SyntaxKind::LBracket),
        "tuple TypeRef should keep its LBracket"
    );
}

#[test]
fn tuple_type_union_member_parses_with_flag() {
    let text = "Array[integer, boolean] | string x = [1, true]\n";
    let (node, diags) = parse_types_feature(text);
    assert!(diags.is_empty(), "tuple-in-union should parse: {diags:?}");
    assert_eq!(node.text().to_string(), text, "round-trip");
}

fn parse_interfaces(text: &str) -> (SyntaxNode, Vec<leek_diagnostics::Diagnostic>) {
    use leek_lexer::lex;
    use leek_parser::{ParseFeatures, parse_tokens_with};
    let lex_out = lex(text, src(), Version::LATEST);
    let features = ParseFeatures {
        interfaces: true,
        ..Default::default()
    };
    let r = parse_tokens_with(text, src(), &lex_out.tokens, Version::LATEST, features);
    (SyntaxNode::new_root(r.green), r.diagnostics)
}

#[test]
fn interface_decl_parses_with_flag() {
    let text = "interface Named {\n  string name;\n  string describe();\n}\n";
    let (node, diags) = parse_interfaces(text);
    assert!(
        diags.is_empty(),
        "interface should parse cleanly: {diags:?}"
    );
    assert_eq!(node.text().to_string(), text, "round-trip");
    let iface = node
        .children()
        .find(|n| n.kind() == leek_syntax::SyntaxKind::InterfaceDecl)
        .expect("InterfaceDecl node");
    let members: Vec<_> = iface
        .children()
        .filter(|n| n.kind() == leek_syntax::SyntaxKind::InterfaceMember)
        .collect();
    assert_eq!(members.len(), 2, "field + method");
    // The field has no ParamList; the method has one.
    assert!(
        !members[0]
            .children()
            .any(|n| n.kind() == leek_syntax::SyntaxKind::ParamList),
        "field member has no param list"
    );
    assert!(
        members[1]
            .children()
            .any(|n| n.kind() == leek_syntax::SyntaxKind::ParamList),
        "method member has a param list"
    );
}

#[test]
fn implements_clause_parses_with_flag() {
    let text = "class Dog extends Animal implements Named, Walker {\n}\n";
    let (node, diags) = parse_interfaces(text);
    assert!(
        diags.is_empty(),
        "implements should parse cleanly: {diags:?}"
    );
    assert_eq!(node.text().to_string(), text, "round-trip");
    let class = node
        .children()
        .find(|n| n.kind() == leek_syntax::SyntaxKind::ClassDecl)
        .expect("ClassDecl node");
    let clause = class
        .children()
        .find(|n| n.kind() == leek_syntax::SyntaxKind::ImplementsClause)
        .expect("ImplementsClause node");
    let names: Vec<String> = clause
        .children_with_tokens()
        .filter_map(leek_syntax::language::NodeOrToken::into_token)
        .filter(|t| t.kind() == leek_syntax::SyntaxKind::Ident)
        .map(|t| t.text().to_string())
        .collect();
    assert_eq!(names, ["Named", "Walker"]);
}

#[test]
fn interface_decl_is_not_a_decl_without_flag() {
    // Without the flag the reserved `interface` keyword stays an
    // error (mirroring upstream's reservation); no InterfaceDecl node
    // may appear.
    let (node, _) = parse_str("interface Named {\n  string name;\n}\n");
    assert!(
        !node
            .descendants()
            .any(|n| n.kind() == leek_syntax::SyntaxKind::InterfaceDecl),
        "InterfaceDecl must be feature-gated"
    );
}

fn parse_enums(text: &str) -> (SyntaxNode, Vec<leek_diagnostics::Diagnostic>) {
    use leek_lexer::lex;
    use leek_parser::{ParseFeatures, parse_tokens_with};
    let lex_out = lex(text, src(), Version::LATEST);
    let features = ParseFeatures {
        enums: true,
        ..Default::default()
    };
    let r = parse_tokens_with(text, src(), &lex_out.tokens, Version::LATEST, features);
    (SyntaxNode::new_root(r.green), r.diagnostics)
}

#[test]
fn enum_decl_parses_with_flag() {
    let text = "enum Color {\n  RED,\n  GREEN,\n  BLUE = 10,\n}\n";
    let (node, diags) = parse_enums(text);
    assert!(diags.is_empty(), "enum should parse cleanly: {diags:?}");
    assert_eq!(node.text().to_string(), text, "round-trip");
    let decl = node
        .children()
        .find(|n| n.kind() == leek_syntax::SyntaxKind::EnumDecl)
        .expect("EnumDecl node");
    let members: Vec<_> = decl
        .children()
        .filter(|n| n.kind() == leek_syntax::SyntaxKind::EnumMember)
        .collect();
    assert_eq!(members.len(), 3, "three variants");
    // Only the explicit-value variant carries an IntLiteral token.
    let has_int = |m: &SyntaxNode| {
        m.children_with_tokens()
            .filter_map(leek_syntax::language::NodeOrToken::into_token)
            .any(|t| t.kind() == leek_syntax::SyntaxKind::IntLiteral)
    };
    assert!(!has_int(&members[0]), "RED has no explicit value");
    assert!(!has_int(&members[1]), "GREEN has no explicit value");
    assert!(has_int(&members[2]), "BLUE = 10 keeps its IntLiteral");
}

#[test]
fn enum_negative_value_parses_with_flag() {
    let text = "enum Temp { FREEZING = -10, ZERO = 0 }\n";
    let (node, diags) = parse_enums(text);
    assert!(
        diags.is_empty(),
        "negative variant value should parse: {diags:?}"
    );
    assert_eq!(node.text().to_string(), text, "round-trip");
}

#[test]
fn enum_non_integer_value_is_an_error() {
    let text = "enum Bad { A = \"x\" }\n";
    let (_, diags) = parse_enums(text);
    assert!(
        !diags.is_empty(),
        "non-integer variant value must be rejected"
    );
}

#[test]
fn enum_decl_is_not_a_decl_without_flag() {
    // Without the flag the reserved `enum` keyword stays an error
    // (mirroring upstream's reservation); no EnumDecl node may appear.
    let (node, _) = parse_str("enum Color { RED }\n");
    assert!(
        !node
            .descendants()
            .any(|n| n.kind() == leek_syntax::SyntaxKind::EnumDecl),
        "EnumDecl must be feature-gated"
    );
}

fn parse_generics(text: &str) -> (SyntaxNode, Vec<leek_diagnostics::Diagnostic>) {
    use leek_lexer::lex;
    use leek_parser::{ParseFeatures, parse_tokens_with};
    let lex_out = lex(text, src(), Version::LATEST);
    let features = ParseFeatures {
        generics: true,
        ..Default::default()
    };
    let r = parse_tokens_with(text, src(), &lex_out.tokens, Version::LATEST, features);
    (SyntaxNode::new_root(r.green), r.diagnostics)
}

#[test]
fn generic_function_parses_with_flag() {
    let text = "function push<T>(Array<T> array, T value) -> T { return value }\n";
    let (node, diags) = parse_generics(text);
    assert!(diags.is_empty(), "generic fn should parse: {diags:?}");
    assert_eq!(node.text().to_string(), text, "round-trip");
    // The `<T>` is a TypeParamList; the function name stays `push`.
    let fnode = node
        .descendants()
        .find(|n| n.kind() == leek_syntax::SyntaxKind::FnDecl)
        .unwrap();
    assert!(
        fnode
            .children()
            .any(|c| c.kind() == leek_syntax::SyntaxKind::TypeParamList),
        "expected a TypeParamList"
    );
}

#[test]
fn generic_class_and_method_parse_with_flag() {
    let text =
        "class Box<T> extends Container<T> {\n  T value\n  T get<U>(U key) { return value }\n}\n";
    let (_node, diags) = parse_generics(text);
    assert!(
        diags.is_empty(),
        "generic class/method should parse: {diags:?}"
    );
}

#[test]
fn generic_function_errors_without_flag() {
    // Without the experimental flag, `<` after a name isn't a type-param list.
    let (_, diags) = parse_str("function push<T>(T value) { return value }\n");
    assert!(
        !diags.is_empty(),
        "generic syntax should error without the flag"
    );
}

#[test]
fn type_reference_forms_parse() {
    // Type *uses* (as opposed to type-parameter *declarations*) are not
    // gated on the experimental flag — both generic and non-generic
    // forms must parse everywhere a type is accepted (here: param types).
    let forms = [
        "Array",
        "Array<integer>",
        "Map<string, integer>",
        "Set<integer>",
        "Function",
        "Function<integer, integer => string>",
        "Function< => string>",
        "Class<Cat>",
        "Class",
        // nested + nullable + union, for good measure
        "Map<string, Array<integer>>",
        "Array<integer>?",
        "integer | string",
    ];
    for form in forms {
        let src = format!("function f({form} a) {{ return a }}\n");
        let (_, diags) = parse_str(&src);
        let errs: Vec<_> = diags
            .iter()
            .filter(|d| d.severity == leek_diagnostics::Severity::Error)
            .collect();
        assert!(errs.is_empty(), "type form {form:?} should parse: {errs:?}");
    }
}

#[test]
fn var_decl_with_int_init() {
    assert_round_trip("var x = 1;");
}

#[test]
fn var_decl_shape() {
    let (node, _) = parse_str("var x = 1;");
    let tree = dump(&node);
    assert_eq!(
        tree,
        "\
SourceFile
  VarDeclStmt
    KwVar \"var\"
    Ident \"x\"
    Eq \"=\"
    LiteralExpr
      IntLiteral \"1\"
    Semicolon \";\"
"
    );
}

#[test]
fn return_with_and_without_value() {
    assert_round_trip("return;");
    assert_round_trip("return 42;");
    assert_round_trip("return foo;");
}

#[test]
fn arithmetic_precedence() {
    let (node, _) = parse_str("var x = 1 + 2 * 3;");
    let tree = dump(&node);
    // `2 * 3` binds tighter; expected: `1 + (2 * 3)`.
    assert_eq!(
        tree,
        "\
SourceFile
  VarDeclStmt
    KwVar \"var\"
    Ident \"x\"
    Eq \"=\"
    BinaryExpr
      LiteralExpr
        IntLiteral \"1\"
      Plus \"+\"
      BinaryExpr
        LiteralExpr
          IntLiteral \"2\"
        Star \"*\"
        LiteralExpr
          IntLiteral \"3\"
    Semicolon \";\"
"
    );
}

#[test]
fn left_associativity_for_additive() {
    let (node, _) = parse_str("var x = 1 - 2 - 3;");
    let tree = dump(&node);
    // Left-assoc: `(1 - 2) - 3`.
    assert_eq!(
        tree,
        "\
SourceFile
  VarDeclStmt
    KwVar \"var\"
    Ident \"x\"
    Eq \"=\"
    BinaryExpr
      BinaryExpr
        LiteralExpr
          IntLiteral \"1\"
        Minus \"-\"
        LiteralExpr
          IntLiteral \"2\"
      Minus \"-\"
      LiteralExpr
        IntLiteral \"3\"
    Semicolon \";\"
"
    );
}

#[test]
fn right_associativity_for_power_and_assign() {
    // Power: 2 ** 3 ** 4 → 2 ** (3 ** 4)
    let (node, _) = parse_str("var x = 2 ** 3 ** 4;");
    let tree = dump(&node);
    assert!(
        tree.contains("BinaryExpr\n      LiteralExpr\n        IntLiteral \"2\"\n      StarStar \"**\"\n      BinaryExpr"),
        "expected right-assoc for **, got:\n{tree}"
    );

    // Assignment: a = b = 1 → a = (b = 1)
    let (node, _) = parse_str("a = b = 1;");
    let tree = dump(&node);
    assert!(
        tree.contains(
            "NameRef\n        Ident \"a\"\n      Eq \"=\"\n      BinaryExpr\n        NameRef"
        ),
        "expected right-assoc for =, got:\n{tree}"
    );
}

#[test]
fn parens_override_precedence() {
    let (node, _) = parse_str("var x = (1 + 2) * 3;");
    let tree = dump(&node);
    // `(1 + 2) * 3` — outer is multiplicative with ParenExpr left.
    assert!(tree.contains("ParenExpr"), "missing ParenExpr in\n{tree}");
    assert!(
        tree.contains("Star \"*\""),
        "expected outermost multiplication, got:\n{tree}"
    );
    assert_round_trip("var x = (1 + 2) * 3;");
}

#[test]
fn unary_prefix() {
    let (node, _) = parse_str("var x = -1;");
    let tree = dump(&node);
    assert!(tree.contains("UnaryExpr"));
    assert_round_trip("var x = -1;");
    assert_round_trip("var x = !true;");
}

#[test]
fn function_call_simple() {
    assert_round_trip("foo();");
    assert_round_trip("foo(1, 2, 3);");
    let (node, _) = parse_str("foo(1, 2);");
    let tree = dump(&node);
    assert!(tree.contains("CallExpr"), "missing CallExpr:\n{tree}");
    assert!(tree.contains("ArgList"), "missing ArgList:\n{tree}");
}

#[test]
fn comparison_and_equality_mix() {
    // `1 < 2 == 3` should parse with comparison tighter than equality:
    // `(1 < 2) == 3`.
    let (node, _) = parse_str("var x = 1 < 2 == 3;");
    let tree = dump(&node);
    assert!(
        tree.contains("BinaryExpr\n      BinaryExpr\n        LiteralExpr\n          IntLiteral \"1\"\n        Lt \"<\""),
        "expected (1<2)==3:\n{tree}"
    );
}

#[test]
fn round_trip_preserves_comments_and_whitespace() {
    let text = "// comment\nvar x = 1;\n";
    let (node, _) = parse_str(text);
    assert_eq!(node.text().to_string(), text);
}

#[test]
fn ast_view_accessors() {
    let (node, _) = parse_str("var damage = 1 + 2;");
    let file = SourceFile::cast(node).unwrap();
    let stmts: Vec<Stmt> = file.stmts().collect();
    assert_eq!(stmts.len(), 1);

    let var = match &stmts[0] {
        Stmt::VarDecl(v) => v,
        other => panic!("expected VarDecl, got {other:?}"),
    };
    assert_eq!(var.name().unwrap().text(), "damage");

    let init = var.init().unwrap();
    let bin = match init {
        Expr::Binary(b) => b,
        other => panic!("expected BinaryExpr, got {other:?}"),
    };
    assert_eq!(BinaryExpr::op(&bin).unwrap().text(), "+");
    assert!(matches!(bin.lhs(), Some(Expr::Literal(_))));
    assert!(matches!(bin.rhs(), Some(Expr::Literal(_))));
}

#[test]
fn semicolons_are_optional() {
    // Real fixtures use both `include("foo");` and `return 'bonjour'`
    // without a trailing semicolon.
    assert_round_trip("var x = 1");
    assert_round_trip("return 42");
}

#[test]
fn empty_array_literal() {
    assert_round_trip("var a = [];");
    let (node, _) = parse_str("var a = [];");
    let tree = dump(&node);
    assert!(tree.contains("ArrayExpr"), "missing ArrayExpr:\n{tree}");
}

#[test]
fn array_literal_with_elements() {
    assert_round_trip("var a = [1, 2, 3];");
    let (node, _) = parse_str("var a = [1, 2, 3];");
    let tree = dump(&node);
    let n = tree.matches("LiteralExpr").count();
    assert_eq!(n, 3, "expected 3 literals in array, tree:\n{tree}");
}

#[test]
fn array_indexing() {
    assert_round_trip("return a[0];");
    let (node, _) = parse_str("return a[0];");
    let tree = dump(&node);
    assert!(tree.contains("IndexExpr"), "missing IndexExpr:\n{tree}");
}

#[test]
fn member_access() {
    assert_round_trip("return obj.field;");
    let (node, _) = parse_str("return obj.field;");
    let tree = dump(&node);
    assert!(tree.contains("FieldExpr"), "missing FieldExpr:\n{tree}");
}

#[test]
fn method_call_chain() {
    // `obj.method(arg)` → FieldExpr → CallExpr.
    assert_round_trip("return obj.method(1);");
    let (node, _) = parse_str("return obj.method(1);");
    let tree = dump(&node);
    assert!(tree.contains("FieldExpr") && tree.contains("CallExpr"));
}

#[test]
fn array_then_indexing() {
    assert_round_trip("return [1, 2, 3][1];");
}

#[test]
fn block_statement() {
    assert_round_trip("{ var x = 1; var y = 2; }");
    let (node, _) = parse_str("{ var x = 1; var y = 2; }");
    let tree = dump(&node);
    assert!(tree.contains("Block"), "missing Block:\n{tree}");
}

#[test]
fn if_with_block() {
    assert_round_trip("if (x) { return 1; }");
    let (node, _) = parse_str("if (x) { return 1; }");
    let tree = dump(&node);
    assert!(tree.contains("IfStmt") && tree.contains("Block"));
}

#[test]
fn if_else_chain() {
    assert_round_trip("if (a) { return 1; } else if (b) { return 2; } else { return 3; }");
    let (node, _) = parse_str("if (a) { return 1; } else if (b) { return 2; }");
    let tree = dump(&node);
    // Two nested IfStmts: the second one is the else-branch of the first.
    assert_eq!(tree.matches("IfStmt").count(), 2);
}

#[test]
fn while_loop() {
    assert_round_trip("while (x > 0) { x = x - 1; }");
    let (node, _) = parse_str("while (x > 0) { x = x - 1; }");
    let tree = dump(&node);
    assert!(tree.contains("WhileStmt"));
}

#[test]
fn break_continue() {
    assert_round_trip("while (true) { if (x) break; continue; }");
    let (node, _) = parse_str("while (true) { if (x) break; continue; }");
    let tree = dump(&node);
    assert!(tree.contains("BreakStmt") && tree.contains("ContinueStmt"));
}

#[test]
fn single_statement_if_body() {
    // The Java reference allows `if (x) doStmt;` without braces.
    assert_round_trip("if (x) return 1;");
}

#[test]
fn nested_array_access() {
    assert_round_trip("return a[0][1];");
}

#[test]
fn pi_and_lemniscate_literals() {
    let src = SourceId::new(1).unwrap();
    let r = leek_parser::parse("var x = π;", src, Version::LATEST);
    assert!(
        r.diagnostics.is_empty(),
        "unexpected diagnostics: {:?}",
        r.diagnostics
    );
    let r = leek_parser::parse("var x = ∞;", src, Version::LATEST);
    assert!(
        r.diagnostics.is_empty(),
        "unexpected diagnostics: {:?}",
        r.diagnostics
    );
}

#[test]
fn function_decl() {
    assert_round_trip("function add(integer a, integer b) -> integer { return a + b; }");
    let (node, _) = parse_str("function add(integer a, integer b) -> integer { return a + b; }");
    let tree = dump(&node);
    assert!(tree.contains("FnDecl"));
    assert!(tree.contains("ParamList"));
    assert!(tree.matches("Param").count() >= 2);
    assert!(tree.contains("TypeRef"));
}

#[test]
fn function_no_types() {
    assert_round_trip("function noop() { return null; }");
}

#[test]
fn lambda_multi_param() {
    assert_round_trip("var f = (a, b) -> a + b;");
    let (node, _) = parse_str("var f = (a, b) -> a + b;");
    let tree = dump(&node);
    assert!(tree.contains("LambdaExpr"));
}

#[test]
fn lambda_single_param() {
    assert_round_trip("var inc = x -> x + 1;");
    let (node, _) = parse_str("var inc = x -> x + 1;");
    assert!(dump(&node).contains("LambdaExpr"));
}

#[test]
fn lambda_block_body() {
    assert_round_trip("var f = (x) -> { return x; };");
}

#[test]
fn paren_vs_lambda_disambig() {
    // Bare `(expr)` must remain a ParenExpr — not a lambda.
    let (node, _) = parse_str("var x = (1 + 2);");
    let tree = dump(&node);
    assert!(tree.contains("ParenExpr"));
    assert!(!tree.contains("LambdaExpr"));
}

#[test]
fn c_style_for() {
    assert_round_trip("for (var i = 0; i < 10; i = i + 1) { x = x + i; }");
    let (node, _) = parse_str("for (var i = 0; i < 10; i = i + 1) {}");
    assert!(dump(&node).contains("ForStmt"));
}

#[test]
fn foreach_single() {
    assert_round_trip("for (var x in arr) { count = count + 1; }");
    let (node, _) = parse_str("for (var x in arr) {}");
    assert!(dump(&node).contains("ForeachStmt"));
}

#[test]
fn foreach_key_value() {
    assert_round_trip("for (var k : var v in m) { sum = sum + v; }");
    let (node, _) = parse_str("for (var k : var v in m) {}");
    assert!(dump(&node).contains("ForeachStmt"));
}

#[test]
fn do_while_loop() {
    assert_round_trip("do { x = x - 1; } while (x > 0);");
    let (node, _) = parse_str("do { x = x - 1; } while (x > 0);");
    assert!(dump(&node).contains("DoWhileStmt"));
}

#[test]
fn switch_basic() {
    assert_round_trip("switch (x) { case 1: return 1; case 2: return 2; default: return 0; }");
    let (node, _) = parse_str("switch (x) { case 1: return 1; default: return 0; }");
    let tree = dump(&node);
    assert!(tree.contains("SwitchStmt"));
    assert!(tree.matches("SwitchCase").count() >= 2);
}

#[test]
fn include_simple() {
    assert_round_trip("include(\"lib.leek\");");
    let (node, _) = parse_str("include(\"lib.leek\");");
    assert!(dump(&node).contains("IncludeStmt"));
}

#[test]
fn import_simple() {
    assert_round_trip("import fight.generator;");
    let (node, _) = parse_str("import fight.generator;");
    assert!(dump(&node).contains("ImportStmt"));
}

#[test]
fn import_string_and_paren_forms() {
    assert_round_trip("import \"fight.generator\";");
    assert_round_trip("import(\"fight.generator\");");
}

#[test]
fn new_expr_basic() {
    assert_round_trip("var c = new Cat();");
    let (node, _) = parse_str("var c = new Cat(\"Whiskers\", 3);");
    let tree = dump(&node);
    assert!(tree.contains("NewExpr"));
    assert!(tree.contains("ArgList"));
}

#[test]
fn class_simple() {
    assert_round_trip("class Cat { public integer age = 0; public meow() { return \"meow\"; } }");
    let (node, _) = parse_str(
        "class Cat extends Animal { public integer age = 0; constructor(integer a) { this.age = a; } public meow() { return \"meow\"; } }",
    );
    let tree = dump(&node);
    assert!(tree.contains("ClassDecl"));
    assert!(tree.contains("ClassField"));
    assert!(tree.contains("ClassMethod"));
    assert!(tree.contains("ClassConstructor"));
}

#[test]
fn typed_var_decl() {
    assert_round_trip("integer x = 5;");
    let (node, _) = parse_str("integer x = 5;");
    let tree = dump(&node);
    assert!(tree.contains("VarDeclStmt"));
    assert!(tree.contains("TypeRef"));
}

#[test]
fn typed_var_decl_generic() {
    assert_round_trip("Array<string> names = [\"a\", \"b\"];");
}

#[test]
fn typed_var_decl_union() {
    assert_round_trip("Array | string mixed = [1, 2];");
}

#[test]
fn typed_var_decl_nullable() {
    assert_round_trip("integer? maybe = null;");
}

#[test]
fn map_literal_in_brackets() {
    assert_round_trip("var m = [1: \"a\", 2: \"b\"];");
    let (node, _) = parse_str("var m = [1: \"a\", 2: \"b\"];");
    assert!(dump(&node).contains("MapExpr"));
}

#[test]
fn brace_set_literal() {
    assert_round_trip("var s = {1, 2, 3};");
    let (node, _) = parse_str("var s = {1, 2, 3};");
    assert!(dump(&node).contains("SetExpr"));
}

#[test]
fn brace_object_literal() {
    // `{f: v}` is an object literal — for maps use `[k: v]`.
    assert_round_trip("var o = {name: \"a\", age: 30};");
    let (node, _) = parse_str("var o = {name: \"a\"};");
    assert!(dump(&node).contains("ObjectExpr"));
}

#[test]
fn empty_array_vs_empty_brace_object() {
    let (a, _) = parse_str("var x = [];");
    assert!(dump(&a).contains("ArrayExpr"));
    let (b, _) = parse_str("var x = {};");
    assert!(dump(&b).contains("ObjectExpr"));
}

#[test]
fn bracket_map_is_canonical() {
    // `[k: v]` is the canonical map syntax, including the empty form.
    assert_round_trip("var m = [:];");
    assert_round_trip("var m = [1: \"a\", 2: \"b\"];");
    let (node, _) = parse_str("var m = [1: \"a\"];");
    assert!(dump(&node).contains("MapExpr"));
}

#[test]
fn ternary_basic() {
    assert_round_trip("var x = a > b ? a : b;");
    let (node, _) = parse_str("var x = a > b ? a : b;");
    assert!(dump(&node).contains("TernaryExpr"));
}

#[test]
fn ternary_nested_right_assoc() {
    // `a ? b : c ? d : e` should parse as `a ? b : (c ? d : e)`.
    assert_round_trip("var x = a ? b : c ? d : e;");
}

#[test]
fn instanceof_op() {
    assert_round_trip("if (x instanceof Array) { return 1; }");
    let (node, _) = parse_str("if (x instanceof Array) { return 1; }");
    assert!(dump(&node).contains("BinaryExpr"));
}

#[test]
fn in_op() {
    assert_round_trip("if (k in m) { return 1; }");
}

#[test]
fn not_in_op() {
    assert_round_trip("if (k not in m) { return 1; }");
}

#[test]
fn cast_as_type() {
    assert_round_trip("var x = a as integer;");
    let (node, _) = parse_str("var x = a as integer;");
    let tree = dump(&node);
    assert!(tree.contains("CastExpr"));
    assert!(tree.contains("TypeRef"));
}

#[test]
fn postfix_inc_dec() {
    assert_round_trip("x++;");
    assert_round_trip("y--;");
    let (node, _) = parse_str("x++;");
    assert!(dump(&node).contains("PostfixExpr"));
}

#[test]
fn postfix_non_null_assertion() {
    // `f()!` — the `!` here is non-null assertion, not logical negation.
    assert_round_trip("var x = f()!;");
}

#[test]
fn shift_operators_binary() {
    assert_round_trip("var x = a << 2;");
    assert_round_trip("var x = a >> 1;");
    assert_round_trip("var x = a >>> 1;");
}

#[test]
fn bitwise_ops() {
    assert_round_trip("var x = a & b | c ^ d;");
    let (node, _) = parse_str("var x = a & b | c ^ d;");
    let tree = dump(&node);
    // & binds tighter than ^, ^ tighter than |. Expected:
    // |
    //   &
    //     a
    //     b
    //   ^
    //     c
    //     d
    assert!(tree.matches("BinaryExpr").count() >= 3);
}

#[test]
fn xor_keyword() {
    assert_round_trip("var x = a xor b;");
}

#[test]
fn slice_basic() {
    assert_round_trip("return a[1:3];");
    let (node, _) = parse_str("return a[1:3];");
    assert!(dump(&node).contains("SliceExpr"));
}

#[test]
fn slice_open_ends() {
    assert_round_trip("return a[:3];");
    assert_round_trip("return a[1:];");
    assert_round_trip("return a[::2];");
}

#[test]
fn interval_literal_basic() {
    // Intervals are bracket-delimited; bare `a..b` is NOT an interval.
    assert_round_trip("var r = [1..10];");
    let (node, _) = parse_str("var r = [1..10];");
    assert!(dump(&node).contains("IntervalExpr"));
}

#[test]
fn interval_half_open() {
    assert_round_trip("var r = [1..10[;");
    assert_round_trip("var r = ]1..10];");
    assert_round_trip("var r = ]1..10[;");
}

#[test]
fn interval_open_ended() {
    assert_round_trip("var r = ]..[;");
    assert_round_trip("var r = [..10];");
    assert_round_trip("var r = [1..];");
}

#[test]
fn interval_with_step() {
    assert_round_trip("var r = [1..10:2];");
}

#[test]
fn interval_with_subscript_inside() {
    // Subscript inside an interval — both `[ ]` shapes coexist.
    assert_round_trip("var r = [arr[0]..arr[1]];");
}

#[test]
fn annotation_on_function() {
    assert_round_trip("@deprecated\nfunction old() { return 1; }");
    let (node, _) = parse_str("@deprecated\nfunction old() { return 1; }");
    let tree = dump(&node);
    assert!(tree.contains("Annotation"));
    assert!(tree.contains("FnDecl"));
    // The annotation should be inside the FnDecl node.
    let fn_decl_offset = tree.find("FnDecl").unwrap();
    let annot_offset = tree.find("Annotation").unwrap();
    assert!(
        annot_offset > fn_decl_offset,
        "annotation outside FnDecl:\n{tree}"
    );
}

#[test]
fn annotation_with_args() {
    assert_round_trip("@allow(L0001)\nvar x = 1;");
}

#[test]
fn annotation_on_class_field() {
    assert_round_trip("class A { @deprecated public integer x = 0; }");
    let (node, _) = parse_str("class A { @deprecated public integer x = 0; }");
    let tree = dump(&node);
    assert!(tree.contains("Annotation"));
    assert!(tree.contains("ClassField"));
}

#[test]
fn prefix_inc_dec() {
    assert_round_trip("++a;");
    assert_round_trip("--a[0];");
    assert_round_trip("return ++count;");
    let (node, _) = parse_str("++a;");
    assert!(dump(&node).contains("UnaryExpr"));
}

#[test]
fn anonymous_function_expression() {
    assert_round_trip("var f = function() { return 1; };");
    assert_round_trip("var f = function(integer x) -> integer { return x; };");
    // Immediately invoked: `(function() { ... })()`.
    assert_round_trip("var v = (function() { return 42; })();");
}

#[test]
fn fat_arrow_lambda() {
    assert_round_trip("var f = x => x + 1;");
    assert_round_trip("var f = (a, b) => a * b;");
}

#[test]
fn fat_arrow_return_type() {
    // Some upstream tests use `=>` between the param list and the
    // return type: `function f() => integer { ... }`.
    assert_round_trip("function f() => integer { return 1; }");
}

#[test]
fn class_name_keyword() {
    // `class.name` reads the current class name as a string.
    assert_round_trip("class A { public m() { return class.name; } }");
}

#[test]
fn reference_prefix_at() {
    // Legacy v1 reference operator on an identifier.
    assert_round_trip("var b = @a;");
}

#[test]
fn empty_map() {
    // `[:]` is the canonical empty-map literal.
    assert_round_trip("var m = [:];");
    let (node, _) = parse_str("var m = [:];");
    assert!(dump(&node).contains("MapExpr"));
}

#[test]
fn global_decl() {
    assert_round_trip("global x = 1;");
    assert_round_trip("global integer count = 0;");
    assert_round_trip("global a = 1, b = 2;");
    let (node, _) = parse_str("global x = 1;");
    let tree = dump(&node);
    assert!(tree.contains("VarDeclStmt"));
    assert!(tree.contains("KwGlobal"));
}

#[test]
fn nested_generic_types() {
    assert_round_trip("Map<integer, Array<integer>> a = [:];");
    assert_round_trip("Map<integer, Map<integer, real>> coefs = [:];");
}

#[test]
fn triple_nested_generic() {
    // Three levels close in a single `>>>`.
    assert_round_trip("Array<Array<Array<integer>>> deep = [];");
}

#[test]
fn function_arrow_type() {
    assert_round_trip("Function<integer => any> f = null;");
    assert_round_trip("Function< => real> f = null;");
}

#[test]
fn function_return_fat_arrow() {
    assert_round_trip("function f() => integer { return 1; }");
}

#[test]
fn null_as_type() {
    assert_round_trip("Array<integer> | null x = null;");
}

#[test]
fn empty_statement_tolerated() {
    let (node, diags) = parse_str("var x = 1; ; var y = 2;");
    assert!(diags.is_empty(), "diagnostics: {diags:?}");
    assert_eq!(node.text().to_string(), "var x = 1; ; var y = 2;");
}

#[test]
fn angle_set_literal() {
    assert_round_trip("var s = <1, 2, 3>;");
    assert_round_trip("var s = <>;");
    let (node, _) = parse_str("var s = <1, 2, 3>;");
    assert!(dump(&node).contains("SetExpr"));
}

#[test]
fn angle_set_with_gt_inside_set() {
    // The closing `>` must not be consumed as a `>` binary inside
    // the element expressions.
    assert_round_trip("var s = <1, 2>; return s;");
}

#[test]
fn keyword_and_or_as_binary() {
    assert_round_trip("return a and b;");
    assert_round_trip("return a or b;");
    assert_round_trip("return a and b or c;");
}

#[test]
fn keyword_as_field_name() {
    assert_round_trip("return obj.class;");
    assert_round_trip("return [0..1].class;");
}

#[test]
fn zero_arg_lambda() {
    assert_round_trip("var f = -> 12;");
    assert_round_trip("var f = => [];");
    assert_round_trip("return (-> 42)();");
}

#[test]
fn soft_return() {
    assert_round_trip("function f(x) { return? x; return 12; }");
}

#[test]
fn error_recovery_keeps_parsing() {
    // `var` not followed by an identifier should NOT halt the parser.
    let (node, diags) = parse_str("var ; var y = 2;");
    assert_eq!(
        node.text().to_string(),
        "var ; var y = 2;",
        "round-trip must hold even with errors",
    );
    assert!(!diags.is_empty(), "expected at least one diagnostic");
    // The second declaration should still be visible.
    let file = SourceFile::cast(node).unwrap();
    let names: Vec<String> = file
        .stmts()
        .filter_map(|s| match s {
            Stmt::VarDecl(v) => v.name().map(|n| n.text().to_string()),
            _ => None,
        })
        .collect();
    assert!(
        names.contains(&"y".to_string()),
        "lost the second decl: {names:?}"
    );
}
