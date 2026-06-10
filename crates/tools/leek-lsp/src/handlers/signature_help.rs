//! `textDocument/signatureHelp` — while the cursor is inside the
//! argument list of a call, show the callee's signature and
//! highlight the parameter the user is currently typing.
//!
//! Resolution order for the callee:
//!  1. Resolver hit for a user-declared function → render its
//!     `FnDecl` CST node via [`signature_for`].
//!  2. Otherwise, if the callee identifier matches a row in
//!     [`leek_resolver::builtins::BUILTIN_FNS`], render it as
//!     `builtin name(arg1, arg2, ...)` using the arity table.
//!  3. Otherwise, fall back to `name(...)` with no parameter list.
//!
//! Active parameter index = number of top-level commas in the
//! ArgList that lie strictly before the cursor offset.

use leek_resolver::builtins::{BUILTIN_FNS, BuiltinFn};
use leek_syntax::language::NodeOrToken;
use leek_syntax::{SyntaxKind, SyntaxNode, SyntaxToken};
use tower_lsp::lsp_types as lsp;

use crate::util::position::position_to_offset;
use crate::workspace::Workspace;
use leek_ide::signature::signature_for;

pub fn handle(ws: &Workspace, uri: &lsp::Url, pos: lsp::Position) -> Option<lsp::SignatureHelp> {
    let doc = ws.doc(uri)?;
    let offset = position_to_offset(doc.pos_map(), pos)?;

    let run = crate::pipeline::run(ws, uri, leek_recipes::Target::Resolved)?;
    let green = &run.get::<leek_parser::pipeline::GreenTreeArtifact>()?.0;
    let root = SyntaxNode::new_root(green.clone());

    // Find the smallest CallExpr whose ArgList covers the cursor.
    let call = enclosing_call_with_cursor_in_args(&root, offset)?;
    let arg_list = call.children().find(|c| c.kind() == SyntaxKind::ArgList)?;
    let active = active_param_index(&arg_list, offset);

    // Find the callee NameRef → ident.
    let callee = call.children().find(|c| c.kind() == SyntaxKind::NameRef)?;
    let callee_ident = callee
        .children_with_tokens()
        .filter_map(leek_syntax::language::NodeOrToken::into_token)
        .find(|t| t.kind() == SyntaxKind::Ident)?;
    let callee_name = callee_ident.text().to_string();

    // Render the signature. Try user fns first, then builtins.
    let (label, parameters) = resolve_user_function(&run, &root, &callee_name)
        .or_else(|| resolve_builtin(&callee_name))
        .unwrap_or_else(|| (format!("{callee_name}(...)"), Vec::new()));

    let active_parameter = if parameters.is_empty() {
        None
    } else {
        Some(active.min(u32::try_from(parameters.len() - 1).unwrap_or(u32::MAX)))
    };

    Some(lsp::SignatureHelp {
        signatures: vec![lsp::SignatureInformation {
            label,
            documentation: None,
            parameters: Some(parameters),
            active_parameter,
        }],
        active_signature: Some(0),
        active_parameter,
    })
}

/// Walk up the CST from the cursor until we find a `CallExpr`
/// whose `ArgList` strictly contains the cursor (between the `(`
/// and `)`). Returns the `CallExpr` node, or `None` if the cursor
/// isn't inside a call.
fn enclosing_call_with_cursor_in_args(root: &SyntaxNode, offset: u32) -> Option<SyntaxNode> {
    // Find the token at the cursor and walk its parents.
    let token = root.token_at_offset(offset.into()).right_biased()?;
    let mut node: Option<SyntaxNode> = token.parent();
    while let Some(n) = node {
        if n.kind() == SyntaxKind::CallExpr
            && let Some(args) = n.children().find(|c| c.kind() == SyntaxKind::ArgList)
        {
            let r = args.text_range();
            let start = u32::from(r.start());
            let end = u32::from(r.end());
            // Cursor inside the parens (allow positions equal
            // to `start` so a cursor *just after* the `(` qualifies).
            if start < offset && offset <= end {
                return Some(n);
            }
        }
        node = n.parent();
    }
    None
}

/// Count top-level commas in `arg_list` whose offset is strictly
/// less than `cursor`. The `ArgList` node itself wraps its own
/// `(` and `)`, so the outer parens push the depth counter to 1 —
/// the "top-level" we count commas at is therefore depth==1.
fn active_param_index(arg_list: &SyntaxNode, cursor: u32) -> u32 {
    let mut depth: i32 = 0;
    let mut commas: u32 = 0;
    for el in arg_list.descendants_with_tokens() {
        let NodeOrToken::Token(tok) = el else {
            continue;
        };
        if u32::from(tok.text_range().start()) >= cursor {
            break;
        }
        match tok.kind() {
            SyntaxKind::Comma if depth == 1 => commas += 1,
            _ => bump_depth(&tok, &mut depth),
        }
    }
    commas
}

fn bump_depth(t: &SyntaxToken, depth: &mut i32) {
    match t.kind() {
        SyntaxKind::LParen | SyntaxKind::LBracket | SyntaxKind::LBrace => *depth += 1,
        SyntaxKind::RParen | SyntaxKind::RBracket | SyntaxKind::RBrace => *depth -= 1,
        _ => {}
    }
}

/// Look up `name` as a user-declared function. Returns the rendered
/// label and the per-parameter sub-ranges (so the editor can
/// underline the active parameter).
fn resolve_user_function(
    run: &leek_pipeline::Run<'_>,
    root: &SyntaxNode,
    name: &str,
) -> Option<(String, Vec<lsp::ParameterInformation>)> {
    let art = run.get::<leek_resolver::pipeline::ResolveArtifact>()?;
    let sym = art
        .table
        .symbols
        .iter()
        .find(|s| s.kind == leek_resolver::SymbolKind::Function && s.name == name)?;
    // Find the FnDecl node.
    let decl = find_fn_decl_for(root, &sym.name)?;
    let label = signature_for(&decl)?;
    let parameters = parameters_in_label(&label);
    Some((label, parameters))
}

/// Find the `FnDecl` whose declared name token reads `name`.
fn find_fn_decl_for(root: &SyntaxNode, name: &str) -> Option<SyntaxNode> {
    root.descendants().find(|n| {
        n.kind() == SyntaxKind::FnDecl
            && n.children_with_tokens()
                .filter_map(leek_syntax::language::NodeOrToken::into_token)
                .find(|t| t.kind() == SyntaxKind::Ident)
                .is_some_and(|id| id.text() == name)
    })
}

/// Look up `name` in [`BUILTIN_FNS`] and render a placeholder
/// signature using arg1..argN. We don't have parameter names for
/// builtins so the labels are positional.
fn resolve_builtin(name: &str) -> Option<(String, Vec<lsp::ParameterInformation>)> {
    let entry: &BuiltinFn = BUILTIN_FNS.iter().find(|b| b.name == name)?;
    // Use min_args for the visible parameter count; show optionals
    // with a `?` suffix up to max_args.
    let min = entry.min_args as usize;
    let max = entry.max_args.min(8) as usize; // cap to keep label short
    let max = max.max(min);
    let mut parts = Vec::with_capacity(max);
    let mut params = Vec::with_capacity(max);
    for i in 0..max {
        let optional = i >= min;
        let arg_label = if optional {
            format!("arg{}?", i + 1)
        } else {
            format!("arg{}", i + 1)
        };
        parts.push(arg_label.clone());
        params.push(lsp::ParameterInformation {
            label: lsp::ParameterLabel::Simple(arg_label),
            documentation: None,
        });
    }
    let label = format!("builtin {}({})", entry.name, parts.join(", "));
    Some((label, params))
}

/// Given a rendered signature label like
/// `function f(integer a, real b) -> string`, extract each
/// comma-separated parameter chunk as a `ParameterInformation`.
/// Falls back to no parameters if we can't find a matching pair of
/// parens.
fn parameters_in_label(label: &str) -> Vec<lsp::ParameterInformation> {
    let open = label.find('(');
    let close = label.rfind(')');
    let (Some(o), Some(c)) = (open, close) else {
        return Vec::new();
    };
    if c <= o + 1 {
        return Vec::new();
    }
    let inside = &label[o + 1..c];
    if inside.trim().is_empty() {
        return Vec::new();
    }
    let mut params = Vec::new();
    let mut depth: i32 = 0;
    let mut start = 0;
    let bytes = inside.as_bytes();
    for (i, &b) in bytes.iter().enumerate() {
        match b {
            b'(' | b'[' | b'<' => depth += 1,
            b')' | b']' | b'>' => depth -= 1,
            b',' if depth == 0 => {
                let chunk = inside[start..i].trim().to_string();
                if !chunk.is_empty() {
                    params.push(lsp::ParameterInformation {
                        label: lsp::ParameterLabel::Simple(chunk),
                        documentation: None,
                    });
                }
                start = i + 1;
            }
            _ => {}
        }
    }
    let last = inside[start..].trim().to_string();
    if !last.is_empty() {
        params.push(lsp::ParameterInformation {
            label: lsp::ParameterLabel::Simple(last),
            documentation: None,
        });
    }
    params
}

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

    /// Convenience: build a Position from a 0-based line / col pair.
    fn pos(l: u32, c: u32) -> lsp::Position {
        lsp::Position {
            line: l,
            character: c,
        }
    }

    #[test]
    fn user_function_shows_param_names() {
        let src = "\
function add(integer a, integer b) {\n\
    return a + b\n\
}\n\
add(1, 2)\n";
        let (ws, uri) = ws_with(src);
        // Cursor between `(` and `1` on line 3, column 4.
        let help = handle(&ws, &uri, pos(3, 4)).expect("signature");
        let sig = &help.signatures[0];
        assert!(sig.label.contains("function add"));
        assert_eq!(sig.parameters.as_ref().unwrap().len(), 2);
        assert_eq!(help.active_parameter, Some(0));
    }

    #[test]
    fn active_param_advances_past_commas() {
        let src = "function f(integer a, integer b, integer c) { return a }\nf(10, 20, 30)\n";
        let (ws, uri) = ws_with(src);
        // After the first comma — should be on param 1.
        let after_first = "f(10, ";
        let col = u32::try_from(after_first.len()).unwrap();
        let help = handle(&ws, &uri, pos(1, col)).expect("signature");
        assert_eq!(help.active_parameter, Some(1));

        // After the second comma — should be on param 2.
        let after_second = "f(10, 20, ";
        let col = u32::try_from(after_second.len()).unwrap();
        let help = handle(&ws, &uri, pos(1, col)).expect("signature");
        assert_eq!(help.active_parameter, Some(2));
    }

    #[test]
    fn nested_call_keeps_outer_active_param() {
        // Cursor inside an outer call after the first comma but
        // before the inner call's `(`. Outer active param should be 1,
        // not bumped by inner-call commas.
        let src = "function f(integer a, integer b) { return a }\nf(g(1, 2), 9)\n";
        let (ws, uri) = ws_with(src);
        let after_inner = "f(g(1, 2), ";
        let col = u32::try_from(after_inner.len()).unwrap();
        let help = handle(&ws, &uri, pos(1, col)).expect("signature");
        assert_eq!(help.active_parameter, Some(1));
    }

    #[test]
    fn builtin_signature_is_rendered_from_arity_table() {
        // `sqrt` is min=max=1 in BUILTIN_FNS.
        let src = "var x = sqrt()\n";
        let (ws, uri) = ws_with(src);
        // Cursor inside the parens.
        let col = u32::try_from("var x = sqrt(".len()).unwrap();
        let help = handle(&ws, &uri, pos(0, col)).expect("signature");
        assert!(help.signatures[0].label.contains("builtin sqrt"));
        assert_eq!(help.signatures[0].parameters.as_ref().unwrap().len(), 1);
    }

    #[test]
    fn unknown_callee_still_returns_a_placeholder() {
        let src = "noSuchFunction(1, 2)\n";
        let (ws, uri) = ws_with(src);
        let col = u32::try_from("noSuchFunction(".len()).unwrap();
        let help = handle(&ws, &uri, pos(0, col)).expect("signature");
        assert!(help.signatures[0].label.contains("noSuchFunction"));
        assert_eq!(help.signatures[0].parameters.as_ref().unwrap().len(), 0);
    }

    #[test]
    fn cursor_outside_any_call_returns_none() {
        let src = "var x = 1\n";
        let (ws, uri) = ws_with(src);
        assert!(handle(&ws, &uri, pos(0, 5)).is_none());
    }
}
