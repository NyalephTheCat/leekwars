//! `textDocument/typeDefinition` — jump to the *type* of the
//! symbol at cursor.
//!
//! - For a variable typed `ClassInstance("Cat")`, jumps to the
//!   `class Cat` declaration.
//! - For a function reference, jumps to the function itself
//!   (same as `definition`, since the function name IS the
//!   type's source of truth).
//! - Returns `None` for primitive types (no source to jump to).

use leek_hir::Def;
use leek_hir::pipeline::HirArtifact;
use leek_resolver::SymbolKind;
use leek_syntax::{SyntaxKind, SyntaxNode};
use leek_types::Type;
use tower_lsp::lsp_types as lsp;

use super::member::{self, class_name_of_type};
use crate::util::position::{position_to_offset, span_to_range};
use crate::workspace::Workspace;

pub fn handle(
    ws: &Workspace,
    uri: &lsp::Url,
    pos: lsp::Position,
) -> Option<lsp::request::GotoTypeDefinitionResponse> {
    let doc = ws.doc(uri)?;
    let offset = position_to_offset(doc.pos_map(), pos)?;

    let run = crate::pipeline::run(ws, uri, leek_recipes::Target::Hir)?;
    let table = &run.get::<leek_resolver::pipeline::ResolveArtifact>()?.table;
    let type_table = &run.get::<leek_types::pipeline::TypeCheckArtifact>()?.table;
    let hir = run.get::<HirArtifact>()?;
    let green = &run.get::<leek_parser::pipeline::GreenTreeArtifact>()?.0;
    let root = SyntaxNode::new_root(green.clone());

    // Type lookup strategy:
    //  (a) Smallest type entry covering the offset (works for
    //      expressions and use sites).
    //  (b) Fallback: the resolver's symbol whose def_span covers
    //      the cursor — get its declared type via the matching
    //      HIR def (handles "cursor on the var name").
    let class_name = type_table
        .smallest_at(offset)
        .map(|e| e.ty.clone())
        .filter(|t| !matches!(t, Type::Any))
        .and_then(|t| class_name_of_type(&t))
        .or_else(|| {
            // Either:
            //  (a) cursor on a use site → reference_at gives us
            //      the binding symbol;
            //  (b) cursor on the binding's identifier → def_span
            //      covers cursor.
            // In both cases we want the binding's full_span and
            // then the most informative typed expression inside it
            // (its `Any` entry isn't useful — we want the init
            // expression's `ClassInstance` etc.).
            let sym = crate::handlers::resolve_symbol(table, offset).cloned()?;
            // Try the HIR-declared type first.
            if let Some(ty) = type_of_symbol(&hir.0, &sym.name, sym.kind)
                && let Some(name) = class_name_of_type(&ty)
            {
                return Some(name);
            }
            // Fallback: the binding's own initializer expression.
            // Declarator-aware — `var a = new A(), b = new B()` reads
            // `new B()` for `b`, never a sibling's initializer.
            let cst_off = sym.def_span.start;
            if let Some(entry) = member::initializer_type(&root, type_table, cst_off)
                && let Some(name) = class_name_of_type(&entry.ty)
            {
                return Some(name);
            }
            // Last resort: the largest non-Any type entry inside the
            // enclosing declaration node (params, class fields — the
            // shapes initializer_type doesn't cover precisely).
            let parent_span = enclosing_decl_span(&root, cst_off);
            let (lo, hi) = parent_span.unwrap_or((sym.full_span.start, sym.full_span.end));
            type_table
                .exprs
                .iter()
                .filter(|e| e.span.start >= lo && e.span.end <= hi && !matches!(e.ty, Type::Any))
                .max_by_key(|e| e.span.end - e.span.start)
                .and_then(|e| class_name_of_type(&e.ty))
        })?;

    let class_sym = table
        .symbols
        .iter()
        .find(|s| s.kind == SymbolKind::Class && s.name == class_name)?;

    let range = span_to_range(doc.pos_map(), class_sym.def_span);
    Some(lsp::request::GotoTypeDefinitionResponse::Scalar(
        lsp::Location {
            uri: uri.clone(),
            range,
        },
    ))
}

/// Walk up the CST from `offset` to find an enclosing
/// `VarDeclStmt`, `Param`, or `ClassField`, returning that node's
/// byte range.
fn enclosing_decl_span(root: &SyntaxNode, offset: u32) -> Option<(u32, u32)> {
    let token = root.token_at_offset(offset.into()).right_biased()?;
    let mut node: Option<SyntaxNode> = token.parent();
    while let Some(n) = node {
        if matches!(
            n.kind(),
            SyntaxKind::VarDeclStmt | SyntaxKind::Param | SyntaxKind::ClassField
        ) {
            let r = n.text_range();
            return Some((u32::from(r.start()), u32::from(r.end())));
        }
        node = n.parent();
    }
    None
}

fn type_of_symbol(hir: &leek_hir::HirFile, name: &str, kind: SymbolKind) -> Option<Type> {
    // For locals + globals we look up VarDecl-style HIR defs.
    // Functions return their declared return type. Classes
    // already ARE their own type.
    for def in &hir.defs {
        match def {
            Def::Function(f) if f.name == name && matches!(kind, SymbolKind::Function) => {
                return f.return_type.clone();
            }
            Def::Global(g) if g.name == name => return g.ty.clone(),
            Def::Local(l) if l.name == name => return l.ty.clone(),
            _ => {}
        }
    }
    None
}
