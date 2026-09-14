//! Shared cursor → symbol resolution helpers used by the position-based
//! navigation handlers (definition, references, documentHighlight,
//! typeDefinition, implementation, prepareCallHierarchy).
//!
//! The common pattern is "what symbol is under the cursor?": if the
//! cursor sits on a *reference*, follow it to its target symbol;
//! otherwise, if the cursor sits on a *declaration*, use that symbol.

use leek_resolver::{ResolveTable, Symbol, SymbolId, SymbolKind};
use leek_syntax::{SyntaxKind, SyntaxNode, SyntaxToken, language::NodeOrToken};
use tower_lsp::lsp_types as lsp;

use crate::workspace::Workspace;

/// Resolve the symbol id at byte `offset`.
///
/// - Cursor on a reference → that reference's `target` id (taken
///   directly, so it resolves even if the symbol slot is somehow
///   absent).
/// - Otherwise, cursor on a declaration → that symbol's `id`.
/// - `None` when the offset matches neither.
pub(crate) fn resolve_symbol_id(table: &ResolveTable, offset: u32) -> Option<SymbolId> {
    if let Some(r) = table.reference_at(offset) {
        Some(r.target)
    } else {
        Some(
            table
                .symbols
                .iter()
                .find(|s| s.def_span.start <= offset && offset < s.def_span.end)?
                .id,
        )
    }
}

/// Resolve the whole [`Symbol`] at byte `offset`.
///
/// - Cursor on a reference → the symbol its `target` points at
///   (`None` if that slot is missing).
/// - Otherwise, cursor on a declaration → that symbol.
/// - `None` when the offset matches neither.
pub(crate) fn resolve_symbol(table: &ResolveTable, offset: u32) -> Option<&Symbol> {
    if let Some(r) = table.reference_at(offset) {
        table.symbol(r.target)
    } else {
        table
            .symbols
            .iter()
            .find(|s| s.def_span.start <= offset && offset < s.def_span.end)
    }
}

/// One occurrence of a symbol somewhere in the workspace, with its LSP
/// range and its start byte offset (the latter lets call-hierarchy test
/// which function body a call site falls inside).
pub(crate) struct Occurrence {
    pub uri: lsp::Url,
    pub range: lsp::Range,
    pub start: u32,
    pub is_declaration: bool,
}

/// True for the symbol kinds that live in Leekscript's single flat
/// global namespace and can therefore be referenced from *other* files
/// via `include(...)`. Locals, params and fields are file/scope-local,
/// so their references never cross a file boundary — those stay
/// single-file (and renaming them workspace-wide would be wrong).
pub(crate) fn is_workspace_global(kind: SymbolKind) -> bool {
    matches!(
        kind,
        SymbolKind::Global | SymbolKind::Function | SymbolKind::Class
    )
}

/// Find every occurrence (declaration + references) of the top-level
/// symbol named `name` of `kind`, scoped to the *program* the file at
/// `home_uri` belongs to — see [`program_scope`](super::program_scope).
///
/// The scope is computed semantically from the include graph, so an
/// independent AI that happens to reuse `name` is excluded while every
/// file that shares a program with `home_uri` (including a shared
/// library and all of its includers) is covered.
///
/// Within a program, classification is exact: the LSP resolves each
/// file in isolation, so a cross-file reference does not bind to the
/// declaration's symbol. We bridge that with a CST identifier scan — a
/// `name`-matching `Ident` token is an occurrence unless the file's own
/// resolver bound it to a *different* symbol (a local/param/field
/// shadow or a class member) or it is a member access (`obj.name`).
/// Leekscript's per-program flat namespace makes a top-level `name`
/// unique within the scope, so an otherwise-unbound `name` token can
/// only mean that declaration.
pub(crate) fn workspace_occurrences(
    ws: &Workspace,
    home_uri: &lsp::Url,
    name: &str,
    kind: SymbolKind,
) -> Vec<Occurrence> {
    let mut out: Vec<Occurrence> = Vec::new();
    for file in crate::handlers::program_scope::program_scope(ws, home_uri) {
        out.append(&mut occurrences_in_file(
            ws,
            &file.uri,
            file.source_file,
            name,
            kind,
        ));
    }
    out
}

/// Occurrences of the top-level symbol `name`/`kind` within a *single*
/// file. The per-file half of [`workspace_occurrences`], also used by
/// `documentHighlight` (which is document-local by definition but must
/// still recognise a cross-file symbol's use sites in this file).
///
/// Classification per `name`-matching `Ident` token: a token the file
/// resolved counts only if it targets a matching top-level symbol
/// (excludes local/param/field shadows and class members); a
/// declaration token counts only if it's the top-level declaration; an
/// unbound token is a cross-file reference unless it's a member access.
pub(crate) fn occurrences_in_file(
    ws: &Workspace,
    uri: &lsp::Url,
    source_file: leek_pipeline::salsa::SourceFile,
    name: &str,
    kind: SymbolKind,
) -> Vec<Occurrence> {
    let mut out: Vec<Occurrence> = Vec::new();
    let Some(run) = crate::pipeline::run_on_file(ws, source_file, leek_session::Target::Resolved)
    else {
        return out;
    };
    let Some(art) = run.get::<leek_resolver::pipeline::ResolveArtifact>() else {
        return out;
    };
    let table = &art.table;
    let Some(green) = run.get::<leek_parser::pipeline::GreenTreeArtifact>() else {
        return out;
    };
    let root = SyntaxNode::new_root(green.0.clone());
    let text = source_file.text(&ws.db);
    let line_table = leek_span::LineTable::new(text);
    let pm = crate::util::position::PosMap::new(&line_table, text);

    for tok in root
        .descendants_with_tokens()
        .filter_map(NodeOrToken::into_token)
    {
        if tok.kind() != SyntaxKind::Ident || tok.text() != name {
            continue;
        }
        let start = u32::from(tok.text_range().start());
        let end = u32::from(tok.text_range().end());
        let range = lsp::Range {
            start: pm.to_position(start),
            end: pm.to_position(end),
        };

        if let Some(r) = table.reference_at(start) {
            if symbol_is_global_match(table, &root, r.target, name, kind) {
                out.push(Occurrence {
                    uri: uri.clone(),
                    range,
                    start,
                    is_declaration: false,
                });
            }
            continue;
        }
        if let Some(sym) = table.symbols.iter().find(|s| s.def_span.start == start) {
            if sym.kind == kind && sym.name == name && !offset_in_class(&root, start) {
                out.push(Occurrence {
                    uri: uri.clone(),
                    range,
                    start,
                    is_declaration: true,
                });
            }
            continue;
        }
        if preceded_by_dot(&tok) {
            continue;
        }
        out.push(Occurrence {
            uri: uri.clone(),
            range,
            start,
            is_declaration: false,
        });
    }
    out
}

/// Whether resolved reference `id` targets the top-level symbol we're
/// after. The target is declared in the file currently being scanned,
/// so `root` is that file's CST — we use it to reject class members
/// (whose `container` the resolver leaves unset, so a structural CST
/// check is the reliable signal) and local/param/field shadows.
fn symbol_is_global_match(
    table: &ResolveTable,
    root: &SyntaxNode,
    id: SymbolId,
    name: &str,
    kind: SymbolKind,
) -> bool {
    table.symbol(id).is_some_and(|s| {
        s.kind == kind && s.name == name && !offset_in_class(root, s.def_span.start)
    })
}

/// Whether the node covering `offset` has a `ClassDecl` ancestor — i.e.
/// the declaration at that offset is a class member, not a top-level
/// (file-scope) declaration.
fn offset_in_class(root: &SyntaxNode, offset: u32) -> bool {
    enclosing_class_decl(root, offset).is_some()
}

/// The name of the class whose body encloses `offset`, if any. Used to
/// fill `container_name` for class members in workspace-symbol results
/// (the resolver records methods/fields with no `container`, so the
/// class name is recovered structurally from the CST).
pub(crate) fn enclosing_class_name(root: &SyntaxNode, offset: u32) -> Option<String> {
    let class = enclosing_class_decl(root, offset)?;
    // `class Name ...` — the class name is the first `Ident` token among
    // the decl's direct children (the `class` keyword isn't an `Ident`).
    class
        .children_with_tokens()
        .filter_map(NodeOrToken::into_token)
        .find(|t| t.kind() == SyntaxKind::Ident)
        .map(|t| t.text().to_string())
}

/// The nearest `ClassDecl` ancestor of the smallest node covering
/// `offset`, if any.
fn enclosing_class_decl(root: &SyntaxNode, offset: u32) -> Option<SyntaxNode> {
    let mut cur = Some(smallest_node_at(root, offset));
    while let Some(n) = cur {
        if n.kind() == SyntaxKind::ClassDecl {
            return Some(n);
        }
        cur = n.parent();
    }
    None
}

/// Whether `sym` is declared *directly* in a class body — a field or a
/// method — as opposed to a local or parameter declared inside a
/// method, which is an ordinary scoped binding.
///
/// The resolver never fills [`Symbol::container`] (it declares methods
/// as plain [`SymbolKind::Function`] and fields as
/// [`SymbolKind::Field`] into the pushed class scope), so there is no
/// semantic "is a member" signal and the answer has to come from the
/// CST.
///
/// Used by the rename family to refuse member renames: member accesses
/// (`this.x`, `obj.m()`) are never recorded as references, so any
/// occurrence search over a member name is wrong in both directions —
/// it misses every dotted use site and hits same-named top-level
/// declarations. See leekwars#46; the sound fix needs the member-ref
/// table from the resolver rework.
pub(crate) fn symbol_is_class_member(root: &SyntaxNode, sym: &Symbol) -> bool {
    // `Field` is only ever declared for a `ClassField`, so it needs no
    // CST evidence — and stays right on a half-parsed buffer.
    if sym.kind == SymbolKind::Field {
        return true;
    }
    class_member_decl_at(root, sym.def_span.start).is_some()
}

/// The `ClassField` / `ClassMethod` / `ClassConstructor` (or, on a
/// half-parsed buffer, `ClassBody` / `ClassDecl`) reached first when
/// walking up from `offset`.
///
/// The walk stops at a `Block` or a `ParamList`, which mean the offset
/// is a method-local or a method parameter: those are scoped bindings
/// that rename correctly today and must not be refused.
fn class_member_decl_at(root: &SyntaxNode, offset: u32) -> Option<SyntaxNode> {
    let mut cur = Some(smallest_node_at(root, offset));
    while let Some(n) = cur {
        match n.kind() {
            SyntaxKind::ClassField
            | SyntaxKind::ClassMethod
            | SyntaxKind::ClassConstructor
            | SyntaxKind::ClassBody
            | SyntaxKind::ClassDecl => return Some(n),
            // Inside a method body or parameter list — a local or a
            // param, not a member.
            SyntaxKind::Block | SyntaxKind::ParamList => return None,
            _ => cur = n.parent(),
        }
    }
    None
}

/// The smallest node covering `offset`, found by descending from
/// `root`. Returns `root` itself when no child covers the offset.
fn smallest_node_at(root: &SyntaxNode, offset: u32) -> SyntaxNode {
    let mut node = root.clone();
    loop {
        let next = node.children().find(|c| {
            let r = c.text_range();
            u32::from(r.start()) <= offset && offset < u32::from(r.end())
        });
        match next {
            Some(child) => node = child,
            None => return node,
        }
    }
}

/// The identifier name at byte `offset`, if the cursor sits on a plain
/// `Ident` token that is not the member half of a `recv.member` access.
/// Used by cross-file navigation to recover the name to look up when the
/// current file couldn't resolve it (an `include`d top-level symbol).
pub(crate) fn ident_name_at(root: &SyntaxNode, offset: u32) -> Option<String> {
    let tok = root
        .descendants_with_tokens()
        .filter_map(NodeOrToken::into_token)
        .find(|t| {
            let r = t.text_range();
            t.kind() == SyntaxKind::Ident
                && u32::from(r.start()) <= offset
                && offset < u32::from(r.end())
        })?;
    if preceded_by_dot(&tok) {
        return None;
    }
    Some(tok.text().to_string())
}

/// The byte range `(start, end)` of the `Ident` token at `offset`, if
/// any. Lets a cross-file hover highlight the identifier under the
/// cursor even though the symbol it names lives in another file.
pub(crate) fn ident_range_at(root: &SyntaxNode, offset: u32) -> Option<(u32, u32)> {
    root.descendants_with_tokens()
        .filter_map(NodeOrToken::into_token)
        .find(|t| {
            let r = t.text_range();
            t.kind() == SyntaxKind::Ident
                && u32::from(r.start()) <= offset
                && offset < u32::from(r.end())
        })
        .map(|t| {
            let r = t.text_range();
            (u32::from(r.start()), u32::from(r.end()))
        })
}

/// Find the top-level declaration of `name` somewhere in the program
/// that the file at `home_uri` belongs to. Returns the declaring file
/// plus its [`Symbol`]. Used for cross-file go-to-definition and hover:
/// when a file references a function/class/global defined in something
/// it `include`s, the reference doesn't resolve locally, so we search
/// the program scope for the declaration. There is at most one in a
/// valid program (a second would be a redeclaration error).
pub(crate) fn find_top_level_decl(
    ws: &Workspace,
    home_uri: &lsp::Url,
    name: &str,
) -> Option<(crate::handlers::program_scope::ScopeFile, Symbol)> {
    for file in crate::handlers::program_scope::program_scope(ws, home_uri) {
        let Some(run) =
            crate::pipeline::run_on_file(ws, file.source_file, leek_session::Target::Resolved)
        else {
            continue;
        };
        let Some(art) = run.get::<leek_resolver::pipeline::ResolveArtifact>() else {
            continue;
        };
        let Some(green) = run.get::<leek_parser::pipeline::GreenTreeArtifact>() else {
            continue;
        };
        let root = SyntaxNode::new_root(green.0.clone());
        if let Some(sym) = art.table.symbols.iter().find(|s| {
            s.name == name
                && is_workspace_global(s.kind)
                && !offset_in_class(&root, s.def_span.start)
        }) {
            return Some((file, sym.clone()));
        }
    }
    None
}

/// A cross-file use site resolved to its declaration: the identifier
/// under the cursor names a top-level symbol declared in an `include`d
/// file. Carries what references/rename need (the declaration's home
/// file + the symbol's name/kind, used to anchor the program-wide
/// search) plus the use-site identifier's byte range (for
/// `prepareRename` to highlight).
pub(crate) struct CrossFileUse {
    /// The file the symbol is *declared* in — the anchor for the
    /// reference search, so it spans every includer, not just the
    /// use-site's own program.
    pub home_uri: lsp::Url,
    pub name: String,
    pub kind: SymbolKind,
    pub use_start: u32,
    pub use_end: u32,
}

/// Resolve the identifier at `offset` in `root` (the file at `uri`) to a
/// top-level declaration elsewhere in its program. `None` when the
/// cursor isn't on a plain identifier, it's a member access, or no
/// matching top-level declaration exists in scope.
pub(crate) fn cross_file_use_target(
    ws: &Workspace,
    uri: &lsp::Url,
    root: &SyntaxNode,
    offset: u32,
) -> Option<CrossFileUse> {
    let name = ident_name_at(root, offset)?;
    let (use_start, use_end) = ident_range_at(root, offset)?;
    let (file, sym) = find_top_level_decl(ws, uri, &name)?;
    Some(CrossFileUse {
        home_uri: file.uri,
        name: sym.name,
        kind: sym.kind,
        use_start,
        use_end,
    })
}

/// Whether the token's nearest preceding non-trivia token is `.` — i.e.
/// the identifier is the member half of a `receiver.member` access.
pub(crate) fn preceded_by_dot(tok: &SyntaxToken) -> bool {
    let mut prev = tok.prev_token();
    while let Some(t) = prev {
        if t.kind().is_trivia() {
            prev = t.prev_token();
            continue;
        }
        return t.kind() == SyntaxKind::Dot;
    }
    false
}

// ─── lexical scope of a declaration ─────────────────────────────────

/// The syntax node kinds that open a new resolver scope. Mirrors the
/// `push_scope` / `push_function_scope` sites in `leek-resolver`
/// (`resolve_fn_body`, `resolve_class`, `resolve_class_method_body`,
/// `resolve_class_constructor_body`, `resolve_block_body`,
/// `resolve_for`, `resolve_foreach`, and the lambda arm of
/// `resolve_expr`), so a declaration's lexical reach on the CST matches
/// the one the resolver gives it.
fn opens_a_scope(kind: SyntaxKind) -> bool {
    matches!(
        kind,
        SyntaxKind::FnDecl
            | SyntaxKind::ClassDecl
            | SyntaxKind::ClassMethod
            | SyntaxKind::ClassConstructor
            | SyntaxKind::LambdaExpr
            | SyntaxKind::Block
            | SyntaxKind::ForStmt
            | SyntaxKind::ForeachStmt
    )
}

/// The node whose scope a declaration starting at `decl_offset` binds
/// into: the innermost scope-opening ancestor, or `root` for a
/// file-scope declaration.
///
/// A parameter's declaration sits in the header rather than the body,
/// so the walk lands on the `FnDecl` / `ClassMethod` — which spans the
/// body too, exactly the reach `push_function_scope` gives it.
fn scope_node_of(root: &SyntaxNode, decl_offset: u32) -> SyntaxNode {
    let mut cur = Some(smallest_node_at(root, decl_offset));
    while let Some(n) = cur {
        if opens_a_scope(n.kind()) {
            return n;
        }
        cur = n.parent();
    }
    root.clone()
}

/// Whether `sym`'s binding is live at byte `cursor`.
///
/// Globals, functions and classes live in Leekscript's flat namespace
/// and are visible everywhere. Everything else — locals, parameters,
/// class fields — is visible only inside the node that opened its
/// scope. Containment is the whole test on purpose: the resolver's
/// `lookup_id` walks every enclosing scope outward without stopping at
/// a function boundary, so a stricter rule (say, "declared before the
/// cursor") would hide names that do resolve today.
pub(crate) fn symbol_in_scope_at(root: &SyntaxNode, sym: &Symbol, cursor: u32) -> bool {
    if is_workspace_global(sym.kind) || sym.kind == SymbolKind::Builtin {
        return true;
    }
    let scope = scope_node_of(root, sym.def_span.start);
    let range = scope.text_range();
    let (lo, hi) = (u32::from(range.start()), u32::from(range.end()));
    if lo > cursor {
        return false;
    }
    // Half-open, except at file scope: completing on the blank line
    // after the last statement puts the cursor exactly on the root's
    // end offset, and a top-level binding is certainly live there.
    if scope == *root {
        cursor <= hi
    } else {
        cursor < hi
    }
}

/// Whether the declaration covering `decl_offset` sits directly at file
/// scope — not inside a function body, a class, a loop or a block.
///
/// Used to decide which of another file's symbols this file can see: an
/// `include`d file contributes its top-level declarations to the shared
/// flat namespace and nothing else.
pub(crate) fn is_top_level_decl(root: &SyntaxNode, decl_offset: u32) -> bool {
    let node = smallest_node_at(root, decl_offset);
    let Some(decl) = std::iter::successors(Some(node), SyntaxNode::parent).find(|n| {
        matches!(
            n.kind(),
            SyntaxKind::FnDecl
                | SyntaxKind::ClassDecl
                | SyntaxKind::ClassMethod
                | SyntaxKind::ClassConstructor
                | SyntaxKind::ClassField
                | SyntaxKind::VarDeclStmt
                | SyntaxKind::Param
        )
    }) else {
        return false;
    };
    // The declaration node itself opens a scope for its *contents*; what
    // decides file-scope-ness is everything above it.
    std::iter::successors(decl.parent(), SyntaxNode::parent).all(|n| !opens_a_scope(n.kind()))
}
