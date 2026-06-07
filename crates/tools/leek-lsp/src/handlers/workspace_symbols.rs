//! `workspace/symbol` — substring search across every file the
//! workspace knows about (open buffers + indexed project files).
//!
//! Class members (methods, fields) carry their declaring class as
//! `container_name` so the editor's symbol picker shows `Cat.meow`
//! rather than a bare `meow`. The resolver records members with no
//! `container`, so the class name is recovered structurally from the
//! CST via [`enclosing_class_name`](crate::handlers::enclosing_class_name).

use leek_recipes::Target;
use leek_resolver::SymbolKind;
use leek_syntax::SyntaxNode;
use tower_lsp::lsp_types as lsp;

use crate::util::position::span_to_range;
use crate::workspace::Workspace;

pub fn handle(ws: &Workspace, query: &str) -> Option<Vec<lsp::SymbolInformation>> {
    let lower = query.to_ascii_lowercase();
    let mut out: Vec<lsp::SymbolInformation> = Vec::new();

    for target in ws.analysis_targets() {
        // Recipe planning can fail; skip this target rather than crash.
        let Some(run) = crate::pipeline::run_on_file(ws, target.source_file, Target::Resolved)
        else {
            continue;
        };
        let Some(art) = run.get::<leek_resolver::pipeline::ResolveArtifact>() else {
            continue;
        };
        let root = run
            .get::<leek_parser::pipeline::GreenTreeArtifact>()
            .map(|g| SyntaxNode::new_root(g.0.clone()));
        for sym in &art.table.symbols {
            if !sym.name.to_ascii_lowercase().contains(&lower) {
                continue;
            }
            // Methods/fields show their class; top-level symbols don't.
            let container_name = root
                .as_ref()
                .and_then(|r| crate::handlers::enclosing_class_name(r, sym.def_span.start));
            #[allow(deprecated)]
            out.push(lsp::SymbolInformation {
                name: sym.name.clone(),
                kind: to_lsp_symbol_kind(sym.kind),
                tags: None,
                deprecated: None,
                location: lsp::Location {
                    uri: target.uri.clone(),
                    range: span_to_range(target.pos_map(), sym.def_span),
                },
                container_name,
            });
        }
    }

    out.sort_by(|a, b| a.name.cmp(&b.name));
    Some(out)
}

fn to_lsp_symbol_kind(k: SymbolKind) -> lsp::SymbolKind {
    match k {
        SymbolKind::Global | SymbolKind::Local => lsp::SymbolKind::VARIABLE,
        SymbolKind::Function => lsp::SymbolKind::FUNCTION,
        SymbolKind::Class => lsp::SymbolKind::CLASS,
        SymbolKind::Param => lsp::SymbolKind::VARIABLE,
        SymbolKind::Field => lsp::SymbolKind::FIELD,
        SymbolKind::Builtin => lsp::SymbolKind::FUNCTION,
    }
}
