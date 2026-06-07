//! `textDocument/implementation` — find the implementations of the
//! symbol under the cursor, across the whole *program*.
//!
//! Concrete rules for Leekscript:
//!
//! - Cursor on a **class** → every transitive subclass (`class X
//!   extends Y` chains), in any `include`d file.
//! - Cursor on a **method** → every subclass's own same-named method
//!   (overrides). The cursor's enclosing class is the base.
//!
//! Subclasses and overrides are gathered from a program-wide class
//! index, so a subclass declared in another file is found and its
//! location points at *that* file. Falls back to the cursor's own
//! declaration so "Go to Implementation" never dead-ends, and resolves
//! a cross-file class reference when the cursor isn't on a local symbol.

use std::collections::{HashSet, VecDeque};

use leek_hir::pipeline::HirArtifact;
use leek_hir::Def;
use leek_pipeline::salsa::SourceFile;
use leek_resolver::SymbolKind;
use leek_span::{LineTable, Span};
use tower_lsp::lsp_types as lsp;

use crate::util::position::{PosMap, position_to_offset, span_to_range};
use crate::workspace::Workspace;

pub fn handle(
    ws: &Workspace,
    uri: &lsp::Url,
    pos: lsp::Position,
) -> Option<lsp::request::GotoImplementationResponse> {
    let doc = ws.doc(uri)?;
    let offset = position_to_offset(doc.pos_map(), pos)?;
    let run = crate::pipeline::run(ws, uri, leek_recipes::Target::Hir)?;
    let table = &run.get::<leek_resolver::pipeline::ResolveArtifact>()?.table;
    let hir = run.get::<HirArtifact>()?;

    let mut locations: Vec<lsp::Location> = Vec::new();

    // 1. Cursor on a locally-resolved class or method.
    if let Some(sym) = crate::handlers::resolve_symbol(table, offset).cloned() {
        match sym.kind {
            SymbolKind::Class => {
                // Subclasses anchored on this class's home (= this file).
                collect_subclasses(ws, uri, &sym.name, &mut locations);
            }
            SymbolKind::Function => {
                // A method symbol: find the enclosing class (the base)
                // from this file's HIR, then walk subclass overrides.
                if let Some(base) = base_class_for_method(&hir.0, offset, &sym.name) {
                    collect_overrides(ws, uri, &base, &sym.name, &mut locations);
                }
            }
            _ => {}
        }
        if locations.is_empty() {
            locations.push(loc(uri, doc.pos_map(), sym.def_span));
        }
        return Some(lsp::request::GotoImplementationResponse::Array(locations));
    }

    // 2. Cursor on a use of a class declared in an `include`d file.
    let green = &run.get::<leek_parser::pipeline::GreenTreeArtifact>()?.0;
    let root = leek_syntax::SyntaxNode::new_root(green.clone());
    let name = crate::handlers::ident_name_at(&root, offset)?;
    let (file, sym) = crate::handlers::find_top_level_decl(ws, uri, &name)?;
    if sym.kind != SymbolKind::Class {
        return None;
    }
    collect_subclasses(ws, &file.uri, &sym.name, &mut locations);
    if locations.is_empty() {
        locations.push(loc_in(ws, &file.uri, file.source_file, sym.def_span));
    }
    Some(lsp::request::GotoImplementationResponse::Array(locations))
}

/// Push a location for every transitive subclass of `class_name`. The
/// program is anchored on `home_uri` (the class's declaring file) so
/// subclasses in files that include it are reached.
fn collect_subclasses(
    ws: &Workspace,
    home_uri: &lsp::Url,
    class_name: &str,
    out: &mut Vec<lsp::Location>,
) {
    let classes = program_classes(ws, home_uri);
    for sub in subclasses_of(&classes, class_name) {
        if let Some(c) = classes.iter().find(|c| c.name == sub) {
            out.push(loc_in(ws, &c.uri, c.source_file, c.def_span));
        }
    }
}

/// Push a location for every subclass's own override of `method_name`.
fn collect_overrides(
    ws: &Workspace,
    home_uri: &lsp::Url,
    base_class: &str,
    method_name: &str,
    out: &mut Vec<lsp::Location>,
) {
    let classes = program_classes(ws, home_uri);
    for sub in subclasses_of(&classes, base_class) {
        if let Some(c) = classes.iter().find(|c| c.name == sub)
            && let Some((_, span)) = c.methods.iter().find(|(n, _)| n == method_name)
        {
            out.push(loc_in(ws, &c.uri, c.source_file, *span));
        }
    }
}

/// The class (by name) whose body brackets `offset` and which declares a
/// method named `method`. The reliable base for an override search —
/// the resolver records methods with no `container`, so we use HIR.
fn base_class_for_method(hir: &leek_hir::HirFile, offset: u32, method: &str) -> Option<String> {
    hir.defs.iter().find_map(|d| match d {
        Def::Class(c) => {
            let in_class = c.span.start <= offset && offset < c.span.end;
            let has_method = c.methods.iter().any(|m| m.name == method);
            (in_class && has_method).then(|| c.name.clone())
        }
        _ => None,
    })
}

/// One class across the program, with the data implementation needs:
/// inheritance parent, declaration span, and its methods' spans.
struct ProgClass {
    uri: lsp::Url,
    source_file: SourceFile,
    name: String,
    parent: Option<String>,
    def_span: Span,
    methods: Vec<(String, Span)>,
}

/// Every class in the program `home_uri` belongs to, joined from each
/// file's HIR (parent + methods) and resolve table (the class's
/// declaration span).
fn program_classes(ws: &Workspace, home_uri: &lsp::Url) -> Vec<ProgClass> {
    let mut out: Vec<ProgClass> = Vec::new();
    for file in crate::handlers::program_scope::program_scope(ws, home_uri) {
        let Some(run) = crate::pipeline::run_on_file(ws, file.source_file, leek_recipes::Target::Hir)
        else {
            continue;
        };
        let Some(table) = run
            .get::<leek_resolver::pipeline::ResolveArtifact>()
            .map(|a| &a.table)
        else {
            continue;
        };
        let Some(hir) = run.get::<HirArtifact>() else {
            continue;
        };
        for def in &hir.0.defs {
            let Def::Class(c) = def else { continue };
            let Some(sym) = table
                .symbols
                .iter()
                .find(|s| s.kind == SymbolKind::Class && s.name == c.name)
            else {
                continue;
            };
            out.push(ProgClass {
                uri: file.uri.clone(),
                source_file: file.source_file,
                name: c.name.clone(),
                parent: c.parent.clone(),
                def_span: sym.def_span,
                methods: c.methods.iter().map(|m| (m.name.clone(), m.span)).collect(),
            });
        }
    }
    out
}

/// All classes (transitively) extending `name`, breadth-first,
/// excluding `name` itself.
fn subclasses_of(classes: &[ProgClass], name: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    let mut queue: VecDeque<String> = VecDeque::new();
    queue.push_back(name.to_string());
    seen.insert(name.to_string());
    while let Some(cur) = queue.pop_front() {
        for c in classes {
            if c.parent.as_deref() == Some(cur.as_str()) && seen.insert(c.name.clone()) {
                out.push(c.name.clone());
                queue.push_back(c.name.clone());
            }
        }
    }
    out
}

fn loc(uri: &lsp::Url, pm: PosMap<'_>, span: Span) -> lsp::Location {
    lsp::Location {
        uri: uri.clone(),
        range: span_to_range(pm, span),
    }
}

/// Build a location whose span is converted against `source_file`'s own
/// text (a subclass/override may live in a different file).
fn loc_in(ws: &Workspace, uri: &lsp::Url, source_file: SourceFile, span: Span) -> lsp::Location {
    let text = source_file.text(&ws.db);
    let line_table = LineTable::new(text);
    let pm = PosMap::new(&line_table, text);
    loc(uri, pm, span)
}
