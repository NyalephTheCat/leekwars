//! `textDocument/completion` — identifier + keyword + builtin +
//! snippet completion, plus a member-completion mode triggered by
//! `.`.
//!
//! ## Sources
//!
//! 1. **User symbols** discovered by the resolver — every declared
//!    function, class, global, local, param. Each item's `detail`
//!    is the full one-line signature (function header, class
//!    `extends` clause, typed `var`) when we can render one from
//!    the CST; otherwise the inferred type's name.
//! 2. **Builtin functions** from [`BUILTIN_FNS`] (with arity in
//!    the detail string) plus every name in [`BUILTINS`] not
//!    already covered by the arity table.
//! 3. **Builtin constants** from [`BUILTIN_CONSTANTS`].
//! 4. **Keywords** + a small **snippet** set.
//!
//! ## Member mode
//!
//! When the cursor is positioned right after a `.` token we switch
//! to member completion:
//!
//! - `this.` — list the enclosing class's fields, methods, and
//!   constructor.
//! - `Integer.` / `Real.` / `String.` / `Array.` / `Map.` / `Set.`
//!   — list every entry of [`FINAL_BUILTIN_FIELDS`] under that
//!   prefix.
//!
//! Receiver-typed member completion (e.g. `myCat.` where `myCat`
//! is `ClassInstance("Cat")`) would need to consult the type
//! table; we resolve that path for declared classes via the
//! resolver's symbol table here too.

use leek_resolver::SymbolKind;
use leek_resolver::builtins::{BUILTIN_CONSTANTS, BUILTIN_FNS, BUILTINS, FINAL_BUILTIN_FIELDS};
use leek_syntax::{SyntaxKind, SyntaxNode};
use leek_types::Type;
use tower_lsp::lsp_types as lsp;

use crate::util::position::position_to_offset;
use crate::workspace::Workspace;
use leek_ide::signature::signature_for;

pub fn handle(
    ws: &Workspace,
    uri: &lsp::Url,
    pos: lsp::Position,
) -> Option<lsp::CompletionResponse> {
    let doc = ws.doc(uri)?;
    let offset = position_to_offset(doc.pos_map(), pos)?;

    let run = crate::pipeline::run(ws, uri, leek_recipes::Target::TypeChecked)?;
    let green = &run.get::<leek_parser::pipeline::GreenTreeArtifact>()?.0;
    let root = SyntaxNode::new_root(green.clone());

    // Member mode: did the user just type `.`?
    if let Some((receiver, receiver_start)) = member_receiver(&doc.text, offset)
        && let Some(items) = member_items(&run, &root, receiver, receiver_start) {
            return Some(lsp::CompletionResponse::Array(items));
        }
        // Fall through to global suggestions if we can't resolve
        // the receiver — better than offering nothing.

    Some(lsp::CompletionResponse::Array(global_items(&run, &root, uri)))
}

// ─── completionItem/resolve ─────────────────────────────────────────

/// Identifying payload stashed on each resolvable completion item.
/// The expensive part — the symbol's documentation — is *not* computed
/// during `handle`; it is filled in lazily here only for the one item
/// the editor focuses. The `uri` lets us re-find the user symbol's
/// source; `def_start` points at its declaration for doc-comment
/// extraction.
#[derive(serde::Serialize, serde::Deserialize)]
struct ResolveData {
    uri: String,
    name: String,
    /// `"user"` for a declared symbol, `"builtin"` for a library/stdlib
    /// name resolved from the embedded signature headers.
    kind: String,
    /// Byte offset of the declaration (user symbols only).
    def_start: Option<u32>,
}

/// `completionItem/resolve` — attach `documentation` to the focused
/// item. Returns the item unchanged when it carries no resolvable
/// `data` (keywords, snippets, constants) or is already resolved.
pub fn resolve(ws: &Workspace, mut item: lsp::CompletionItem) -> lsp::CompletionItem {
    if item.documentation.is_some() {
        return item; // already resolved
    }
    let Some(data) = item.data.take() else {
        return item;
    };
    let Ok(data) = serde_json::from_value::<ResolveData>(data) else {
        return item;
    };

    let doc = match data.kind.as_str() {
        "builtin" => builtin_documentation(&data.name),
        "user" => user_documentation(ws, &data),
        _ => None,
    };
    if let Some(markdown) = doc {
        item.documentation = Some(lsp::Documentation::MarkupContent(lsp::MarkupContent {
            kind: lsp::MarkupKind::Markdown,
            value: markdown,
        }));
    }
    item
}

/// Build markdown documentation for a builtin / library name from the
/// embedded `.leek` signature headers: every overload's signature in a
/// code block, followed by the first available doc-comment.
fn builtin_documentation(name: &str) -> Option<String> {
    let sigs = leek_ide::library_sigs::library_signatures(name)?;
    if sigs.is_empty() {
        return None;
    }
    let body = sigs
        .iter()
        .map(|s| s.signature.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    let mut out = format!("```leekscript\n{body}\n```");
    if let Some(d) = sigs.iter().find_map(|s| s.doc.as_deref())
        && !d.trim().is_empty()
    {
        out.push_str("\n\n");
        out.push_str(d.trim());
    }
    Some(out)
}

/// Pull the doc-comment that sits above a user symbol's declaration.
fn user_documentation(ws: &Workspace, data: &ResolveData) -> Option<String> {
    let uri = lsp::Url::parse(&data.uri).ok()?;
    let doc = ws.doc(&uri)?;
    let start = data.def_start?;
    let comment = leek_ide::doc::doc_comment_before(&doc.text, start)?;
    let comment = comment.trim();
    (!comment.is_empty()).then(|| comment.to_string())
}

// ─── member completion ──────────────────────────────────────────────

/// If the character immediately before `offset` is a `.`, return
/// the receiver-name text AND its byte offset within `text`.
/// Otherwise return `None`. The byte offset is essential for
/// type-table lookups (we query the type at the receiver's start
/// to find its `ClassInstance(N)` etc.).
fn member_receiver(text: &str, offset: u32) -> Option<(&str, u32)> {
    let off = offset as usize;
    if off == 0 || off > text.len() {
        return None;
    }
    let before = &text[..off];
    // Strip trailing identifier chars that the user has already
    // started typing (the partial member name).
    let partial_len = before
        .as_bytes()
        .iter()
        .rev()
        .take_while(|b| is_ident_byte(**b))
        .count();
    let before = &before[..before.len() - partial_len];
    let trimmed = before.trim_end();
    if !trimmed.ends_with('.') {
        return None;
    }
    let after_dot = trimmed.len();
    let bytes = trimmed.as_bytes();
    let mut end = after_dot - 1; // index of '.'
    while end > 0 && bytes[end - 1].is_ascii_whitespace() {
        end -= 1;
    }
    let mut start = end;
    while start > 0 && is_ident_byte(bytes[start - 1]) {
        start -= 1;
    }
    if start == end {
        return None;
    }
    Some((&trimmed[start..end], leek_span::offset(start)))
}

fn is_ident_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

fn member_items(
    run: &leek_pipeline::Run<'_>,
    root: &SyntaxNode,
    receiver: &str,
    receiver_start: u32,
) -> Option<Vec<lsp::CompletionItem>> {
    // 1) Class.Field — every entry under `Class.` in
    //    FINAL_BUILTIN_FIELDS.
    let prefix = format!("{receiver}.");
    let mut items: Vec<lsp::CompletionItem> = FINAL_BUILTIN_FIELDS
        .iter()
        .filter_map(|path| path.strip_prefix(&prefix))
        .map(|name| lsp::CompletionItem {
            label: name.into(),
            kind: Some(lsp::CompletionItemKind::CONSTANT),
            detail: Some(format!("{receiver}.{name}")),
            ..Default::default()
        })
        .collect();

    // 1b) Receiver is a typed variable — look up its
    //     `ClassInstance(N)` via the type table and list class N's
    //     members. Handles the common `var c = new Cat(); c.<here>`
    //     case that v0.2's completion missed.
    if let Some(art) = run.get::<leek_types::pipeline::TypeCheckArtifact>()
        && let Some(entry) = art.table.smallest_at(receiver_start)
            && let Some(class_name) = class_name_of_type(&entry.ty)
                && class_name != receiver
                    && let Some(cls_node) = find_class_decl_by_name(root, &class_name) {
                        push_class_members(&cls_node, &mut items);
                    }

    // 2) `this.` — list members of the enclosing ClassDecl.
    if receiver == "this"
        && let Some(cls) = enclosing_class(root) {
            push_class_members(&cls, &mut items);
        }

    // 3) Receiver names a user-declared class → list its members.
    // Use the CST directly: in salsa mode the type-check step does not
    // leave a ResolveArtifact in the run context.
    if let Some(cls_node) = find_class_decl_by_name(root, receiver) {
        push_class_members(&cls_node, &mut items);
    }

    if items.is_empty() { None } else { Some(items) }
}

/// Extract every `ClassField` / `ClassMethod` / `ClassConstructor`
/// under `cls_node`'s `ClassBody` and append as completion items.
fn push_class_members(cls_node: &SyntaxNode, items: &mut Vec<lsp::CompletionItem>) {
    let Some(body) = cls_node
        .children()
        .find(|c| c.kind() == SyntaxKind::ClassBody)
    else {
        return;
    };
    for member in body.children() {
        match member.kind() {
            SyntaxKind::ClassField | SyntaxKind::ClassMethod | SyntaxKind::ClassConstructor => {
                if let Some(label) = member_label(&member) {
                    items.push(lsp::CompletionItem {
                        label,
                        kind: Some(member_kind(member.kind())),
                        detail: signature_for(&member),
                        ..Default::default()
                    });
                }
            }
            _ => {}
        }
    }
}

/// Walk through `Nullable`/`Array<T>` wrappers to a base
/// `ClassInstance(name)`. Mirrors the helper in `type_definition.rs`.
fn class_name_of_type(ty: &Type) -> Option<String> {
    match ty {
        Type::ClassInstance(n, _) => Some(n.clone()),
        Type::Nullable(inner) => class_name_of_type(inner),
        Type::Array(inner) => class_name_of_type(inner),
        _ => None,
    }
}

fn enclosing_class(root: &SyntaxNode) -> Option<SyntaxNode> {
    // The first ClassDecl whose range encloses … hmm, we don't have
    // the cursor here. Caller is in member mode AFTER `this.`, which
    // is only valid inside a class. Pick the smallest ClassDecl that
    // covers the cursor position — but we don't have it. Simplest:
    // return the LAST ClassDecl we see (matches the common case of
    // one class per file). Refine later if needed.
    root.descendants()
        .filter(|n| n.kind() == SyntaxKind::ClassDecl)
        .last()
}

fn find_class_decl_by_name(root: &SyntaxNode, name: &str) -> Option<SyntaxNode> {
    root.descendants().find(|n| {
        n.kind() == SyntaxKind::ClassDecl
            && n.children_with_tokens()
                .filter_map(leek_syntax::language::NodeOrToken::into_token)
                .find(|t| t.kind() == SyntaxKind::Ident)
                .is_some_and(|id| id.text() == name)
    })
}

fn member_label(member: &SyntaxNode) -> Option<String> {
    if member.kind() == SyntaxKind::ClassConstructor {
        return Some("constructor".into());
    }
    member
        .children_with_tokens()
        .filter_map(leek_syntax::language::NodeOrToken::into_token)
        .find(|t| t.kind() == SyntaxKind::Ident)
        .map(|t| t.text().to_string())
}

fn member_kind(k: SyntaxKind) -> lsp::CompletionItemKind {
    match k {
        SyntaxKind::ClassField => lsp::CompletionItemKind::FIELD,
        SyntaxKind::ClassMethod => lsp::CompletionItemKind::METHOD,
        SyntaxKind::ClassConstructor => lsp::CompletionItemKind::CONSTRUCTOR,
        _ => lsp::CompletionItemKind::PROPERTY,
    }
}

// ─── global completion ──────────────────────────────────────────────

fn global_items(
    run: &leek_pipeline::Run<'_>,
    root: &SyntaxNode,
    uri: &lsp::Url,
) -> Vec<lsp::CompletionItem> {
    let mut items: Vec<lsp::CompletionItem> = Vec::new();

    // 1. User symbols with their rendered signatures. The doc-comment
    //    above the declaration is deferred to `resolve`; we only stash
    //    a `data` pointer to its declaration here.
    if let Some(art) = run.get::<leek_resolver::pipeline::ResolveArtifact>() {
        for sym in &art.table.symbols {
            let detail = decl_signature_for_symbol(root, sym)
                .unwrap_or_else(|| symbol_kind_label(sym.kind).into());
            items.push(lsp::CompletionItem {
                label: sym.name.clone(),
                kind: Some(symbol_kind_to_lsp(sym.kind)),
                detail: Some(detail),
                // Stash the *declaration node's* start (the
                // `function`/`class`/`var` keyword), which is what
                // `doc_comment_before` needs in `resolve` — the symbol
                // span sits mid-line and would find no comment above.
                data: resolve_data(uri, &sym.name, "user", decl_start_for_symbol(root, sym)),
                ..Default::default()
            });
        }
    }

    // 2. Builtin functions — arity-tracked entries get a detail
    //    string with their signature; everything in BUILTINS not
    //    already covered is added as a plain function entry. We
    //    track names already emitted so the user symbols above
    //    don't get a duplicate item from the builtin pass.
    let mut seen: std::collections::HashSet<String> =
        items.iter().map(|it| it.label.clone()).collect();
    for b in BUILTIN_FNS {
        if !seen.insert(b.name.to_string()) {
            continue;
        }
        let detail = format_builtin_detail(b);
        items.push(lsp::CompletionItem {
            label: b.name.into(),
            kind: Some(lsp::CompletionItemKind::FUNCTION),
            detail: Some(detail),
            data: resolve_data(uri, b.name, "builtin", None),
            ..Default::default()
        });
    }
    for name in BUILTINS {
        if !seen.insert((*name).to_string()) {
            continue;
        }
        items.push(lsp::CompletionItem {
            label: (*name).into(),
            kind: Some(lsp::CompletionItemKind::FUNCTION),
            detail: Some("builtin".into()),
            data: resolve_data(uri, name, "builtin", None),
            ..Default::default()
        });
    }

    // 3. Builtin constants.
    for name in BUILTIN_CONSTANTS {
        if !seen.insert((*name).to_string()) {
            continue;
        }
        items.push(lsp::CompletionItem {
            label: (*name).into(),
            kind: Some(lsp::CompletionItemKind::CONSTANT),
            detail: Some("builtin constant".into()),
            ..Default::default()
        });
    }

    // 4. Host-environment library functions + constants (registered from a
    //    loaded library like `leekwars`, e.g. `getCell`, `CELL_EMPTY`).
    for (name, lo, hi, _v) in leek_resolver::builtins::dynamic_builtin_functions() {
        if !seen.insert(name.clone()) {
            continue;
        }
        let detail = if lo == hi {
            format!("library {name}({lo} args)")
        } else {
            format!("library {name}({lo}-{hi} args)")
        };
        let data = resolve_data(uri, &name, "builtin", None);
        items.push(lsp::CompletionItem {
            label: name,
            kind: Some(lsp::CompletionItemKind::FUNCTION),
            detail: Some(detail),
            data,
            ..Default::default()
        });
    }
    for name in leek_resolver::builtins::dynamic_builtin_constants() {
        if !seen.insert(name.clone()) {
            continue;
        }
        items.push(lsp::CompletionItem {
            label: name,
            kind: Some(lsp::CompletionItemKind::CONSTANT),
            detail: Some("library constant".into()),
            ..Default::default()
        });
    }

    // 4. Keywords.
    for kw in KEYWORDS {
        items.push(lsp::CompletionItem {
            label: (*kw).into(),
            kind: Some(lsp::CompletionItemKind::KEYWORD),
            ..Default::default()
        });
    }

    // 5. Snippets.
    for (label, body) in SNIPPETS {
        items.push(lsp::CompletionItem {
            label: (*label).into(),
            kind: Some(lsp::CompletionItemKind::SNIPPET),
            insert_text: Some((*body).into()),
            insert_text_format: Some(lsp::InsertTextFormat::SNIPPET),
            ..Default::default()
        });
    }

    items
}

/// Build the `data` payload `resolve` reads back. Cheap to serialize;
/// the actual documentation lookup it enables is what we defer.
fn resolve_data(
    uri: &lsp::Url,
    name: &str,
    kind: &str,
    def_start: Option<u32>,
) -> Option<serde_json::Value> {
    serde_json::to_value(ResolveData {
        uri: uri.to_string(),
        name: name.to_string(),
        kind: kind.to_string(),
        def_start,
    })
    .ok()
}

fn decl_signature_for_symbol(root: &SyntaxNode, sym: &leek_resolver::Symbol) -> Option<String> {
    // Find the smallest CST node that covers the symbol's full
    // span, then walk up to the enclosing declaration node (same
    // dance the hover handler does).
    let span = sym.full_span;
    let node = node_covering(root, span.start, span.end)?;
    let decl = enclosing_decl(&node)?;
    signature_for(&decl)
}

/// Byte offset of the enclosing declaration node for `sym` — the start
/// of its `function`/`class`/`var` keyword. `resolve` feeds this to
/// `doc_comment_before` to recover the symbol's doc-comment lazily.
fn decl_start_for_symbol(root: &SyntaxNode, sym: &leek_resolver::Symbol) -> Option<u32> {
    let span = sym.full_span;
    let node = node_covering(root, span.start, span.end)?;
    let decl = enclosing_decl(&node)?;
    Some(u32::from(decl.text_range().start()))
}

fn node_covering(root: &SyntaxNode, start: u32, end: u32) -> Option<SyntaxNode> {
    fn covers(n: &SyntaxNode, start: u32, end: u32) -> bool {
        let r = n.text_range();
        u32::from(r.start()) <= start && end <= u32::from(r.end())
    }
    if !covers(root, start, end) {
        return None;
    }
    let mut current = root.clone();
    loop {
        let next = current.children().find(|c| covers(c, start, end));
        match next {
            Some(n) => current = n,
            None => return Some(current),
        }
    }
}

fn enclosing_decl(n: &SyntaxNode) -> Option<SyntaxNode> {
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

fn format_builtin_detail(b: &leek_resolver::builtins::BuiltinFn) -> String {
    if b.min_args == b.max_args {
        let args = (0..b.min_args)
            .map(|i| format!("arg{}", i + 1))
            .collect::<Vec<_>>()
            .join(", ");
        format!("builtin {}({})", b.name, args)
    } else {
        format!(
            "builtin {}(...) — {}-{} args, v{}+",
            b.name, b.min_args, b.max_args, b.min_version,
        )
    }
}

fn symbol_kind_label(k: SymbolKind) -> &'static str {
    match k {
        SymbolKind::Global => "global",
        SymbolKind::Local => "local",
        SymbolKind::Function => "function",
        SymbolKind::Class => "class",
        SymbolKind::Param => "param",
        SymbolKind::Field => "field",
        SymbolKind::Builtin => "builtin",
    }
}

fn symbol_kind_to_lsp(k: SymbolKind) -> lsp::CompletionItemKind {
    match k {
        SymbolKind::Global | SymbolKind::Local => lsp::CompletionItemKind::VARIABLE,
        SymbolKind::Function => lsp::CompletionItemKind::FUNCTION,
        SymbolKind::Class => lsp::CompletionItemKind::CLASS,
        SymbolKind::Param => lsp::CompletionItemKind::VARIABLE,
        SymbolKind::Field => lsp::CompletionItemKind::FIELD,
        SymbolKind::Builtin => lsp::CompletionItemKind::FUNCTION,
    }
}

const KEYWORDS: &[&str] = &[
    "var",
    "global",
    "function",
    "class",
    "extends",
    "constructor",
    "static",
    "private",
    "public",
    "protected",
    "include",
    "new",
    "if",
    "else",
    "while",
    "do",
    "for",
    "in",
    "break",
    "continue",
    "return",
    "switch",
    "case",
    "default",
    "and",
    "or",
    "not",
    "is",
    "instanceof",
    "xor",
    "true",
    "false",
    "null",
    "this",
    "super",
    "integer",
    "real",
    "boolean",
    "string",
    "any",
    "void",
];

const SNIPPETS: &[(&str, &str)] = &[
    ("ifsnip", "if ($1) {\n\t$0\n}"),
    ("ifelse", "if ($1) {\n\t$2\n} else {\n\t$0\n}"),
    ("forsnip", "for (var $1 = 0; $1 < $2; $1++) {\n\t$0\n}"),
    ("foreach", "for (var $1 in $2) {\n\t$0\n}"),
    ("whilesnip", "while ($1) {\n\t$0\n}"),
    ("funsnip", "function $1($2) {\n\t$0\n}"),
    ("classsnip", "class $1 {\n\t$0\n}"),
];

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workspace::Workspace;
    use tower_lsp::lsp_types as lsp;

    fn ws_with(src: &str) -> (Workspace, lsp::Url) {
        let mut ws = Workspace::default();
        let uri = lsp::Url::parse("file:///t.leek").unwrap();
        ws.open(uri.clone(), src.to_string());
        (ws, uri)
    }

    fn pos(l: u32, c: u32) -> lsp::Position {
        lsp::Position {
            line: l,
            character: c,
        }
    }

    fn array_items(resp: lsp::CompletionResponse) -> Vec<lsp::CompletionItem> {
        match resp {
            lsp::CompletionResponse::Array(v) => v,
            lsp::CompletionResponse::List(l) => l.items,
        }
    }

    #[test]
    fn includes_builtin_functions() {
        let src = "var x = 1\n";
        let (ws, uri) = ws_with(src);
        let resp = handle(&ws, &uri, pos(0, 9)).unwrap();
        let items = array_items(resp);
        assert!(
            items.iter().any(|i| i.label == "sqrt"),
            "missing sqrt builtin",
        );
        assert!(
            items.iter().any(|i| i.label == "push"),
            "missing push builtin",
        );
    }

    #[test]
    fn user_function_detail_renders_signature() {
        let src = "function add(integer a, integer b) -> integer { return a + b }\n";
        let (ws, uri) = ws_with(src);
        let resp = handle(&ws, &uri, pos(1, 0)).unwrap();
        let items = array_items(resp);
        let add = items
            .iter()
            .find(|i| i.label == "add")
            .expect("user fn in completion");
        let detail = add.detail.as_deref().unwrap_or("");
        assert!(detail.contains("function add"), "detail = {detail:?}");
        assert!(detail.contains("integer a"), "detail = {detail:?}");
    }

    #[test]
    fn this_dot_lists_class_members() {
        let src = concat!(
            "class Cat {\n",
            "    integer age\n",
            "    string name\n",
            "    function meow() {\n",
            "        this.\n",
            "    }\n",
            "}\n",
        );
        let (ws, uri) = ws_with(src);
        // Cursor right after `this.` on line 4. The source line is
        // "        this." — 8 spaces of indent + "this.".
        let col = "        this.".len() as u32;
        let resp = handle(&ws, &uri, pos(4, col)).unwrap();
        let items = array_items(resp);
        let labels: Vec<&str> = items.iter().map(|i| i.label.as_str()).collect();
        assert!(labels.contains(&"age"), "labels: {labels:?}");
        assert!(labels.contains(&"name"), "labels: {labels:?}");
        assert!(labels.contains(&"meow"), "labels: {labels:?}");
    }

    #[test]
    fn class_name_dot_lists_static_builtin_fields() {
        let src = "var v = Integer.\n";
        let (ws, uri) = ws_with(src);
        let col = "var v = Integer.".len() as u32;
        let resp = handle(&ws, &uri, pos(0, col)).unwrap();
        let items = array_items(resp);
        let labels: Vec<&str> = items.iter().map(|i| i.label.as_str()).collect();
        assert!(labels.contains(&"MAX_VALUE"), "labels: {labels:?}");
        assert!(labels.contains(&"MIN_VALUE"), "labels: {labels:?}");
    }

    #[test]
    fn member_completion_for_user_class() {
        let src = concat!(
            "class Dog {\n",
            "    integer age\n",
            "    function bark() { return 0 }\n",
            "}\n",
            "var d = Dog\n",
        );
        // Append a `var x = Dog.` line and complete after the dot.
        let src2 = format!("{src}var x = Dog.\n");
        let (ws2, uri2) = ws_with(&src2);
        let line = src2.lines().count() as u32 - 1; // line containing `Dog.`
        let resp = handle(&ws2, &uri2, pos(line, "var x = Dog.".len() as u32)).unwrap();
        let items = array_items(resp);
        let labels: Vec<&str> = items.iter().map(|i| i.label.as_str()).collect();
        assert!(labels.contains(&"age"), "labels: {labels:?}");
        assert!(labels.contains(&"bark"), "labels: {labels:?}");
    }

    #[test]
    fn includes_builtin_constants() {
        let src = "var x = 1\n";
        let (ws, uri) = ws_with(src);
        let resp = handle(&ws, &uri, pos(0, 9)).unwrap();
        let items = array_items(resp);
        let labels: Vec<&str> = items.iter().map(|i| i.label.as_str()).collect();
        assert!(labels.contains(&"PI"), "labels missing PI: {labels:?}");
        assert!(labels.contains(&"INFINITY"), "labels missing INFINITY");
    }

    #[test]
    fn no_dot_means_global_completion() {
        let src = "var x = 1\n";
        let (ws, uri) = ws_with(src);
        let resp = handle(&ws, &uri, pos(0, 9)).unwrap();
        let items = array_items(resp);
        // Keywords should be present in global mode.
        assert!(items.iter().any(|i| i.label == "if"));
        assert!(items.iter().any(|i| i.label == "function"));
    }

    #[test]
    fn handle_defers_documentation_to_resolve() {
        // The eager pass attaches a `data` pointer but no documentation.
        let src = "function add(integer a, integer b) -> integer { return a + b }\n";
        let (ws, uri) = ws_with(src);
        let items = array_items(handle(&ws, &uri, pos(1, 0)).unwrap());
        let add = items.iter().find(|i| i.label == "add").expect("add item");
        assert!(add.documentation.is_none(), "docs must be deferred");
        assert!(add.data.is_some(), "must carry a resolve pointer");
    }

    #[test]
    fn resolve_attaches_user_doc_comment() {
        let src = "// Adds two integers.\nfunction add(integer a, integer b) -> integer { return a + b }\n";
        let (ws, uri) = ws_with(src);
        let items = array_items(handle(&ws, &uri, pos(2, 0)).unwrap());
        let add = items.iter().find(|i| i.label == "add").cloned().unwrap();
        let resolved = resolve(&ws, add);
        let lsp::Documentation::MarkupContent(m) = resolved.documentation.expect("docs") else {
            panic!("expected markup documentation");
        };
        assert!(
            m.value.contains("Adds two integers."),
            "resolved doc = {:?}",
            m.value
        );
    }

    #[test]
    fn resolve_attaches_builtin_signature() {
        let src = "var x = 1\n";
        let (ws, uri) = ws_with(src);
        let items = array_items(handle(&ws, &uri, pos(0, 9)).unwrap());
        let count = items
            .iter()
            .find(|i| i.label == "count")
            .cloned()
            .expect("count builtin");
        assert!(count.documentation.is_none());
        let resolved = resolve(&ws, count);
        let lsp::Documentation::MarkupContent(m) = resolved.documentation.expect("docs") else {
            panic!("expected markup documentation");
        };
        assert!(
            m.value.contains("function count("),
            "resolved builtin doc = {:?}",
            m.value
        );
    }

    #[test]
    fn resolve_is_noop_for_dataless_item() {
        let (ws, _uri) = ws_with("var x = 1\n");
        let kw = lsp::CompletionItem {
            label: "if".into(),
            kind: Some(lsp::CompletionItemKind::KEYWORD),
            ..Default::default()
        };
        let resolved = resolve(&ws, kw);
        assert!(resolved.documentation.is_none());
    }
}
