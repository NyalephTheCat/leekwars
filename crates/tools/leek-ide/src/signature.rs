//! Format one-line declaration signatures for hover, completion,
//! etc. Pulls everything from the green CST so we don't need a
//! typed-AST roundtrip.
//!
//! Output shape uses Leekscript's actual prefix-type syntax:
//!   - `function f(integer x, real y) -> string`
//!   - `class Cat extends Animal`
//!   - `integer x` (typed) / `var x` (untyped)
//!   - `private static integer count`
//!   - `constructor(string name)`
//!
//! Param types and return types default to `any` when omitted —
//! matches what the inference layer sees.

use leek_syntax::language::NodeOrToken;
use leek_syntax::{SyntaxKind, SyntaxNode, SyntaxToken};

/// Inspect `node` and render a one-line signature for it. Returns
/// `None` for nodes we don't have a signature shape for (the
/// caller falls back to the inferred type).
pub fn signature_for(node: &SyntaxNode) -> Option<String> {
    signature_for_with(node, &|_| None)
}

/// Like [`signature_for`] but consults `infer` for a return/field type
/// when the source omits the annotation. `infer(node)` is given the
/// declaration node and returns a display type string (e.g. `integer`)
/// or `None` when nothing better than `any` is known. Lets hover show an
/// inferred type for unannotated functions, methods, and fields.
pub fn signature_for_with(
    node: &SyntaxNode,
    infer: &dyn Fn(&SyntaxNode) -> Option<String>,
) -> Option<String> {
    match node.kind() {
        SyntaxKind::FnDecl => Some(function_signature(node, infer)),
        SyntaxKind::ClassDecl => Some(class_signature(node)),
        SyntaxKind::ClassMethod => Some(method_signature(node, infer)),
        SyntaxKind::ClassConstructor => Some(constructor_signature(node)),
        SyntaxKind::ClassField => Some(field_signature(node, infer)),
        SyntaxKind::VarDeclStmt => var_signature(node),
        SyntaxKind::Param => Some(param_signature(node)),
        _ => None,
    }
}

fn function_signature(node: &SyntaxNode, infer: &dyn Fn(&SyntaxNode) -> Option<String>) -> String {
    let name = first_decl_ident_text(node).unwrap_or_else(|| "<anonymous>".into());
    let params = render_param_list(node);
    let ret = render_return_type(node)
        .or_else(|| infer(node))
        .unwrap_or_else(|| "any".into());
    format!("function {name}({params}) -> {ret}")
}

fn method_signature(node: &SyntaxNode, infer: &dyn Fn(&SyntaxNode) -> Option<String>) -> String {
    let prefix = method_modifiers(node);
    let name = first_decl_ident_text(node).unwrap_or_else(|| "<anonymous>".into());
    let params = render_param_list(node);
    // Methods declare their return type as a leading type prefix
    // (`string describe()`), matching Leekscript's actual syntax. A
    // trailing `-> T` form is tolerated as a fallback, then the inferred
    // return type, then `any` — so the return type is always shown
    // (matching the `function … -> any` form).
    let ret = first_type_ref_text(node)
        .or_else(|| render_return_type(node))
        .or_else(|| infer(node))
        .unwrap_or_else(|| "any".into());
    format!("{prefix}{ret} {name}({params})")
}

fn constructor_signature(node: &SyntaxNode) -> String {
    let params = render_param_list(node);
    format!("constructor({params})")
}

fn class_signature(node: &SyntaxNode) -> String {
    let name = first_decl_ident_text(node).unwrap_or_else(|| "<anonymous>".into());
    let parent = class_parent_name(node);
    let mut sig = format!("class {name}");
    if let Some(p) = parent {
        sig.push_str(" extends ");
        sig.push_str(&p);
    }
    sig
}

fn field_signature(node: &SyntaxNode, infer: &dyn Fn(&SyntaxNode) -> Option<String>) -> String {
    let prefix = field_modifiers(node);
    let name = first_decl_ident_text(node).unwrap_or_else(|| "<anonymous>".into());
    let ty = first_type_ref_text(node)
        .or_else(|| infer(node))
        .unwrap_or_else(|| "any".into());
    format!("{prefix}{ty} {name}")
}

fn var_signature(node: &SyntaxNode) -> Option<String> {
    // A single decl with a typed prefix → `integer x`. With
    // `var x`, the keyword is the first significant token and
    // there's no TypeRef. Multi-name decls (`var x, y`) fall back
    // to a less specific shape since hover targets one name.
    let names = collect_var_names(node);
    if names.len() != 1 {
        return None;
    }
    let name = &names[0];
    if let Some(kw) = leading_keyword(node) {
        // `var x` or `global x` — keep the keyword for clarity.
        let kw_label = match kw {
            SyntaxKind::KwGlobal => "global",
            _ => "var",
        };
        let ty = first_type_ref_text(node);
        match ty {
            Some(t) => Some(format!("{kw_label} {t} {name}")),
            None => Some(format!("{kw_label} {name}")),
        }
    } else {
        // Typed local: `integer x = 5` → `integer x`.
        let ty = first_type_ref_text(node).unwrap_or_else(|| "any".into());
        Some(format!("{ty} {name}"))
    }
}

fn param_signature(node: &SyntaxNode) -> String {
    let by_ref = node
        .children_with_tokens()
        .filter_map(leek_syntax::language::NodeOrToken::into_token)
        .any(|t| t.kind() == SyntaxKind::At);
    let name = param_name(node).unwrap_or_else(|| "<param>".into());
    let ty = first_type_ref_text(node);
    let prefix = if by_ref { "@" } else { "" };
    match ty {
        Some(t) => format!("{t} {prefix}{name}"),
        None => format!("{prefix}{name}"),
    }
}

fn render_param_list(node: &SyntaxNode) -> String {
    let param_list = node.children().find(|c| c.kind() == SyntaxKind::ParamList);
    let Some(params) = param_list else {
        return String::new();
    };
    let parts: Vec<String> = params
        .children()
        .filter(|c| c.kind() == SyntaxKind::Param)
        .map(|p| param_signature(&p))
        .collect();
    parts.join(", ")
}

/// Pull the declared return type's TypeRef text — the TypeRef that
/// follows the ParamList and an Arrow/FatArrow token.
fn render_return_type(node: &SyntaxNode) -> Option<String> {
    let mut past_params = false;
    let mut saw_arrow = false;
    for el in node.children_with_tokens() {
        match el {
            NodeOrToken::Node(n) => {
                if !past_params {
                    if n.kind() == SyntaxKind::ParamList {
                        past_params = true;
                    }
                    continue;
                }
                if saw_arrow && n.kind() == SyntaxKind::TypeRef {
                    return Some(typeref_text(&n));
                }
            }
            NodeOrToken::Token(t) => {
                if past_params && matches!(t.kind(), SyntaxKind::Arrow | SyntaxKind::FatArrow) {
                    saw_arrow = true;
                }
            }
        }
    }
    None
}

/// Skip past any leading TypeRef and `@` to land at the first
/// Ident — that's the declared name on Params/Fields/VarDecls.
fn first_decl_ident_text(node: &SyntaxNode) -> Option<String> {
    let mut past_typeref = false;
    for el in node.children_with_tokens() {
        match el {
            NodeOrToken::Node(n) if n.kind() == SyntaxKind::TypeRef => {
                past_typeref = true;
            }
            NodeOrToken::Token(t) if t.kind() == SyntaxKind::Ident => {
                // For non-typed decls (or methods/fields with no leading
                // type annotation, e.g. `purr()`), the first Ident is
                // the name. For typed methods/fields the name follows
                // the TypeRef.
                if !needs_type_skip(node) || past_typeref || !has_type_prefix(node) {
                    return Some(t.text().to_string());
                }
            }
            _ => {}
        }
    }
    None
}

/// True for nodes where the leading TypeRef is a *type annotation*
/// (the name follows it), not a parameter or super-class token.
/// Used by [`first_decl_ident_text`] to decide whether to skip past
/// a leading TypeRef before grabbing the Ident.
fn needs_type_skip(node: &SyntaxNode) -> bool {
    matches!(
        node.kind(),
        SyntaxKind::Param
            | SyntaxKind::VarDeclStmt
            | SyntaxKind::ClassField
            | SyntaxKind::ClassMethod
    )
}

fn param_name(node: &SyntaxNode) -> Option<String> {
    // Same logic as first_decl_ident_text but specifically for
    // Param so the type prefix doesn't get returned as the name.
    let mut past_typeref = false;
    for el in node.children_with_tokens() {
        match el {
            NodeOrToken::Node(n) if n.kind() == SyntaxKind::TypeRef => {
                past_typeref = true;
            }
            NodeOrToken::Node(_) => {}
            NodeOrToken::Token(t) => {
                if t.kind() == SyntaxKind::Ident && (past_typeref || !has_type_prefix(node)) {
                    return Some(t.text().to_string());
                }
            }
        }
    }
    None
}

fn has_type_prefix(node: &SyntaxNode) -> bool {
    node.children().any(|c| c.kind() == SyntaxKind::TypeRef)
}

fn first_type_ref_text(node: &SyntaxNode) -> Option<String> {
    node.children()
        .find(|c| c.kind() == SyntaxKind::TypeRef)
        .map(|n| typeref_text(&n))
}

/// Render a TypeRef as Leekscript source — `Array<integer>`,
/// `Map<string, real>`, `Function<P => R>`, etc. We just stitch
/// the token text back together since the CST preserves it
/// losslessly.
pub fn typeref_text(node: &SyntaxNode) -> String {
    let mut out = String::new();
    for tok in node
        .descendants_with_tokens()
        .filter_map(leek_syntax::language::NodeOrToken::into_token)
    {
        if matches!(
            tok.kind(),
            SyntaxKind::Whitespace | SyntaxKind::LineComment | SyntaxKind::BlockComment
        ) {
            // Collapse internal whitespace to a single space so the
            // signature reads on one line.
            if !out.ends_with(' ') && !out.is_empty() {
                out.push(' ');
            }
            continue;
        }
        out.push_str(tok.text());
    }
    out.trim().to_string()
}

fn class_parent_name(node: &SyntaxNode) -> Option<String> {
    let mut saw_extends = false;
    for tok in node
        .children_with_tokens()
        .filter_map(leek_syntax::language::NodeOrToken::into_token)
    {
        if tok.kind() == SyntaxKind::KwExtends {
            saw_extends = true;
        } else if saw_extends && tok.kind() == SyntaxKind::Ident {
            return Some(tok.text().to_string());
        }
    }
    None
}

fn collect_var_names(node: &SyntaxNode) -> Vec<String> {
    // VarDeclStmt structure: either
    //   KwVar/KwGlobal Ident (, Ident)*       (untyped)
    // or
    //   TypeRef Ident (, Ident)*              (typed-prefix)
    //
    // The Idents are name tokens (TypeRef is a separate node);
    // skip past Eq/initializer expressions and resume after the
    // next Comma.
    let mut names = Vec::new();
    let mut expect_name = true;
    for el in node.children_with_tokens() {
        match el {
            NodeOrToken::Token(t) => match t.kind() {
                SyntaxKind::KwVar | SyntaxKind::KwGlobal | SyntaxKind::Comma => expect_name = true,
                SyntaxKind::Eq | SyntaxKind::Semicolon => expect_name = false,
                SyntaxKind::Ident if expect_name => {
                    names.push(t.text().to_string());
                    expect_name = false;
                }
                _ => {}
            },
            NodeOrToken::Node(n) => {
                // The leading TypeRef should NOT block taking the
                // following Ident as a name — re-enable.
                if n.kind() == SyntaxKind::TypeRef {
                    expect_name = true;
                } else {
                    // An initializer expression: don't grab names
                    // from inside it.
                    expect_name = false;
                }
            }
        }
    }
    names
}

fn leading_keyword(node: &SyntaxNode) -> Option<SyntaxKind> {
    for el in node.children_with_tokens() {
        if let NodeOrToken::Token(t) = el {
            match t.kind() {
                SyntaxKind::Whitespace | SyntaxKind::LineComment | SyntaxKind::BlockComment => {}
                SyntaxKind::KwVar | SyntaxKind::KwGlobal => return Some(t.kind()),
                _ => return None,
            }
        }
    }
    None
}

fn method_modifiers(node: &SyntaxNode) -> String {
    modifier_prefix(
        node,
        &[
            (SyntaxKind::KwPrivate, "private "),
            (SyntaxKind::KwProtected, "protected "),
            (SyntaxKind::KwPublic, "public "),
            (SyntaxKind::KwStatic, "static "),
            (SyntaxKind::KwFinal, "final "),
        ],
    )
}

fn field_modifiers(node: &SyntaxNode) -> String {
    method_modifiers(node)
}

fn modifier_prefix(node: &SyntaxNode, table: &[(SyntaxKind, &str)]) -> String {
    let mut out = String::new();
    let toks: Vec<SyntaxToken> = node
        .children_with_tokens()
        .filter_map(leek_syntax::language::NodeOrToken::into_token)
        .collect();
    for &(kind, label) in table {
        if toks.iter().any(|t| t.kind() == kind) {
            out.push_str(label);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use leek_parser::parse;
    use leek_span::SourceId;
    use leek_syntax::{SyntaxNode, Version};

    fn first_decl(src: &str, kind: SyntaxKind) -> SyntaxNode {
        let source = SourceId::new(1).unwrap();
        let parsed = parse(src, source, Version::V4);
        let root = SyntaxNode::new_root(parsed.green);
        root.descendants()
            .find(|n| n.kind() == kind)
            .expect("decl present")
    }

    #[test]
    fn function_with_typed_params_and_arrow_return() {
        let node = first_decl(
            "function f(integer x, real y) -> string { return \"\" }\n",
            SyntaxKind::FnDecl,
        );
        assert_eq!(
            signature_for(&node).unwrap(),
            "function f(integer x, real y) -> string"
        );
    }

    #[test]
    fn function_without_annotations_falls_back_to_any() {
        let node = first_decl("function g(a, b) {}\n", SyntaxKind::FnDecl);
        assert_eq!(signature_for(&node).unwrap(), "function g(a, b) -> any");
    }

    #[test]
    fn class_with_extends() {
        let node = first_decl("class Cat extends Animal {}\n", SyntaxKind::ClassDecl);
        assert_eq!(signature_for(&node).unwrap(), "class Cat extends Animal");
    }

    #[test]
    fn class_without_extends() {
        let node = first_decl("class Cat {}\n", SyntaxKind::ClassDecl);
        assert_eq!(signature_for(&node).unwrap(), "class Cat");
    }

    #[test]
    fn typed_var_decl() {
        let node = first_decl("integer x = 5\n", SyntaxKind::VarDeclStmt);
        assert_eq!(signature_for(&node).unwrap(), "integer x");
    }

    #[test]
    fn untyped_var_decl() {
        let node = first_decl("var x = 5\n", SyntaxKind::VarDeclStmt);
        assert_eq!(signature_for(&node).unwrap(), "var x");
    }

    #[test]
    fn global_var_decl() {
        let node = first_decl("global x = 5\n", SyntaxKind::VarDeclStmt);
        assert_eq!(signature_for(&node).unwrap(), "global x");
    }

    #[test]
    fn multi_decl_returns_none() {
        let node = first_decl("var x = 1, y = 2;\n", SyntaxKind::VarDeclStmt);
        assert!(signature_for(&node).is_none());
    }

    #[test]
    fn ref_param_uses_at_prefix() {
        let node = first_decl("function swap(@x, @y) {}\n", SyntaxKind::FnDecl);
        assert_eq!(
            signature_for(&node).unwrap(),
            "function swap(@x, @y) -> any"
        );
    }

    #[test]
    fn unannotated_method_renders_any_return() {
        // Symmetry with functions: a method with no return type still
        // renders one (`any`), in prefix position.
        let node = first_decl("class C { meow() {} }\n", SyntaxKind::ClassMethod);
        assert_eq!(signature_for(&node).unwrap(), "any meow()");
    }

    #[test]
    fn function_uses_inferred_return_when_unannotated() {
        let node = first_decl("function f() {}\n", SyntaxKind::FnDecl);
        let sig = signature_for_with(&node, &|n| {
            (n.kind() == SyntaxKind::FnDecl).then(|| "integer".to_string())
        });
        assert_eq!(sig.unwrap(), "function f() -> integer");
    }

    #[test]
    fn method_uses_inferred_return_when_unannotated() {
        let node = first_decl("class C { meow() {} }\n", SyntaxKind::ClassMethod);
        let sig = signature_for_with(&node, &|n| {
            (n.kind() == SyntaxKind::ClassMethod).then(|| "integer".to_string())
        });
        assert_eq!(sig.unwrap(), "integer meow()");
    }

    #[test]
    fn field_uses_inferred_type_when_unannotated() {
        let node = first_decl("class C { x }\n", SyntaxKind::ClassField);
        let sig = signature_for_with(&node, &|n| {
            (n.kind() == SyntaxKind::ClassField).then(|| "string".to_string())
        });
        assert_eq!(sig.unwrap(), "string x");
    }

    #[test]
    fn explicit_annotation_wins_over_inference() {
        // A declared return type is never overridden by the inferred one.
        let node = first_decl(
            "function f() -> string { return \"\" }\n",
            SyntaxKind::FnDecl,
        );
        let sig = signature_for_with(&node, &|_| Some("integer".to_string()));
        assert_eq!(sig.unwrap(), "function f() -> string");
    }
}
