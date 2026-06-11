//! `textDocument/hover` — show the inferred type and (when on a
//! declaration) the full signature plus any leading doc-comment.

use leek_complexity::analyze_file;
use leek_hir::pipeline::HirArtifact;
use leek_span::Span;
use leek_syntax::{SyntaxKind, SyntaxNode};
use leek_types::Type;
use tower_lsp::lsp_types as lsp;

use super::member::{
    enclosing_class_of, enclosing_decl_node, field_access_at, find_member_in_chain,
    initializer_type, member_decl_name, node_covering,
};
use crate::util::position::{position_to_offset, span_to_range};
use crate::workspace::Workspace;
use leek_ide::doc::{directives_enabled, doc_and_directives_before, doc_comment_before};
use leek_ide::signature::signature_for_with;
use leek_types::InferredSignatures;

pub fn handle(ws: &Workspace, uri: &lsp::Url, pos: lsp::Position) -> Option<lsp::Hover> {
    let doc = ws.doc(uri)?;
    let offset = position_to_offset(doc.pos_map(), pos)?;

    let run = crate::pipeline::run(ws, uri, leek_recipes::Target::Hir)?;

    let resolve_art = run.get::<leek_resolver::pipeline::ResolveArtifact>();
    let type_art = run.get::<leek_types::pipeline::TypeCheckArtifact>()?;
    let green = &run.get::<leek_parser::pipeline::GreenTreeArtifact>()?.0;
    let root = SyntaxNode::new_root(green.clone());

    // Two information sources, in priority order:
    //   1. A symbol whose declaration node covers the cursor — gives
    //      us the full signature (params, return, modifiers) and the
    //      preceding doc-comment.
    //   2. A type-table entry for an expression under the cursor —
    //      gives the inferred type.
    // The two can combine (cursor on a name reference: resolve to a
    // symbol AND show the inferred type of the reference site).

    let mut sections: Vec<String> = Vec::new();
    let mut span_for_range: Option<Span> = None;

    // Resolve the cursor to a symbol — try references first
    // (`foo()` calls `foo`), then fall back to declarations
    // (cursor right on the declared name).
    let (symbol, ref_span) = locate_symbol(resolve_art, offset);

    // When the cursor sits on a `this` / `super` keyword, the symbol we
    // resolved to is the *class* (or its parent), but the expression is
    // an *instance* of it. Render an instance-style line so hover never
    // claims `this` IS the class declaration.
    let self_kw = ref_span.and_then(|s| {
        // `get` (not direct indexing) so a stale/out-of-bounds or non-UTF-8
        // boundary span can't panic and crash the server.
        match doc.text.get(s.start as usize..s.end as usize) {
            Some("this") => Some("this"),
            Some("super") => Some("super"),
            _ => None,
        }
    });

    if let (Some(sym), Some(kw)) = (symbol.as_ref(), self_kw) {
        // `this` / `super` are *instances*; their type is the class name
        // itself (`Cat`), matching how every other instance type renders.
        // The note keeps the `this` vs `super` distinction.
        sections.push(format!("```leekscript\n{}\n```", sym.name));
        sections.push(format!("*`{kw}` — instance of `{}`*", sym.name));
        span_for_range = ref_span;
    } else if let Some(sym) = symbol.as_ref() {
        // Find the declaration's CST node — first the node that
        // exactly covers `full_span`, then walk up to the enclosing
        // FnDecl / ClassDecl / VarDeclStmt so signature/doc lookups
        // see the whole declaration. (For Function symbols, the
        // resolver sets `full_span = def_span = the ident token`;
        // the enclosing FnDecl is the parent we need.)
        let decl_node = node_covering(&root, sym.full_span).and_then(|n| enclosing_decl_node(&n));
        let decl_start = decl_node
            .as_ref()
            .map_or(sym.full_span.start, |n| u32::from(n.text_range().start()));
        // Library builtins and leek-wars game functions resolve to a
        // `Builtin` symbol with no user-source declaration node, so
        // `signature_for` has nothing to render. Pull their typed
        // signature(s) + doc from the embedded `.leek` headers instead.
        let library = if sym.kind == leek_resolver::SymbolKind::Builtin {
            builtin_signature_section(&sym.name)
        } else {
            None
        };
        if let Some((code, lib_doc)) = library {
            sections.push(code);
            if let Some(d) = lib_doc {
                sections.push(d);
            }
        } else {
            if let Some(decl_node) = &decl_node {
                if let Some(sig) =
                    signature_for_with(decl_node, &|n| infer_decl_type(&type_art.signatures, n))
                {
                    sections.push(format!("```leekscript\n{sig}\n```"));
                } else {
                    sections.push(format!(
                        "```leekscript\n{} {}\n```",
                        symbol_kind_label(sym.kind),
                        sym.name,
                    ));
                }
            } else {
                sections.push(format!(
                    "```leekscript\n{} {}\n```",
                    symbol_kind_label(sym.kind),
                    sym.name,
                ));
            }
            // Append the doc-comment that sits above the declaration. In a
            // signature file (signature-mode), `@<backend>-backend:`
            // directives are pulled into their own section; in normal code
            // they're inert and stay as plain prose.
            append_doc_sections(&doc.text, decl_start, &mut sections);
        }
        // Surface the symbol's *type* using the same vocabulary as every
        // other type: a function value is `Function<A, B => C>`, and a
        // class reference (the class itself, not an instance) is
        // `Class<Name>`. (Instances and `this` render as the bare class
        // name via `format_type`.)
        // A *top-level* function's decl node is a `FnDecl`; a method
        // shares the `Function` symbol kind but its decl node is a
        // `ClassMethod`. Only top-level functions draw from the
        // bare-name-keyed signature + complexity maps, so a method must
        // not borrow a same-named function's value type or complexity.
        let is_top_level_fn = sym.kind == leek_resolver::SymbolKind::Function
            && decl_node
                .as_ref()
                .is_some_and(|n| n.kind() == SyntaxKind::FnDecl);
        if is_top_level_fn {
            if let Some(t) = function_type_string(&type_art.signatures, &sym.name) {
                sections.push(format!("*type:* `{t}`"));
            }
        } else if sym.kind == leek_resolver::SymbolKind::Class {
            // A class reference (the class itself, not an instance) has
            // type `Class<Name>`.
            sections.push(format!("*type:* `Class<{}>`", sym.name));
        }
        // For top-level functions, append a complexity row computed from
        // the lowered HIR. Constant-time functions get a trivial line
        // but it's still useful next to a multi-line body.
        if is_top_level_fn
            && let Some(complexity_md) = complexity_section(run.get::<HirArtifact>(), &sym.name)
        {
            sections.push(complexity_md);
        }
        span_for_range = ref_span.or(Some(sym.def_span));
    }

    // Member access (`recv.method()` / `Class.field`) doesn't resolve
    // to a symbol, but we can still reach the member's declaration by
    // looking up the receiver's class in the type table and walking the
    // inheritance chain. Only attempt this when nothing else matched.
    let mut member_resolved = false;
    if sections.is_empty()
        && let Some((sig, span)) = member_access_hover(
            &root,
            resolve_art,
            &type_art.table,
            &type_art.signatures,
            offset,
        )
    {
        sections.push(format!("```leekscript\n{sig}\n```"));
        span_for_range = Some(span);
        member_resolved = true;
    }

    // Builtin / leek-wars function names (`count(a)`, `getLife()`) never
    // resolve to an in-table symbol — the resolver knows them ambiently,
    // so `record_ref` no-ops and `locate_symbol` finds nothing. Detect the
    // name directly from the CST and render its typed signature from the
    // embedded `.leek` headers. Only when nothing else matched, so a
    // user `function count(...)` still wins.
    if sections.is_empty()
        && let Some((name, span)) = builtin_name_at(&root, offset)
        && let Some((code, lib_doc)) = builtin_signature_section(&name)
    {
        sections.push(code);
        if let Some(d) = lib_doc {
            sections.push(d);
        }
        span_for_range = Some(span);
        member_resolved = true;
    }

    // Cross-file: an unresolved identifier may name a top-level
    // function / class / global declared in an `include`d file. The
    // LSP resolves each file alone, so such a reference binds to
    // nothing locally — search the program scope and render the
    // declaration's signature + doc from its home file. Only when
    // nothing local matched, so a same-file result always wins.
    if sections.is_empty()
        && let Some(name) = crate::handlers::ident_name_at(&root, offset)
        && let Some((file, sym)) = crate::handlers::find_top_level_decl(ws, uri, &name)
    {
        let mut xfile = cross_file_sections(ws, &file, &sym);
        if !xfile.is_empty() {
            if let Some(fname) = file.uri.path_segments().and_then(std::iter::Iterator::last) {
                sections.push(format!("*defined in `{fname}`*"));
            }
            sections.append(&mut xfile);
            span_for_range = crate::handlers::ident_range_at(&root, offset)
                .map(|(s, e)| Span::new(doc.source_file_source_id(&ws.db), s, e));
            // Suppress the inferred-type row below — the reference site's
            // local type is meaningless for a symbol defined elsewhere.
            member_resolved = true;
        }
    }

    // Always also surface the inferred type — useful both as a
    // standalone hover for expressions (no resolved symbol) and as
    // extra context next to a signature ("here you're using the
    // function as a value, type is `function`").
    let inferred = if member_resolved {
        None
    } else {
        type_art.table.smallest_at(offset)
    }
    .or_else(|| {
        // Cursor on a var-decl name doesn't sit on a typed
        // expression. Fall back to the type of the binding's own
        // initializer (declarator-aware, so a multi-declaration
        // never borrows a sibling's init type).
        symbol
            .as_ref()
            .and_then(|sym| initializer_type(&root, &type_art.table, sym.def_span.start))
    });
    if let Some(entry) = inferred {
        let ty = format_type(&entry.ty);
        // Skip the redundant `any` annotation when we already have
        // a richer signature — most untyped locals' inferred type
        // is `any` and the noise would dwarf the useful info. Also
        // skip for class-name references: the `class X` signature is
        // complete, and the surrounding-expression type is either
        // redundant (a `new X()` instance) or spurious (a body expr
        // picked up by the var-decl init fallback).
        let is_class_ref = matches!(
            symbol.as_ref().map(|s| s.kind),
            Some(leek_resolver::SymbolKind::Class)
        );
        let suppress = symbol.is_some() && (matches!(entry.ty, Type::Any) || is_class_ref);
        if symbol.is_some() {
            if !suppress && !sections.first().is_some_and(|s| s.contains(&ty)) {
                sections.push(format!("*type:* `{ty}`"));
            }
        } else {
            sections.push(format!("```leekscript\n{ty}\n```"));
        }
        if span_for_range.is_none() {
            span_for_range = Some(entry.span);
        }
    }

    if sections.is_empty() {
        return None;
    }
    let value = sections.join("\n\n---\n\n");
    Some(lsp::Hover {
        contents: lsp::HoverContents::Markup(lsp::MarkupContent {
            kind: lsp::MarkupKind::Markdown,
            value,
        }),
        range: span_for_range.map(|s| span_to_range(doc.pos_map(), s)),
    })
}

/// Render hover sections (signature, doc-comment, value-type,
/// complexity) for a top-level symbol declared in another file. Runs
/// the pipeline on that file so the signature draws on *its* inferred
/// types and its own doc-comment — the same content the symbol's
/// home-file hover would show. Mirrors the resolved-symbol branch of
/// [`handle`], scoped to a single declaration.
fn cross_file_sections(
    ws: &Workspace,
    file: &crate::handlers::program_scope::ScopeFile,
    sym: &leek_resolver::Symbol,
) -> Vec<String> {
    let Some(run) = crate::pipeline::run_on_file(ws, file.source_file, leek_recipes::Target::Hir)
    else {
        return Vec::new();
    };
    let Some(type_art) = run.get::<leek_types::pipeline::TypeCheckArtifact>() else {
        return Vec::new();
    };
    let Some(green) = run.get::<leek_parser::pipeline::GreenTreeArtifact>() else {
        return Vec::new();
    };
    let root = SyntaxNode::new_root(green.0.clone());
    let text = file.source_file.text(&ws.db);

    let mut sections: Vec<String> = Vec::new();
    let decl_node = node_covering(&root, sym.full_span).and_then(|n| enclosing_decl_node(&n));
    let decl_start = decl_node
        .as_ref()
        .map_or(sym.full_span.start, |n| u32::from(n.text_range().start()));
    let sig = decl_node
        .as_ref()
        .and_then(|n| signature_for_with(n, &|m| infer_decl_type(&type_art.signatures, m)));
    match sig {
        Some(s) => sections.push(format!("```leekscript\n{s}\n```")),
        None => sections.push(format!(
            "```leekscript\n{} {}\n```",
            symbol_kind_label(sym.kind),
            sym.name
        )),
    }
    append_doc_sections(text, decl_start, &mut sections);

    let is_top_level_fn = sym.kind == leek_resolver::SymbolKind::Function
        && decl_node
            .as_ref()
            .is_some_and(|n| n.kind() == SyntaxKind::FnDecl);
    if is_top_level_fn {
        if let Some(t) = function_type_string(&type_art.signatures, &sym.name) {
            sections.push(format!("*type:* `{t}`"));
        }
        if let Some(c) = complexity_section(run.get::<HirArtifact>(), &sym.name) {
            sections.push(c);
        }
    } else if sym.kind == leek_resolver::SymbolKind::Class {
        sections.push(format!("*type:* `Class<{}>`", sym.name));
    }
    sections
}

/// Look up a symbol under the cursor. Returns `(symbol, ref_span)`
/// — ref_span is `Some` when the cursor was on a *use*, `None`
/// when it was on the declaration itself.
fn locate_symbol(
    resolve_art: Option<&leek_resolver::pipeline::ResolveArtifact>,
    offset: u32,
) -> (Option<leek_resolver::Symbol>, Option<Span>) {
    let Some(art) = resolve_art else {
        return (None, None);
    };
    if let Some(r) = art.table.reference_at(offset) {
        let ref_span = Span::new(
            // SourceId isn't needed for hover (single-file); reuse
            // the resolver's own source-id discipline by faking
            // a 1.
            leek_span::SourceId::new(1).unwrap(),
            r.name_offset,
            r.name_offset + r.name_len,
        );
        return (art.table.symbol(r.target).cloned(), Some(ref_span));
    }
    // Cursor on a declaration: pick the symbol whose def_span
    // covers the offset.
    let sym = art
        .table
        .symbols
        .iter()
        .find(|s| s.def_span.start <= offset && offset < s.def_span.end)
        .cloned();
    (sym, None)
}

fn format_type(ty: &Type) -> String {
    match ty {
        Type::Any => "any".into(),
        Type::Null => "null".into(),
        Type::Void => "void".into(),
        Type::Boolean => "boolean".into(),
        Type::Integer => "integer".into(),
        Type::Real => "real".into(),
        Type::String => "string".into(),
        Type::Array(t) => format!("Array<{}>", format_type(t)),
        Type::Map(k, v) => format!("Map<{}, {}>", format_type(k), format_type(v)),
        Type::Set(t) => format!("Set<{}>", format_type(t)),
        Type::Object => "Object".into(),
        // Surface class instances as `ClassName` rather than the
        // generic `class` so navigation and tooltips line up. Bound
        // generic arguments render as `Box<integer>`.
        Type::ClassInstance(n, args) if !args.is_empty() => {
            let inner: Vec<String> = args.iter().map(format_type).collect();
            format!("{n}<{}>", inner.join(", "))
        }
        Type::ClassInstance(n, _) => n.clone(),
        Type::Function => "function".into(),
        Type::FunctionWithReturn { params, ret } => {
            let ps: Vec<String> = params.iter().map(format_type).collect();
            format!("Function<{} => {}>", ps.join(", "), format_type(ret))
        }
        Type::Interval => "Interval".into(),
        Type::Nullable(t) => match t.as_ref() {
            // `A | B | null` reads better than `A | B?`, where the
            // `?` visually binds to the last member only.
            Type::Union(_) => format!("{} | null", format_type(t)),
            _ => format!("{}?", format_type(t)),
        },
        Type::Union(members) => {
            let inner: Vec<String> = members.iter().map(format_type).collect();
            inner.join(" | ")
        }
        Type::Tuple(members) => {
            let inner: Vec<String> = members.iter().map(format_type).collect();
            format!("Array[{}]", inner.join(", "))
        }
    }
}

/// Append the doc-comment (and, in signature-mode, the
/// `@<backend>-backend:` directives) that sit above a declaration at
/// `decl_start`. In normal code the directives are inert and stay as
/// plain prose.
fn append_doc_sections(text: &str, decl_start: u32, sections: &mut Vec<String>) {
    if directives_enabled(
        text,
        leek_span::FeatureFlags::from_env().function_signatures,
    ) {
        if let Some((doc_text, directives)) = doc_and_directives_before(text, decl_start) {
            if !doc_text.trim().is_empty() {
                sections.push(doc_text);
            }
            if !directives.is_empty() {
                let mut lines = vec!["**Backend implementations:**".to_string()];
                for (backend, body) in directives.iter() {
                    lines.push(format!("- `{backend}`: `{body}`"));
                }
                sections.push(lines.join("\n"));
            }
        }
    } else if let Some(doc_text) = doc_comment_before(text, decl_start) {
        sections.push(doc_text);
    }
}

/// Render hover sections for a library builtin / leek-wars function from
/// the embedded `.leek` headers: a code block with every overload, plus
/// the first overload's doc-comment (if any). `None` when the name isn't
/// a known library function — the caller keeps its `builtin <name>`
/// fallback.
/// The inferred return/field type for a declaration node, as a display
/// string, when the checker knows something better than `any`. Passed as
/// the [`signature_for_with`] fallback so unannotated functions, methods,
/// and fields still show a type on hover.
fn infer_decl_type(sigs: &InferredSignatures, node: &SyntaxNode) -> Option<String> {
    let known = |ty: &Type| (!matches!(ty, Type::Any)).then(|| format_type(ty));
    match node.kind() {
        SyntaxKind::FnDecl => {
            let name = member_decl_name(node)?;
            sigs.fn_returns.get(&name).and_then(known)
        }
        SyntaxKind::ClassMethod => {
            let name = member_decl_name(node)?;
            let class = enclosing_class_of(node)?;
            sigs.method_returns.get(&class)?.get(&name).and_then(known)
        }
        SyntaxKind::ClassField => {
            let name = member_decl_name(node)?;
            let class = enclosing_class_of(node)?;
            sigs.field_types.get(&class)?.get(&name).and_then(known)
        }
        _ => None,
    }
}

/// The library-function name under the cursor, when the cursor sits on an
/// `Ident` used as a bare name / call callee (a `NameRef`) — not a field
/// name (`recv.count`) or a declaration. Returns the name + its span.
fn builtin_name_at(root: &SyntaxNode, offset: u32) -> Option<(String, Span)> {
    for el in root.descendants_with_tokens() {
        let Some(tok) = el.into_token() else { continue };
        if tok.kind() != SyntaxKind::Ident {
            continue;
        }
        let r = tok.text_range();
        if u32::from(r.start()) <= offset && offset < u32::from(r.end()) {
            // A `NameRef` parent means a bare name or call callee; field
            // names hang directly off a `FieldExpr`, declarations off
            // FnDecl/Param/etc., so this cleanly excludes them.
            if tok
                .parent()
                .is_some_and(|p| p.kind() == SyntaxKind::NameRef)
            {
                let span = Span::new(
                    leek_span::SourceId::new(1).unwrap(),
                    u32::from(r.start()),
                    u32::from(r.end()),
                );
                return Some((tok.text().to_string(), span));
            }
            return None;
        }
    }
    None
}

fn builtin_signature_section(name: &str) -> Option<(String, Option<String>)> {
    let sigs = leek_ide::library_sigs::library_signatures(name)?;
    if sigs.is_empty() {
        return None;
    }
    let body = sigs
        .iter()
        .map(|s| s.signature.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    let code = format!("```leekscript\n{body}\n```");
    let doc = sigs.iter().find_map(|s| s.doc.clone());
    Some((code, doc))
}

/// Build a function's value type `Function<P0, … => R>` from the
/// checker's recorded parameter + return types. `None` for a name the
/// checker has no signature for (e.g. a class method, which lives in a
/// per-class map, or a builtin).
fn function_type_string(sigs: &InferredSignatures, name: &str) -> Option<String> {
    let params = sigs.fn_params.get(name);
    let ret = sigs.fn_returns.get(name);
    if params.is_none() && ret.is_none() {
        return None;
    }
    let ty = Type::function_with(
        params.cloned().unwrap_or_default(),
        ret.cloned().unwrap_or(Type::Any),
    );
    Some(format_type(&ty))
}

fn symbol_kind_label(kind: leek_resolver::SymbolKind) -> &'static str {
    use leek_resolver::SymbolKind::{Builtin, Class, Field, Function, Global, Local, Param};
    match kind {
        Global => "global",
        Function => "function",
        Class => "class",
        Param => "param",
        Local => "local",
        Field => "field",
        Builtin => "builtin",
    }
}

/// Render the complexity row for a function name. Returns `None` when
/// `hir` is absent (lowering failed) or the function isn't found.
///
/// Uses the file-level analysis (not the standalone `analyze_function`)
/// so a call to another user function substitutes the callee's formula
/// instead of collapsing to `O(?)` — matching what the codeLens,
/// `leek.showComplexity` command, and `miku analyze` already report.
fn complexity_section(hir: Option<&HirArtifact>, name: &str) -> Option<String> {
    let hir = hir?;
    let report = analyze_file(&hir.0);
    let complexity = report.iter().find(|c| c.name == name)?;
    // For a constant-cost function the operation count is more useful
    // than a bare `O(1)` — the formula has already simplified to that
    // scalar, so show it as the cost directly.
    if matches!(complexity.big_o, leek_complexity::BigO::Constant) {
        return Some(format!("**Cost:** `{}` operations", complexity.formula));
    }
    Some(format!(
        "**Complexity:** {}  \n*ops:* `{}`",
        complexity.big_o, complexity.formula,
    ))
}

/// Resolve a hover on a member-access name token (`recv.member`) to the
/// member's declaration signature. The receiver's class comes from the
/// type table (instance access, including `this`/`super`) or, for a
/// static `Class.member`, from a same-named class declaration. Walks
/// the inheritance chain so inherited members resolve too.
fn member_access_hover(
    root: &SyntaxNode,
    resolve_art: Option<&leek_resolver::pipeline::ResolveArtifact>,
    table: &leek_types::TypeTable,
    signatures: &InferredSignatures,
    offset: u32,
) -> Option<(String, Span)> {
    let (field_expr, field_tok) = field_access_at(root, offset)?;
    let base = field_expr.base()?;
    let class_name = super::member::base_class_name(root, resolve_art, table, &base)?;
    let member = find_member_in_chain(root, &class_name, field_tok.text())?;
    let sig = signature_for_with(&member, &|n| infer_decl_type(signatures, n))?;
    let r = field_tok.text_range();
    Some((
        sig,
        Span::new(
            leek_span::SourceId::new(1).unwrap(),
            u32::from(r.start()),
            u32::from(r.end()),
        ),
    ))
}
