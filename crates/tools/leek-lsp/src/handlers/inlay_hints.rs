//! `textDocument/inlayHint` — show inferred types inline after `var`
//! declarations that have no annotation.

use leek_parser::ast::{AstNode, Expr};
use leek_resolver::SymbolKind;
use leek_syntax::{SyntaxKind, SyntaxNode};
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
    let green = &run.get::<leek_parser::pipeline::GreenTreeArtifact>()?.0;
    let root = SyntaxNode::new_root(green.clone());

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
        // Hints only apply to unannotated `var`/`global` declarations
        // with an initializer — `Entity x = …` already states its
        // type, and a foreach binding has no initializer expression.
        let Some((init_start, init_end)) = initializer_range(&root, off) else {
            continue;
        };
        // The checker's type for the initializer expression itself —
        // a range query, so `fm.myLeek` yields the field access's
        // type, not the type of the `fm` base it starts with.
        let inferred = type_art
            .table
            .spanning(init_start, init_end)
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

/// Byte range of the initializer expression for the declaration whose
/// name token starts at `name_offset`. Returns `None` when the binding
/// isn't a `var`/`global` declaration (e.g. a foreach loop variable),
/// already has an explicit type annotation, or has no `= expr` part —
/// no hint in any of those cases. Multi-declarations
/// (`var a = 1, b = 'x'`) resolve each name to its own initializer.
fn initializer_range(root: &SyntaxNode, name_offset: u32) -> Option<(u32, u32)> {
    let name_tok = root.token_at_offset(name_offset.into()).right_biased()?;
    let decl = name_tok.parent()?;
    if decl.kind() != SyntaxKind::VarDeclStmt {
        return None;
    }
    // A leading TypeRef annotates every name the statement declares.
    if decl.children().any(|n| n.kind() == SyntaxKind::TypeRef) {
        return None;
    }
    let mut seen_name = false;
    for el in decl.children_with_tokens() {
        match el {
            leek_syntax::language::NodeOrToken::Token(t) => {
                if t.text_range() == name_tok.text_range() {
                    seen_name = true;
                } else if seen_name && t.kind() == SyntaxKind::Comma {
                    // Next declarator — this name had no initializer.
                    return None;
                }
            }
            leek_syntax::language::NodeOrToken::Node(n) => {
                if seen_name && Expr::cast(n.clone()).is_some() {
                    let r = n.text_range();
                    return Some((r.start().into(), r.end().into()));
                }
            }
        }
    }
    None
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
    fn annotated_declaration_gets_no_hint() {
        // `Entity x = …` already states its type — no hint, even when
        // the checker has an inferred type for the initializer.
        let (ws, uri) = ws_with("class Entity { cell = 1 }\nEntity x = new Entity()\n");
        let hints = handle(&ws, &uri, whole_file()).expect("hints");
        assert!(hints.is_empty(), "annotated decl must not hint: {hints:?}");
    }

    #[test]
    fn field_access_initializer_hints_field_type() {
        // Regression: the old point query at `def_span.end + 3` landed
        // on the *base* of `fm.leek` and hinted the base's class
        // (`: FM`) instead of the field's type.
        let (ws, uri) = ws_with(
            "class Entity { cell = 1 }\n\
             class FM { Entity leek = new Entity() }\n\
             var fm = new FM()\n\
             var x = fm.leek\n",
        );
        let hints = handle(&ws, &uri, whole_file()).expect("hints");
        let labels: Vec<String> = hints
            .iter()
            .map(|h| match &h.label {
                lsp::InlayHintLabel::String(s) => s.clone(),
                other @ lsp::InlayHintLabel::LabelParts(_) => format!("{other:?}"),
            })
            .collect();
        assert!(
            labels.iter().any(|l| l == ": Entity"),
            "field-access RHS should hint the field type: {labels:?}"
        );
        // Exactly one `: FM` — the `var fm` hint. A second one means
        // `var x` was hinted from the base instead of the field.
        assert_eq!(
            labels.iter().filter(|l| *l == ": FM").count(),
            1,
            "must not hint the base's class for `var x = fm.leek`: {labels:?}"
        );
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
