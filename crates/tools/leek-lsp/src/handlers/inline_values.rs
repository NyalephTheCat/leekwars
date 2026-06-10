//! `textDocument/inlineValue` — variable values shown inline during a
//! debug session.
//!
//! The editor sends this while paused at a breakpoint (the
//! [`context.stopped_location`](lsp::InlineValueContext) is the current
//! line). We return a [`InlineValueVariableLookup`] for each in-scope
//! variable occurrence; the editor then fetches the *actual* value from
//! the active debug adapter (this project's `leek-dap`) and renders it
//! at the end of the line. So the LSP never touches runtime state — it
//! only points the debugger at the variables and their ranges, which is
//! exactly the division of labour the protocol intends.
//!
//! We emit lookups for the declaration and every reference of each
//! local, parameter, and global within the requested range, bounded to
//! the stopped line (a variable used *after* the stop point hasn't run
//! yet, so its value would be misleading). Functions and classes are
//! skipped — they aren't values you watch.

use leek_resolver::SymbolKind;
use leek_span::Span;
use tower_lsp::lsp_types as lsp;

use crate::util::position::{PosMap, position_to_offset, span_to_range};
use crate::workspace::Workspace;

pub fn handle(
    ws: &Workspace,
    uri: &lsp::Url,
    range: lsp::Range,
    stopped_location: lsp::Range,
) -> Option<Vec<lsp::InlineValue>> {
    let doc = ws.doc(uri)?;
    let pm = doc.pos_map();
    let run = crate::pipeline::run(ws, uri, leek_recipes::Target::Resolved)?;
    let table = &run.get::<leek_resolver::pipeline::ResolveArtifact>()?.table;

    let range_start = position_to_offset(pm, range.start)?;
    let range_end = position_to_offset(pm, range.end)?;
    // Inline values are shown up to the stopped line (its end position
    // denotes the line). Occurrences past it haven't executed.
    let stop_line = stopped_location.end.line;

    let in_window = |off: u32| -> bool {
        off >= range_start && off < range_end && pm.to_position(off).line <= stop_line
    };
    let is_var = |k| {
        matches!(
            k,
            SymbolKind::Local | SymbolKind::Param | SymbolKind::Global
        )
    };

    let mut out: Vec<lsp::InlineValue> = Vec::new();

    // Declarations (`var x` — shows the value right at the decl).
    for sym in &table.symbols {
        if is_var(sym.kind) && in_window(sym.def_span.start) {
            out.push(variable_lookup(pm, sym.def_span, &sym.name));
        }
    }
    // References (reads and assignment targets).
    for r in &table.references {
        let Some(sym) = table.symbol(r.target) else {
            continue;
        };
        if is_var(sym.kind) && in_window(r.name_offset) {
            let span = Span::new(
                doc.source_file_source_id(&ws.db),
                r.name_offset,
                r.name_offset + r.name_len,
            );
            out.push(variable_lookup(pm, span, &sym.name));
        }
    }

    Some(out)
}

fn variable_lookup(pm: PosMap<'_>, span: Span, name: &str) -> lsp::InlineValue {
    lsp::InlineValue::VariableLookup(lsp::InlineValueVariableLookup {
        range: span_to_range(pm, span),
        // The name is also extractable from the range text, but stating
        // it makes the debugger's lookup unambiguous.
        variable_name: Some(name.to_string()),
        // Leekscript identifiers are case-sensitive.
        case_sensitive_lookup: true,
    })
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

    fn range(sl: u32, sc: u32, el: u32, ec: u32) -> lsp::Range {
        lsp::Range {
            start: lsp::Position {
                line: sl,
                character: sc,
            },
            end: lsp::Position {
                line: el,
                character: ec,
            },
        }
    }

    fn names(vals: &[lsp::InlineValue]) -> Vec<String> {
        vals.iter()
            .filter_map(|v| match v {
                lsp::InlineValue::VariableLookup(l) => l.variable_name.clone(),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn emits_lookups_for_locals_up_to_stop_line() {
        // Stopped on line 2 (`var y = x + 1`).
        let (ws, uri) = ws_with("function f() {\n  var x = 1\n  var y = x + 1\n  return y\n}\n");
        let whole = range(0, 0, 5, 0);
        let stopped = range(2, 0, 2, 20);
        let vals = handle(&ws, &uri, whole, stopped).expect("values");
        let ns = names(&vals);
        // `x`: declaration (line 1) + use (line 2) = 2.
        assert_eq!(ns.iter().filter(|n| *n == "x").count(), 2, "{ns:?}");
        // `y`: declaration (line 2) only — the `return y` use is on line
        // 3, past the stop line, so it's excluded.
        assert_eq!(ns.iter().filter(|n| *n == "y").count(), 1, "{ns:?}");
        // Nothing past the stopped line.
        assert!(
            vals.iter().all(|v| match v {
                lsp::InlineValue::VariableLookup(l) => l.range.start.line <= 2,
                _ => true,
            }),
            "no occurrence past the stop line"
        );
    }

    #[test]
    fn all_lookups_are_case_sensitive() {
        let (ws, uri) = ws_with("var a = 1\nvar b = a\n");
        let vals = handle(&ws, &uri, range(0, 0, 2, 0), range(1, 0, 1, 9)).unwrap();
        assert!(!vals.is_empty());
        assert!(vals.iter().all(|v| match v {
            lsp::InlineValue::VariableLookup(l) => l.case_sensitive_lookup,
            _ => false,
        }));
    }

    #[test]
    fn skips_functions_and_classes() {
        let (ws, uri) = ws_with("function helper() { return 1 }\nclass Cat {}\nvar n = helper()\n");
        let vals = handle(&ws, &uri, range(0, 0, 3, 0), range(2, 0, 2, 20)).unwrap();
        let ns = names(&vals);
        // `helper` and `Cat` are not watchable values; only `n` is.
        assert!(ns.contains(&"n".to_string()), "{ns:?}");
        assert!(!ns.contains(&"helper".to_string()), "{ns:?}");
        assert!(!ns.contains(&"Cat".to_string()), "{ns:?}");
    }

    #[test]
    fn respects_the_requested_range() {
        // Only line 1's `b` is in range; line 0's `a` is excluded.
        let (ws, uri) = ws_with("var a = 1\nvar b = 2\n");
        let vals = handle(&ws, &uri, range(1, 0, 2, 0), range(1, 0, 1, 9)).unwrap();
        let ns = names(&vals);
        assert!(ns.contains(&"b".to_string()), "{ns:?}");
        assert!(!ns.contains(&"a".to_string()), "{ns:?}");
    }
}
