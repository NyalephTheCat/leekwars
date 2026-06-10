//! `textDocument/inlayHint` — show inferred types inline after `var`
//! declarations that have no annotation.

use leek_resolver::SymbolKind;
use leek_types::Type;
use tower_lsp::lsp_types as lsp;

use crate::util::position::offset_to_position;
use crate::workspace::Workspace;

pub fn handle(ws: &Workspace, uri: &lsp::Url, range: lsp::Range) -> Option<Vec<lsp::InlayHint>> {
    // Respect the client's `leek.inlayHints` toggle — an empty set
    // clears any hints the editor is currently showing.
    if !ws.settings.inlay_hints {
        return Some(Vec::new());
    }
    let doc = ws.doc(uri)?;
    let run = crate::pipeline::run(ws, uri, leek_recipes::Target::TypeChecked)?;

    let resolve_art = run.get::<leek_resolver::pipeline::ResolveArtifact>()?;
    let type_art = run.get::<leek_types::pipeline::TypeCheckArtifact>()?;

    let from_offset = position_to_offset_local(doc, range.start);
    let to_offset = position_to_offset_local(doc, range.end);

    let mut out: Vec<lsp::InlayHint> = Vec::new();
    for sym in &resolve_art.table.symbols {
        if !matches!(sym.kind, SymbolKind::Local | SymbolKind::Global) {
            continue;
        }
        let off = sym.def_span.start;
        if let Some(from) = from_offset
            && off < from
        {
            continue;
        }
        if let Some(to) = to_offset
            && off > to
        {
            continue;
        }
        // Look up the inferred type at the var's def site (typed by
        // the checker via the initializer expression).
        let inferred = type_art
            .table
            .smallest_at(sym.def_span.end + 3) // best-effort: after `= `
            .map_or(Type::Any, |t| t.ty.clone());
        if matches!(inferred, Type::Any) {
            continue;
        }
        let type_name = format_type(&inferred);
        out.push(lsp::InlayHint {
            position: offset_to_position(doc.pos_map(), sym.def_span.end),
            label: lsp::InlayHintLabel::String(format!(": {type_name}")),
            kind: Some(lsp::InlayHintKind::TYPE),
            text_edits: None,
            // The hover tooltip is deferred to `resolve`: we stash the
            // type name in `data` and only build the markdown when the
            // user actually points at this one hint.
            tooltip: None,
            padding_left: Some(false),
            padding_right: Some(true),
            data: Some(serde_json::Value::String(type_name)),
        });
    }
    Some(out)
}

/// `inlayHint/resolve` — build the hover tooltip for the focused hint
/// from the type name stashed in its `data`. Returns the hint
/// unchanged when it has no `data` or is already resolved.
pub fn resolve(mut hint: lsp::InlayHint) -> lsp::InlayHint {
    if hint.tooltip.is_some() {
        return hint;
    }
    if let Some(serde_json::Value::String(type_name)) = &hint.data {
        hint.tooltip = Some(lsp::InlayHintTooltip::MarkupContent(lsp::MarkupContent {
            kind: lsp::MarkupKind::Markdown,
            value: format!("Inferred type: `{type_name}`"),
        }));
    }
    hint
}

fn position_to_offset_local(doc: &crate::documents::DocHandle, pos: lsp::Position) -> Option<u32> {
    crate::util::position::position_to_offset(doc.pos_map(), pos)
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workspace::Workspace;

    fn ws_with(src: &str) -> (Workspace, lsp::Url) {
        let mut ws = Workspace::default();
        let uri = lsp::Url::parse("file:///t.leek").unwrap();
        ws.open(uri.clone(), src.to_string());
        (ws, uri)
    }

    fn whole_file() -> lsp::Range {
        lsp::Range {
            start: lsp::Position {
                line: 0,
                character: 0,
            },
            end: lsp::Position {
                line: u32::MAX,
                character: 0,
            },
        }
    }

    #[test]
    fn hint_defers_tooltip_but_carries_type_data() {
        // `var n = 1 + 2` infers integer with no annotation → one hint.
        let (ws, uri) = ws_with("var n = 1 + 2\n");
        let hints = handle(&ws, &uri, whole_file()).expect("hints");
        let hint = hints.first().expect("at least one hint");
        assert!(hint.tooltip.is_none(), "tooltip must be deferred");
        assert_eq!(hint.data, Some(serde_json::Value::String("integer".into())));
    }

    #[test]
    fn resolve_builds_tooltip_from_data() {
        let (ws, uri) = ws_with("var n = 1 + 2\n");
        let hint = handle(&ws, &uri, whole_file()).unwrap().remove(0);
        let resolved = resolve(hint);
        let lsp::InlayHintTooltip::MarkupContent(m) = resolved.tooltip.expect("tooltip") else {
            panic!("expected markup tooltip");
        };
        assert!(m.value.contains("integer"), "tooltip = {:?}", m.value);
    }

    #[test]
    fn resolve_is_noop_without_data() {
        let hint = lsp::InlayHint {
            position: lsp::Position {
                line: 0,
                character: 0,
            },
            label: lsp::InlayHintLabel::String(": integer".into()),
            kind: Some(lsp::InlayHintKind::TYPE),
            text_edits: None,
            tooltip: None,
            padding_left: None,
            padding_right: None,
            data: None,
        };
        assert!(resolve(hint).tooltip.is_none());
    }
}
