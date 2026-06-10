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
use leek_syntax::{SyntaxKind, SyntaxNode, Version};

use super::boundary34;
use super::util::{
    ident_of_name_ref_expr, is_field_name_position, is_null_literal, name_ref_ident,
    token_range as range,
};
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
                    3 if args.iter().any(is_null_literal) => {
                        // `arraySlice` substitutes a default for a
                        // null argument; `subArray` doesn't (and our
                        // `(null) - 1` end-compensation would turn a
                        // default into -1). No faithful rewrite.
                        diagnostics.push(deprecated_diag(
                            &name,
                            "subArray",
                            leek_syntax::node_span(call.syntax(), source_id),
                            "a null argument takes a default in `arraySlice` but not in \
                             `subArray` — spell the bounds explicitly, then re-migrate",
                        ));
                    }
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
                && edits.replace_token(&ident, (*new_name).to_string()).is_ok()
            {
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
            let Some(ident) = name_ref_ident(&node) else {
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

        // Pass C — semantic drift at the 3/4 boundary (shared with
        // the upgrade direction, see `boundary34`). v4's bool-vs-
        // non-bool `==` is plain false, exactly like v3's `===`, so
        // bool-literal comparisons strictify faithfully; the
        // callback-parameter swap is its own inverse.
        boundary34::strictify_juggling_equality(&file, edits);
        boundary34::swap_callback_params(&file, source_id, edits, diagnostics);
        boundary34::flag_container_drift(&file, source_id, diagnostics);
    }
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

    #[test]
    fn bool_literal_equality_strictifies() {
        // v4's `x == true` is false for non-bool x — exactly v3's
        // `===`, so the rewrite preserves the v4 behavior.
        let out = migrate("var x = 1\nreturn x == true\n");
        assert!(out.contains("x === true"), "got: {out}");
        let out = migrate("var x = 1\nreturn x != false\n");
        assert!(out.contains("x !== false"), "got: {out}");
    }

    #[test]
    fn non_bool_equality_untouched() {
        let out = migrate("var x = 1\nreturn x == 1\n");
        assert!(out.contains("x == 1"), "got: {out}");
    }

    #[test]
    fn swaps_two_param_callback_names() {
        // v4 calls the callback with (value, key); v3 with
        // (key, value). The swap is its own inverse.
        let out = migrate("return arrayMap([1, 2], (v, k) => v * 2)\n");
        assert!(out.contains("(k, v) => v * 2"), "got: {out}");
    }

    #[test]
    fn mixed_literal_equality_strictifies() {
        // v4's `0 == '0'` is plain false — exactly v3's `===`.
        let out = migrate("return 0 == '0'\n");
        assert!(out.contains("0 === '0'"), "got: {out}");
        let out = migrate("return 0 != []\n");
        assert!(out.contains("0 !== []"), "got: {out}");
    }

    #[test]
    fn same_class_literal_equality_untouched() {
        let out = migrate("return 1 == 1.0\n");
        assert!(out.contains("1 == 1.0"), "got: {out}");
    }

    #[test]
    fn array_slice_3arg_with_null_is_flagged_not_rewritten() {
        // `arraySlice(a, x, null)` defaults the end bound; our
        // `(null) - 1` compensation would produce -1 instead.
        let res = run_pass(
            &V4ToV3,
            "var a = [1, 2, 3]\nreturn arraySlice(a, 1, null)\n",
            SourceId::new(1).unwrap(),
        );
        assert!(
            res.text.contains("arraySlice(a, 1, null)"),
            "source must be untouched: {}",
            res.text
        );
        assert!(
            res.diagnostics
                .iter()
                .any(|d| d.message.contains("arraySlice")),
            "missing flag: {:?}",
            res.diagnostics
        );
    }

    #[test]
    fn search_call_is_flagged() {
        let res = run_pass(
            &V4ToV3,
            "return search([1, 2], 3)\n",
            SourceId::new(1).unwrap(),
        );
        assert!(
            res.diagnostics.iter().any(|d| d.message.contains("search")),
            "missing flag: {:?}",
            res.diagnostics
        );
    }
}
