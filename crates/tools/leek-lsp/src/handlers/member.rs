//! Shared `recv.member` resolution used by hover, signatureHelp, and
//! definition: find the member access under the cursor, resolve the
//! receiver expression to its class, and walk the inheritance chain
//! to the member's declaration node.

use leek_parser::ast::{AstNode, Expr, FieldExpr};
use leek_syntax::{SyntaxKind, SyntaxNode, SyntaxToken, language::NodeOrToken};
use leek_types::Type;

/// The innermost `FieldExpr` whose *member* token covers `offset`,
/// together with that token. `None` when the cursor isn't on the
/// member half of a `recv.member` access.
pub(crate) fn field_access_at(root: &SyntaxNode, offset: u32) -> Option<(FieldExpr, SyntaxToken)> {
    let mut best: Option<(SyntaxNode, SyntaxToken)> = None;
    for node in root.descendants() {
        if node.kind() != SyntaxKind::FieldExpr {
            continue;
        }
        let Some(field) = FieldExpr::cast(node.clone()).and_then(|f| f.field()) else {
            continue;
        };
        let r = field.text_range();
        if u32::from(r.start()) <= offset && offset < u32::from(r.end()) {
            let smaller = best
                .as_ref()
                .is_none_or(|(b, _)| node.text_range().len() < b.text_range().len());
            if smaller {
                best = Some((node.clone(), field));
            }
        }
    }
    let (node, tok) = best?;
    Some((FieldExpr::cast(node)?, tok))
}

/// The class a member-access receiver denotes: a `ClassInstance` type
/// from the type table (instances, `this`, `super`), the receiver
/// variable's declared/inferred init type, or — for a static
/// `Class.member` — the receiver name itself when it names a class.
pub(crate) fn base_class_name(
    root: &SyntaxNode,
    resolve_art: Option<&leek_resolver::pipeline::ResolveArtifact>,
    table: &leek_types::TypeTable,
    base: &Expr,
) -> Option<String> {
    let range = base.syntax().text_range();
    let start = u32::from(range.start());
    let end = u32::from(range.end());
    // A range query over the whole base expression — a point query at
    // its start would land on the chain's *first* link (`fm` in
    // `fm.leek.cell`) and resolve the wrong class for chained access.
    if let Some(entry) = table.spanning(start, end)
        && let Some(name) = class_name_of_type(&entry.ty)
    {
        return Some(name);
    }
    // A plain `var c = new Cat()` isn't recorded with a type at its use
    // sites in non-strict mode, but its initializer *is* typed. Resolve
    // the receiver to its declaration and read the init type.
    if let Some(name) = receiver_class_via_decl(root, resolve_art, table, start) {
        return Some(name);
    }
    // Static receiver: `Animal.make()` — `Animal` is a class name, not
    // a typed value, so the type table reports `any`.
    if let Expr::Name(nr) = base
        && let Some(ident) = nr.ident()
        && find_class_decl_by_name(root, ident.text()).is_some()
    {
        return Some(ident.text().to_string());
    }
    // `this.member` — fall back to the enclosing class from the CST when
    // the type table has no instance entry (e.g. a mid-edit buffer).
    if base
        .syntax()
        .first_token()
        .is_some_and(|t| t.kind() == SyntaxKind::KwThis)
    {
        return enclosing_class_of(base.syntax());
    }
    None
}

/// Resolve a receiver variable to its declaration and read the class
/// from the declaration's initializer type (`var c = new Cat()`).
fn receiver_class_via_decl(
    root: &SyntaxNode,
    resolve_art: Option<&leek_resolver::pipeline::ResolveArtifact>,
    table: &leek_types::TypeTable,
    base_start: u32,
) -> Option<String> {
    let art = resolve_art?;
    let r = art.table.reference_at(base_start)?;
    let sym = art.table.symbol(r.target)?;
    let entry = initializer_type(root, table, sym.def_span.start)?;
    class_name_of_type(&entry.ty)
}

/// The checker's type for the initializer that belongs to the binding
/// whose name token starts at `name_offset`.
///
/// For a `var`/`global` declaration this resolves the *declarator's
/// own* initializer expression — `var a = new A(), b = new B()` reads
/// `new B()` for `b`, never the statement's largest expression (which
/// could be a sibling declarator's). Other declaration shapes (params,
/// class fields) fall back to the largest typed expression inside the
/// declaration node.
pub(crate) fn initializer_type<'t>(
    root: &SyntaxNode,
    table: &'t leek_types::TypeTable,
    name_offset: u32,
) -> Option<&'t leek_types::TypedExpr> {
    let name_tok = root.token_at_offset(name_offset.into()).right_biased()?;
    let decl = enclosing_decl_node(&name_tok.parent()?)?;
    if decl.kind() == SyntaxKind::VarDeclStmt {
        let mut seen_name = false;
        for el in decl.children_with_tokens() {
            match el {
                NodeOrToken::Token(t) => {
                    if t.text_range() == name_tok.text_range() {
                        seen_name = true;
                    } else if seen_name && t.kind() == SyntaxKind::Comma {
                        // Next declarator — this name had no initializer.
                        return None;
                    }
                }
                NodeOrToken::Node(n) => {
                    if seen_name && Expr::cast(n.clone()).is_some() {
                        let r = n.text_range();
                        return table.spanning(r.start().into(), r.end().into());
                    }
                }
            }
        }
        return None;
    }
    let r = decl.text_range();
    largest_expr_type(table, r.start().into(), r.end().into())
}

/// The largest typed expression contained in `[lo, hi)` — an
/// approximation of "the declaration's value expression" for shapes
/// where the exact initializer node isn't recoverable.
pub(crate) fn largest_expr_type(
    table: &leek_types::TypeTable,
    lo: u32,
    hi: u32,
) -> Option<&leek_types::TypedExpr> {
    table
        .exprs
        .iter()
        .filter(|e| e.span.start >= lo && e.span.end <= hi)
        .max_by_key(|e| e.span.end - e.span.start)
}

/// Unwrap `Nullable`/`Array` wrappers down to a `ClassInstance` name.
pub(crate) fn class_name_of_type(ty: &Type) -> Option<String> {
    match ty {
        Type::ClassInstance(n, _) => Some(n.clone()),
        Type::Nullable(inner) | Type::Array(inner) => class_name_of_type(inner),
        _ => None,
    }
}

/// Walk `class_name` and its ancestors looking for a member named
/// `member`. Returns the member's CST node (method / field /
/// constructor) for signature rendering.
pub(crate) fn find_member_in_chain(
    root: &SyntaxNode,
    class_name: &str,
    member: &str,
) -> Option<SyntaxNode> {
    let mut current = Some(class_name.to_string());
    for _ in 0..64 {
        let cls = find_class_decl_by_name(root, &current?)?;
        if let Some(m) = class_member_named(&cls, member) {
            return Some(m);
        }
        current = class_parent_name_of(&cls);
    }
    None
}

/// Find a top-level `class <name>` declaration node.
pub(crate) fn find_class_decl_by_name(root: &SyntaxNode, name: &str) -> Option<SyntaxNode> {
    root.descendants().find(|n| {
        n.kind() == SyntaxKind::ClassDecl
            && n.children_with_tokens()
                .filter_map(NodeOrToken::into_token)
                .find(|t| t.kind() == SyntaxKind::Ident)
                .is_some_and(|t| t.text() == name)
    })
}

/// The `extends Parent` name on a class declaration, if any.
fn class_parent_name_of(cls: &SyntaxNode) -> Option<String> {
    let mut saw_extends = false;
    for el in cls.children_with_tokens() {
        if let Some(t) = el.into_token() {
            if t.kind() == SyntaxKind::KwExtends {
                saw_extends = true;
            } else if saw_extends && t.kind() == SyntaxKind::Ident {
                return Some(t.text().to_string());
            }
        }
    }
    None
}

/// Find the class member (method / field / constructor) whose declared
/// name is `name` within `cls`'s body.
fn class_member_named(cls: &SyntaxNode, name: &str) -> Option<SyntaxNode> {
    let body = cls.children().find(|c| c.kind() == SyntaxKind::ClassBody)?;
    body.children().find(|member| match member.kind() {
        SyntaxKind::ClassConstructor => name == "constructor",
        SyntaxKind::ClassMethod | SyntaxKind::ClassField => {
            member_decl_name(member).as_deref() == Some(name)
        }
        _ => false,
    })
}

/// The declared name of a class method/field — the first `Ident` token
/// child. Any leading return/field type sits inside a `TypeRef` node,
/// so its name isn't a direct token and won't be mistaken for this.
pub(crate) fn member_decl_name(member: &SyntaxNode) -> Option<String> {
    member_decl_name_token(member).map(|t| t.text().to_string())
}

/// The name token of a class method/field declaration. `None` for a
/// constructor (its "name" is the `constructor` keyword).
pub(crate) fn member_decl_name_token(member: &SyntaxNode) -> Option<SyntaxToken> {
    member
        .children_with_tokens()
        .filter_map(NodeOrToken::into_token)
        .find(|t| t.kind() == SyntaxKind::Ident)
}

/// Walk up the CST from `n` until we hit a node whose kind is a
/// declaration we know how to render a signature for. The
/// resolver records `full_span` as the identifier token only for
/// top-level function/method symbols, so the smallest covering
/// node lands on `Ident` rather than the enclosing FnDecl/etc.
/// — this helper does the climb.
pub(crate) fn enclosing_decl_node(n: &SyntaxNode) -> Option<SyntaxNode> {
    let mut cur = Some(n.clone());
    while let Some(node) = cur {
        if matches!(
            node.kind(),
            SyntaxKind::FnDecl
                | SyntaxKind::ClassDecl
                | SyntaxKind::ClassMethod
                | SyntaxKind::ClassConstructor
                | SyntaxKind::ClassField
                | SyntaxKind::VarDeclStmt
                | SyntaxKind::Param
        ) {
            return Some(node);
        }
        cur = node.parent();
    }
    None
}

/// Find the smallest descendant of `root` whose token range fully
/// contains `span`. Used to look up the CST declaration node for
/// signature rendering.
pub(crate) fn node_covering(root: &SyntaxNode, span: leek_span::Span) -> Option<SyntaxNode> {
    fn covers(n: &SyntaxNode, span: leek_span::Span) -> bool {
        let r = n.text_range();
        u32::from(r.start()) <= span.start && span.end <= u32::from(r.end())
    }
    if !covers(root, span) {
        return None;
    }
    // Walk down through the smallest covering child until we hit
    // a node whose children no longer cover.
    let mut current = root.clone();
    loop {
        let next = current.children().find(|c| covers(c, span));
        match next {
            Some(n) => current = n,
            None => return Some(current),
        }
    }
}

/// The name of the class enclosing `node` (walking up to the nearest
/// `ClassDecl`), for member-type lookups.
pub(crate) fn enclosing_class_of(node: &SyntaxNode) -> Option<String> {
    let mut cur = node.parent();
    while let Some(n) = cur {
        if n.kind() == SyntaxKind::ClassDecl {
            return member_decl_name(&n);
        }
        cur = n.parent();
    }
    None
}
