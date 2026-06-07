//! v4 → v3 downgrade.
//!
//! Inverts the [`super::v3_to_v4`] table:
//!
//! | v4                       | v3                              |
//! |--------------------------|---------------------------------|
//! | `randReal(a, b)`         | `randFloat(a, b)`               |
//! | `mapRemove(map, k)`      | `removeKey(map, k)`             |
//! | `arraySlice(a, i, j)`    | `subArray(a, i, (j) - 1)`       |
//!
//! ### `arraySlice` arities
//!
//! v4's `arraySlice` accepts 1-4 args (`slice`, `slice(start)`,
//! `slice(start, end)`, `slice(start, end, step)`). v3's
//! `subArray` only takes the 3-arg `(arr, from, to)` form. We
//! downgrade the 3-arg case in place and emit a
//! `DeprecatedFeature` diagnostic for everything else (1-arg, 2-
//! arg, 4-arg) — those would need synthesized `count(arr)` calls
//! and we don't want to invent code silently.
//!
//! `removeKey` in the v1-v3 interpreter handles BOTH maps and
//! arrays — by integer index for arrays, by key for maps. That's
//! how v1's unified array/map type still works after downgrade:
//! the rename produces source that runs equivalently against the
//! older interpreter.

use std::collections::HashSet;

use leek_diagnostics::{Diagnostic, codes};
use leek_parser::ast::{AstNode, CallExpr, Expr, SourceFile};
use leek_parser::parse;
use leek_rewrite::EditSet;
use leek_span::{SourceId, Span};
use leek_syntax::language::NodeOrToken;
use leek_syntax::{SyntaxKind, SyntaxNode, Version};

use crate::MigrationPass;

pub struct V4ToV3;

/// Pure renames — no arg fix-ups.
const RENAMES: &[(&str, &str)] = &[("randReal", "randFloat"), ("mapRemove", "removeKey")];

impl MigrationPass for V4ToV3 {
    fn name(&self) -> &'static str {
        "v4-to-v3"
    }
    fn from_version(&self) -> Version {
        Version::V4
    }
    fn to_version(&self) -> Version {
        Version::V3
    }

    fn collect_edits(
        &self,
        source: &str,
        source_id: SourceId,
        edits: &mut EditSet,
        diagnostics: &mut Vec<Diagnostic>,
    ) {
        let parsed = parse(source, source_id, Version::V4);
        let root = SyntaxNode::new_root(parsed.green);
        let Some(file) = SourceFile::cast(root.clone()) else {
            return;
        };

        let mut consumed: HashSet<(u32, u32)> = HashSet::new();

        // Pass A: call-expression rewrites (arraySlice + arity
        // gating, then pure renames at call sites).
        for node in file.syntax().descendants() {
            if node.kind() != SyntaxKind::CallExpr {
                continue;
            }
            let Some(call) = CallExpr::cast(node.clone()) else {
                continue;
            };
            let Some(callee) = call.callee() else {
                continue;
            };
            let Some(ident) = ident_of_name_ref_expr(&callee) else {
                continue;
            };
            let name = ident.text().to_string();

            if name == "arraySlice" {
                let Some(arg_list) = call.arg_list() else {
                    continue;
                };
                let args: Vec<Expr> = arg_list.args().collect();
                match args.len() {
                    3 => {
                        // The canonical case — rename and bump the
                        // end index DOWN by one to land back on
                        // inclusive semantics.
                        if edits.replace_token(&ident, "subArray".to_string()).is_ok() {
                            consumed.insert(range(&ident));
                        }
                        let end = &args[2];
                        let end_text = end.syntax().text().to_string();
                        let _ = edits.replace_node(end.syntax(), format!("({end_text}) - 1"));
                    }
                    1 | 2 | 4 => {
                        diagnostics.push(deprecated_diag(
                            &name,
                            "subArray",
                            leek_syntax::node_span(call.syntax(), source_id),
                            "v3 `subArray` only accepts (arr, from, to); \
                             this call's arity needs manual rewrite",
                        ));
                    }
                    _ => {
                        // 0-arg or 5+-arg — neither is valid in either
                        // version; leave it alone.
                    }
                }
            } else if let Some((_, new_name)) = RENAMES.iter().find(|(old, _)| *old == name)
                && edits.replace_token(&ident, (*new_name).to_string()).is_ok() {
                    consumed.insert(range(&ident));
                }
        }

        // Pass B: first-class refs. `randReal`/`mapRemove` rename
        // transparently. `arraySlice` first-class refs can't be
        // safely downgraded (call sites might rely on the
        // exclusive end, on 1/2/4 arity, or on the step arg).
        for node in file.syntax().descendants() {
            if node.kind() != SyntaxKind::NameRef {
                continue;
            }
            if is_field_name_position(&node) {
                continue;
            }
            let Some(ident) = node
                .children_with_tokens()
                .filter_map(leek_syntax::language::NodeOrToken::into_token)
                .find(|t| t.kind() == SyntaxKind::Ident)
            else {
                continue;
            };
            let r = range(&ident);
            if consumed.contains(&r) {
                continue;
            }
            let name = ident.text();
            if name == "arraySlice" {
                diagnostics.push(deprecated_diag(
                    name,
                    "subArray",
                    Span::new(source_id, r.0, r.1),
                    "first-class reference to `arraySlice`; end-index \
                     semantics differ — downgrade by hand",
                ));
                continue;
            }
            if let Some((_, new)) = RENAMES.iter().find(|(old, _)| *old == name) {
                let _ = edits.replace_token(&ident, (*new).to_string());
            }
        }
    }
}

fn ident_of_name_ref_expr(expr: &Expr) -> Option<leek_syntax::SyntaxToken> {
    let Expr::Name(name_ref) = expr else {
        return None;
    };
    name_ref
        .syntax()
        .children_with_tokens()
        .filter_map(leek_syntax::language::NodeOrToken::into_token)
        .find(|t| t.kind() == SyntaxKind::Ident)
}

fn is_field_name_position(node: &SyntaxNode) -> bool {
    let Some(parent) = node.parent() else {
        return false;
    };
    if parent.kind() != SyntaxKind::FieldExpr {
        return false;
    }
    let mut seen_dot = false;
    for el in parent.children_with_tokens() {
        match el {
            NodeOrToken::Token(t) if t.kind() == SyntaxKind::Dot => seen_dot = true,
            NodeOrToken::Node(n) if n == *node => return seen_dot,
            _ => {}
        }
    }
    false
}

fn range(tok: &leek_syntax::SyntaxToken) -> (u32, u32) {
    let r = tok.text_range();
    (u32::from(r.start()), u32::from(r.end()))
}


fn deprecated_diag(old: &str, new: &str, span: Span, note: &str) -> Diagnostic {
    Diagnostic::warning(
        codes::DEPRECATED_FEATURE,
        span,
        format!("`{old}` has no direct equivalent in v3; would use `{new}`"),
    )
    .with_note(note.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::run_pass;

    fn migrate(src: &str) -> String {
        run_pass(&V4ToV3, src, SourceId::new(1).unwrap()).text
    }

    #[test]
    fn renames_rand_real() {
        let out = migrate("var x = randReal(0, 1)\n");
        assert!(out.contains("randFloat(0, 1)"), "got: {out}");
        assert!(!out.contains("randReal"));
    }

    #[test]
    fn renames_map_remove() {
        let out = migrate("mapRemove(m, 'a')\n");
        assert!(out.contains("removeKey(m, "));
    }

    #[test]
    fn array_slice_3arg_becomes_sub_array_with_end_minus_one() {
        let out = migrate("var s = arraySlice([1, 2, 3], 0, 3)\n");
        assert!(
            out.contains("subArray([1, 2, 3], 0, (3) - 1)"),
            "got: {out}"
        );
        assert!(!out.contains("arraySlice"));
    }

    #[test]
    fn array_slice_2arg_warns_and_leaves_source() {
        let res = run_pass(
            &V4ToV3,
            "var s = arraySlice(a, 1)\n",
            SourceId::new(1).unwrap(),
        );
        assert!(res.text.contains("arraySlice(a, 1)"));
        assert!(
            res.diagnostics
                .iter()
                .any(|d| d.message.contains("arraySlice"))
        );
    }

    #[test]
    fn array_slice_4arg_with_step_warns() {
        let res = run_pass(
            &V4ToV3,
            "var s = arraySlice(a, 0, 9, 2)\n",
            SourceId::new(1).unwrap(),
        );
        assert!(res.text.contains("arraySlice(a, 0, 9, 2)"));
        assert!(
            res.diagnostics
                .iter()
                .any(|d| d.message.contains("arraySlice"))
        );
    }

    #[test]
    fn first_class_array_slice_ref_is_not_rewritten() {
        let res = run_pass(&V4ToV3, "var f = arraySlice\n", SourceId::new(1).unwrap());
        assert!(res.text.contains("var f = arraySlice"));
        assert!(
            res.diagnostics
                .iter()
                .any(|d| d.message.contains("arraySlice"))
        );
    }

    #[test]
    fn bumps_version_pragma() {
        let out = migrate("// @version:4\nvar s = arraySlice([1], 0, 1)\n");
        assert!(out.starts_with("// @version:3\n"), "got: {out}");
    }
}
