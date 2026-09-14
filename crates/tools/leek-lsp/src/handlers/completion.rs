//! `textDocument/completion` — identifier + keyword + builtin +
//! snippet completion, plus a member-completion mode triggered by
//! `.`.
//!
//! ## Sources
//!
//! 1. **User symbols** discovered by the resolver — every declared
//!    function, class, global, local, param *whose binding is live
//!    at the cursor* (see [`symbol_in_scope_at`]). Each item's
//!    `detail` is the full one-line signature (function header,
//!    class `extends` clause, typed `var`) when we can render one
//!    from the CST; otherwise the inferred type's name.
//! 2. **Top-level declarations from the rest of the program** — the
//!    files this one shares a flat namespace with, via
//!    [`program_scope`]. Each such item keeps the uri and offsets of
//!    the file that *declares* it, so `completionItem/resolve` reads
//!    the right doc-comment.
//! 3. **Builtin functions** from [`BUILTIN_FNS`] (typed signature
//!    from the embedded library headers when there is one) plus
//!    every name in [`BUILTINS`] not already covered by the arity
//!    table.
//! 4. **Builtin constants** from [`BUILTIN_CONSTANTS`].
//! 5. **Keywords** + a small **snippet** set.
//!
//! ## Member mode
//!
//! When the cursor is positioned right after a `.` token we switch
//! to member completion:
//!
//! - `this.` — list the fields, methods and constructor of the class
//!   the cursor is *inside*, plus everything it inherits.
//! - `Integer.` / `Real.` / `String.` / `Array.` / `Map.` / `Set.`
//!   — list every entry of [`FINAL_BUILTIN_FIELDS`] under that
//!   prefix.
//! - a receiver the type table says is a `ClassInstance` (`var c =
//!   new Cat(); c.` …) — we take the class name from its inferred
//!   type, find that class's declaration in the CST, and list its
//!   members as for `this.`.
//! - a receiver that *names* a declared class — likewise, from the
//!   CST alone.
//!
//! Receiver-typed member completion (e.g. `myCat.` where `myCat`
//! is `ClassInstance("Cat")`) would need to consult the type
//! table; we resolve that path for declared classes via the
//! resolver's symbol table here too. A class named by any file in
//! the program is found, and its `extends` chain is walked.

use leek_resolver::SymbolKind;
use leek_resolver::builtins::{BUILTIN_CONSTANTS, BUILTIN_FNS, BUILTINS, FINAL_BUILTIN_FIELDS};
use leek_syntax::{SyntaxKind, SyntaxNode, is_ident_continue};
use tower_lsp::lsp_types as lsp;

use super::member::{class_name_of_type, class_parent_name_of, find_class_decl_by_name};
use super::program_scope::{ScopeFile, program_scope};
use crate::handlers::{enclosing_class_name, is_top_level_decl, symbol_in_scope_at};
use crate::workspace::Workspace;
use leek_ide::signature::signature_for;

/// Everything the item builders need about one completion request: the
/// analysed home file, the cursor, and the program the file belongs to.
///
/// Bundled because both builders need all of it and threading seven
/// parameters through each of them reads badly.
struct Ctx<'a, 'db> {
    ws: &'a Workspace,
    /// The file the cursor is in — the *home* file of the program.
    uri: &'a lsp::Url,
    run: &'a leek_pipeline::Run<'db>,
    /// CST of the home file.
    root: &'a SyntaxNode,
    /// Every file the home file shares a flat namespace with, home
    /// file included.
    scope: &'a [ScopeFile],
    /// Cursor position, as a byte offset into the home file.
    offset: u32,
    /// Language version the home file is analysed under; gates the
    /// builtins we offer.
    version: leek_syntax::Version,
}

pub fn handle(
    ws: &Workspace,
    uri: &lsp::Url,
    pos: lsp::Position,
) -> Option<lsp::CompletionResponse> {
    let doc = ws.doc(uri)?;
    let offset = doc.pos_map().to_offset(pos)?;

    let run = crate::pipeline::run(ws, uri, leek_session::Target::TypeChecked)?;
    let green = &run.get::<leek_parser::pipeline::GreenTreeArtifact>()?.0;
    let root = SyntaxNode::new_root(green.clone());

    // Computed once per request and shared by both modes: the scope
    // walk re-parses the workspace to read include edges, and
    // completion runs on every keystroke (leekwars#155 tracks making
    // that cheaper).
    let scope = program_scope(ws, uri);
    let cx = Ctx {
        ws,
        uri,
        run: &run,
        root: &root,
        scope: &scope,
        offset,
        version: doc.source_file_version(&ws.db),
    };

    // Member mode: did the user just type `.`?
    if let Some((receiver, receiver_start)) = member_receiver(&doc.text, offset)
        && let Some(items) = member_items(&cx, receiver, receiver_start)
    {
        return Some(lsp::CompletionResponse::Array(items));
    }
    // Fall through to global suggestions if we can't resolve
    // the receiver — better than offering nothing.

    Some(lsp::CompletionResponse::Array(global_items(&cx)))
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
///
/// `data.uri` is the *declaring* file, which for a cross-file item is
/// not the file the user is typing in and need not be open — hence the
/// fall back to the indexed text.
fn user_documentation(ws: &Workspace, data: &ResolveData) -> Option<String> {
    let uri = lsp::Url::parse(&data.uri).ok()?;
    let start = data.def_start?;
    let targets = ws.analysis_targets();
    let text: &str = match ws.doc(&uri) {
        Some(doc) => &doc.text,
        None => targets.iter().find(|t| *t.uri == uri)?.text,
    };
    let comment = leek_ide::doc::doc_comment_before(text, start)?;
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
    if off == 0 {
        return None;
    }
    // `get` rather than `&text[..off]`: a client can send a position that
    // lands mid-character, and slicing there would panic and take down the
    // whole completion request.
    let before = text.get(..off)?;

    // Strip the partial member name the user has already typed.
    let partial_len = trailing_ident_len(before);
    let trimmed = before[..before.len() - partial_len].trim_end();
    if !trimmed.ends_with('.') {
        return None;
    }

    // Back over the `.`, then over any space between it and the receiver.
    let end = trimmed.len() - '.'.len_utf8();
    let end = end - trailing_len(&trimmed[..end], char::is_whitespace);
    let start = end - trailing_ident_len(&trimmed[..end]);
    if start == end {
        return None;
    }
    Some((&trimmed[start..end], leek_span::offset(start)))
}

/// Byte length of the run of identifier characters ending `s`.
///
/// Uses the lexer's own predicate, so the receiver the LSP resolves is the
/// one the compiler would tokenize. An ASCII-only test used to live here, and
/// it truncated every Latin-1 identifier: `créature.` resolved against
/// `ature`, and no member ever completed.
fn trailing_ident_len(s: &str) -> usize {
    trailing_len(s, is_ident_continue)
}

/// Byte length of the trailing run of chars satisfying `pred`. Summing
/// `len_utf8` keeps the result on a character boundary, which a byte-wise
/// scan does not.
fn trailing_len(s: &str, pred: impl Fn(char) -> bool) -> usize {
    s.chars()
        .rev()
        .take_while(|c| pred(*c))
        .map(char::len_utf8)
        .sum()
}

fn member_items(
    cx: &Ctx<'_, '_>,
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
    let mut named_a_class = false;
    if let Some(art) = cx.run.get::<leek_types::pipeline::TypeCheckArtifact>()
        && let Some(entry) = art.table.smallest_at(receiver_start)
        && let Some(class_name) = class_name_of_type(&entry.ty)
        && class_name != receiver
    {
        named_a_class = push_class_chain(cx, &class_name, &mut items);
    }

    // 2) `this.` — members of the class the cursor is *inside*. Using
    //    the cursor matters: a file with two classes used to complete
    //    the last one's members from inside the first.
    if receiver == "this"
        && let Some(class_name) = enclosing_class_name(cx.root, cx.offset)
    {
        named_a_class |= push_class_chain(cx, &class_name, &mut items);
    }

    // 3) Receiver names a user-declared class → list its members.
    // Use the CST directly: in salsa mode the type-check step does not
    // leave a ResolveArtifact in the run context. Skipped once the
    // receiver's type already named a class, so an ordinary variable
    // doesn't cost a program-wide search for a class of its name on
    // every keystroke after a `.`.
    if !named_a_class {
        push_class_chain(cx, receiver, &mut items);
    }

    if items.is_empty() { None } else { Some(items) }
}

/// Push `class_name`'s members, then each ancestor's, following
/// `extends` across the whole program. Returns whether `class_name`
/// itself named a declared class.
///
/// Members already in `items` are skipped, so an override hides the
/// inherited copy and a class reached by two routes is listed once.
fn push_class_chain(
    cx: &Ctx<'_, '_>,
    class_name: &str,
    items: &mut Vec<lsp::CompletionItem>,
) -> bool {
    let mut current = class_name.to_string();
    // False for `class_name` itself, true for every ancestor above it —
    // and so also "the head of the chain was found".
    let mut inherited = false;
    // Same cap `member::find_member_in_chain` uses, so a cyclic
    // `extends` terminates instead of hanging the editor.
    for _ in 0..64 {
        let Some(cls) = find_class_decl_in_program(cx, &current) else {
            break;
        };
        push_class_members(&cls, inherited.then_some(current.as_str()), items);
        inherited = true;
        match class_parent_name_of(&cls) {
            Some(parent) => current = parent,
            None => break,
        }
    }
    inherited
}

/// Find `class <name>` in the home file, or failing that in any other
/// file of the program — `#400` expands includes in the HIR lowerer,
/// not the CST, so an included class is simply absent from this file's
/// green tree and has to be looked up in its own.
fn find_class_decl_in_program(cx: &Ctx<'_, '_>, name: &str) -> Option<SyntaxNode> {
    if let Some(cls) = find_class_decl_by_name(cx.root, name) {
        return Some(cls);
    }
    for file in cx.scope {
        if file.uri == *cx.uri {
            continue;
        }
        let Some(root) = file_root(cx.ws, file) else {
            continue;
        };
        if let Some(cls) = find_class_decl_by_name(&root, name) {
            return Some(cls);
        }
    }
    None
}

/// Parse one program-scope file and hand back an *owned* root, so the
/// node outlives the `Run` that produced its green tree.
fn file_root(ws: &Workspace, file: &ScopeFile) -> Option<SyntaxNode> {
    let run = crate::pipeline::run_on_file(ws, file.source_file, leek_session::Target::Parsed)?;
    let green = run.get::<leek_parser::pipeline::GreenTreeArtifact>()?;
    Some(SyntaxNode::new_root(green.0.clone()))
}

/// Extract every `ClassField` / `ClassMethod` / `ClassConstructor`
/// under `cls_node`'s `ClassBody` and append as completion items.
///
/// `inherited_from` names the subclass's ancestor when `cls_node` was
/// reached by walking `extends`; it is shown in the item's `detail`
/// (rather than `labelDetails`, which needs a client capability the
/// server does not yet record). A `(label, kind)` pair already present
/// is left alone, which is what makes an override win over the member
/// it overrides.
fn push_class_members(
    cls_node: &SyntaxNode,
    inherited_from: Option<&str>,
    items: &mut Vec<lsp::CompletionItem>,
) {
    let Some(body) = cls_node
        .children()
        .find(|c| c.kind() == SyntaxKind::ClassBody)
    else {
        return;
    };
    for member in body.children() {
        match member.kind() {
            SyntaxKind::ClassField | SyntaxKind::ClassMethod | SyntaxKind::ClassConstructor => {
                let Some(label) = member_label(&member) else {
                    continue;
                };
                let kind = member_kind(member.kind());
                if items
                    .iter()
                    .any(|it| it.label == label && it.kind == Some(kind))
                {
                    continue;
                }
                let detail = match (signature_for(&member), inherited_from) {
                    (Some(sig), Some(base)) => Some(format!("{sig} — from {base}")),
                    (Some(sig), None) => Some(sig),
                    (None, Some(base)) => Some(format!("from {base}")),
                    (None, None) => None,
                };
                items.push(lsp::CompletionItem {
                    label,
                    kind: Some(kind),
                    detail,
                    ..Default::default()
                });
            }
            _ => {}
        }
    }
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

fn global_items(cx: &Ctx<'_, '_>) -> Vec<lsp::CompletionItem> {
    let mut items: Vec<lsp::CompletionItem> = Vec::new();

    // 1. User symbols with their rendered signatures, restricted to the
    //    ones whose binding is live at the cursor — offering another
    //    function's local is worse than offering nothing. The
    //    doc-comment above the declaration is deferred to `resolve`; we
    //    only stash a `data` pointer to its declaration here.
    if let Some(art) = cx.run.get::<leek_resolver::pipeline::ResolveArtifact>() {
        for sym in &art.table.symbols {
            if !symbol_in_scope_at(cx.root, sym, cx.offset) {
                continue;
            }
            let detail = decl_signature_for_symbol(cx.root, sym)
                .unwrap_or_else(|| symbol_kind_label(sym.kind).into());
            items.push(lsp::CompletionItem {
                label: sym.name.clone(),
                kind: Some(symbol_kind_to_lsp(sym.kind)),
                detail: Some(detail),
                // Stash the *declaration node's* start (the
                // `function`/`class`/`var` keyword), which is what
                // `doc_comment_before` needs in `resolve` — the symbol
                // span sits mid-line and would find no comment above.
                data: resolve_data(
                    cx.uri,
                    &sym.name,
                    "user",
                    decl_start_for_symbol(cx.root, sym),
                ),
                ..Default::default()
            });
        }
    }

    // We track the names already emitted so later passes never add a
    // duplicate item. Seeding it from this file's own symbols also
    // means a local shadow wins over a same-named cross-file symbol.
    let mut seen: std::collections::HashSet<String> =
        items.iter().map(|it| it.label.clone()).collect();

    // 2. Top-level declarations from the other files of this program.
    //    Leekscript's namespace is flat per program, so an `include`d
    //    file's functions, classes and top-level variables are usable
    //    here by their bare names.
    for file in cx.scope {
        if file.uri == *cx.uri {
            continue;
        }
        push_cross_file_items(cx.ws, file, &mut seen, &mut items);
    }

    // 3. Builtin functions — arity-tracked entries get a detail
    //    string with their signature; everything in BUILTINS not
    //    already covered is added as a plain function entry.
    for b in BUILTIN_FNS {
        // A builtin the file's language version predates cannot be
        // called at all (`FUNCTION_NOT_AVAILABLE`). Claim the name so
        // the untyped `BUILTINS` pass below doesn't re-offer it.
        if b.min_version > cx.version as u8 {
            seen.insert(b.name.to_string());
            continue;
        }
        if !seen.insert(b.name.to_string()) {
            continue;
        }
        let detail = format_builtin_detail(b);
        items.push(lsp::CompletionItem {
            label: b.name.into(),
            kind: Some(lsp::CompletionItemKind::FUNCTION),
            detail: Some(detail),
            data: resolve_data(cx.uri, b.name, "builtin", None),
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
            detail: Some(library_detail(name).unwrap_or_else(|| "builtin".into())),
            data: resolve_data(cx.uri, name, "builtin", None),
            ..Default::default()
        });
    }

    // 4. Builtin constants.
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

    // 5. Host-environment library functions + constants (registered from a
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
        let data = resolve_data(cx.uri, &name, "builtin", None);
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

    // 6. Keywords, and 7. snippets. The snippets are labelled after the
    //    keyword they expand, so each pair shares a label; the
    //    `sort_text` suffix keeps the bare keyword above its snippet
    //    without disturbing the alphabetical order of everything else.
    for kw in KEYWORDS {
        items.push(lsp::CompletionItem {
            label: (*kw).into(),
            kind: Some(lsp::CompletionItemKind::KEYWORD),
            sort_text: Some(format!("{kw}0")),
            ..Default::default()
        });
    }
    for (label, body) in SNIPPETS {
        items.push(lsp::CompletionItem {
            label: (*label).into(),
            kind: Some(lsp::CompletionItemKind::SNIPPET),
            sort_text: Some(format!("{label}1")),
            insert_text: Some((*body).into()),
            insert_text_format: Some(lsp::InsertTextFormat::SNIPPET),
            ..Default::default()
        });
    }

    items
}

/// Append the top-level declarations of one *other* file of the program.
///
/// Every item keeps `file`'s uri and `file`'s byte offsets, never the
/// home file's: `resolve` re-opens the declaring document to read the
/// doc-comment, and offsets from a merged table would silently land on
/// an unrelated declaration in the home buffer.
fn push_cross_file_items(
    ws: &Workspace,
    file: &ScopeFile,
    seen: &mut std::collections::HashSet<String>,
    items: &mut Vec<lsp::CompletionItem>,
) {
    let Some(run) =
        crate::pipeline::run_on_file(ws, file.source_file, leek_session::Target::Resolved)
    else {
        return;
    };
    let Some(art) = run.get::<leek_resolver::pipeline::ResolveArtifact>() else {
        return;
    };
    let Some(green) = run.get::<leek_parser::pipeline::GreenTreeArtifact>() else {
        return;
    };
    let root = SyntaxNode::new_root(green.0.clone());

    for sym in &art.table.symbols {
        // A top-level `var` is a `Local` to the resolver but still
        // lands in the shared file scope, so it crosses an include
        // just like a `global` does. Params and fields never do.
        if !matches!(
            sym.kind,
            SymbolKind::Function | SymbolKind::Class | SymbolKind::Global | SymbolKind::Local
        ) {
            continue;
        }
        if !is_top_level_decl(&root, sym.def_span.start) {
            continue;
        }
        if !seen.insert(sym.name.clone()) {
            continue;
        }
        let detail = decl_signature_for_symbol(&root, sym)
            .unwrap_or_else(|| symbol_kind_label(sym.kind).into());
        items.push(lsp::CompletionItem {
            label: sym.name.clone(),
            kind: Some(symbol_kind_to_lsp(sym.kind)),
            detail: Some(detail),
            data: resolve_data(
                &file.uri,
                &sym.name,
                "user",
                decl_start_for_symbol(&root, sym),
            ),
            ..Default::default()
        });
    }
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

/// One-line `detail` for a library name, from the embedded signature
/// headers — the same source `builtin_documentation` reads. The first
/// overload's signature, with a count of the rest. `None` for a name the
/// headers don't cover. (Unifying the two builtin metadata sources
/// outright is leekwars#162.)
fn library_detail(name: &str) -> Option<String> {
    let sigs = leek_ide::library_sigs::library_signatures(name)?;
    let first = sigs.first()?;
    Some(match sigs.len() {
        1 => first.signature.clone(),
        n => format!("{} (+{} overloads)", first.signature, n - 1),
    })
}

/// One-line `detail` for an arity-tracked builtin: the typed signature
/// when the library headers have one, else the arity table.
fn format_builtin_detail(b: &leek_resolver::builtins::BuiltinFn) -> String {
    if let Some(detail) = library_detail(b.name) {
        return detail;
    }
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

/// Snippet bodies, labelled after the keyword each one expands. The
/// duplicate label a keyword item already carries is deliberate — see
/// the `sort_text` note in [`global_items`].
const SNIPPETS: &[(&str, &str)] = &[
    ("if", "if ($1) {\n\t$0\n}"),
    ("ifelse", "if ($1) {\n\t$2\n} else {\n\t$0\n}"),
    ("for", "for (var $1 = 0; $1 < $2; $1++) {\n\t$0\n}"),
    ("foreach", "for (var $1 in $2) {\n\t$0\n}"),
    ("while", "while ($1) {\n\t$0\n}"),
    ("function", "function $1($2) {\n\t$0\n}"),
    ("class", "class $1 {\n\t$0\n}"),
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

    /// The receiver scan must agree with the lexer, which accepts the
    /// Latin-1 letters. An ASCII-only scan stopped at the `é` and handed the
    /// type lookup `ature` instead of `créature`, so no member completed.
    #[test]
    fn latin_1_receiver_is_not_truncated() {
        let text = "var créature = []\ncréature.";
        let off = u32::try_from(text.len()).unwrap();
        assert_eq!(
            member_receiver(text, off).map(|(name, _)| name),
            Some("créature")
        );
    }

    /// The partial member name the user is mid-way through typing is Latin-1
    /// aware too, on both sides of the dot.
    #[test]
    fn latin_1_partial_member_is_stripped_whole() {
        let text = "créature.éveil";
        let off = u32::try_from(text.len()).unwrap();
        let (name, start) = member_receiver(text, off).unwrap();
        assert_eq!(name, "créature");
        assert_eq!(start, 0);
    }

    /// A position that lands inside a multi-byte character must return `None`
    /// rather than panic and take the whole request down with it.
    #[test]
    fn an_offset_off_a_char_boundary_is_rejected() {
        let text = "créature.";
        assert_eq!(member_receiver(text, 4), None);
    }

    #[test]
    fn space_around_the_dot_still_finds_the_receiver() {
        let text = "créature . ";
        let off = u32::try_from(text.len()).unwrap();
        assert_eq!(
            member_receiver(text, off).map(|(name, _)| name),
            Some("créature")
        );
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
        let col = u32::try_from("        this.".len()).unwrap();
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
        let col = u32::try_from("var v = Integer.".len()).unwrap();
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
        let line = u32::try_from(src2.lines().count()).unwrap() - 1; // line containing `Dog.`
        let resp = handle(
            &ws2,
            &uri2,
            pos(line, u32::try_from("var x = Dog.".len()).unwrap()),
        )
        .unwrap();
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
        // Language-level constants from upstream `LeekConstants.java`
        // (the COLOR_* family was once missing here — regression guard).
        assert!(labels.contains(&"COLOR_RED"), "labels missing COLOR_RED");
        assert!(
            labels.contains(&"COLOR_GREEN"),
            "labels missing COLOR_GREEN"
        );
        assert!(labels.contains(&"COLOR_BLUE"), "labels missing COLOR_BLUE");
        assert!(
            labels.contains(&"OPERATIONS_LIMIT"),
            "labels missing OPERATIONS_LIMIT"
        );
        assert!(labels.contains(&"TYPE_MAP"), "labels missing TYPE_MAP");
        // `SORT_RANDOM` is NOT a real LeekWars constant (upstream's
        // random sort mode is internal to `shuffle`) — keep it out.
        assert!(
            !labels.contains(&"SORT_RANDOM"),
            "SORT_RANDOM is not a real constant and must not be offered"
        );
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
