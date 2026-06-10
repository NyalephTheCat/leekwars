//! v3 → v4 migration.
//!
//! ## What changes
//!
//! Builtin renames called out by `LeekFunctions.setMaxVersion(3,
//! "replacement")` in the upstream — the function is the last
//! release where the old name works, and `replacement` is the
//! new name. We handle three:
//!
//! | v3 builtin           | v4 builtin            | semantic shift                                |
//! |----------------------|-----------------------|-----------------------------------------------|
//! | `randFloat(a, b)`    | `randReal(a, b)`      | none (identical signature & range)            |
//! | `removeKey(map, k)`  | `mapRemove(map, k)`   | none (identical on maps)                      |
//! | `subArray(a, i, j)`  | `arraySlice(a, i, j+1)` | **end index changes from inclusive to exclusive** |
//!
//! The `subArray` case is the interesting one: a naïve textual
//! rename to `arraySlice` would silently drop the last element of
//! every slice. We compensate by replacing the third arg's text
//! with `(<orig>) + 1` so the migration is semantically faithful.
//!
//! For each rewrite we collect both a callee-token edit and (for
//! `subArray`) an arg-text edit; we then process first-class
//! references (`var f = randFloat`) in a second pass, skipping any
//! NameRef token we already touched. First-class references to
//! `subArray` cannot be transformed safely (we don't know each
//! call site's arity) — we emit a `DeprecatedFeature` diagnostic
//! and leave the source alone.

use std::collections::HashSet;

use leek_diagnostics::{Diagnostic, codes};
use leek_parser::ast::{AstNode, CallExpr, Expr, SourceFile};
use leek_parser::parse;
use leek_rewrite::EditSet;
use leek_span::{SourceId, Span};
use leek_syntax::{SyntaxKind, SyntaxNode, Version};

use super::boundary34;
use super::util::{ident_of_name_ref_expr, is_field_name_position, name_ref_ident, token_range};
use crate::MigrationPass;

pub struct V3ToV4;

/// Source: upstream `LeekFunctions.setMaxVersion(3, "<replacement>")`.
/// Only renames here are "pure"; `subArray` is handled specially
/// because its arg semantics change.
const RENAMES: &[(&str, &str)] = &[("randFloat", "randReal"), ("removeKey", "mapRemove")];

impl MigrationPass for V3ToV4 {
    fn name(&self) -> &'static str {
        "v3-to-v4"
    }
    fn from_version(&self) -> Version {
        Version::V3
    }
    fn to_version(&self) -> Version {
        Version::V4
    }

    fn collect_edits(
        &self,
        source: &str,
        source_id: SourceId,
        edits: &mut EditSet,
        diagnostics: &mut Vec<Diagnostic>,
    ) {
        let parsed = parse(source, source_id, Version::V3);
        let root = SyntaxNode::new_root(parsed.green);
        let Some(file) = SourceFile::cast(root.clone()) else {
            return;
        };

        // Pass A — call-expression rewrites. Track each NameRef
        // token range we consume so pass B doesn't rewrite it
        // again.
        let mut consumed_ident_ranges: HashSet<(u32, u32)> = HashSet::new();
        for node in file.syntax().descendants() {
            if node.kind() != SyntaxKind::CallExpr {
                continue;
            }
            let Some(call) = CallExpr::cast(node.clone()) else {
                continue;
            };
            let Some(callee_expr) = call.callee() else {
                continue;
            };
            let Some(ident) = ident_of_name_ref_expr(&callee_expr) else {
                continue;
            };
            let name = ident.text().to_string();

            if name == "subArray" {
                let Some(arg_list) = call.arg_list() else {
                    continue;
                };
                let args: Vec<Expr> = arg_list.args().collect();
                if args.len() != 3 {
                    // Unexpected arity — leave it alone and warn.
                    diagnostics.push(deprecated_diag(
                        &name,
                        "arraySlice",
                        leek_syntax::node_span(call.syntax(), source_id),
                        "non-3-arg `subArray` call — manual review required",
                    ));
                    continue;
                }
                // Rename the callee token.
                if edits
                    .replace_token(&ident, "arraySlice".to_string())
                    .is_ok()
                {
                    consumed_ident_ranges.insert(token_range(&ident));
                }
                // Compensate inclusive→exclusive end semantics by
                // bumping the third arg.
                let third = &args[2];
                let third_text = third.syntax().text().to_string();
                let new_third = format!("({third_text}) + 1");
                let _ = edits.replace_node(third.syntax(), new_third);
            } else if let Some((_, new_name)) = RENAMES.iter().find(|(old, _)| *old == name)
                && edits.replace_token(&ident, (*new_name).to_string()).is_ok()
            {
                consumed_ident_ranges.insert(token_range(&ident));
            }
        }

        // Pass B — first-class references that aren't directly the
        // callee of a CallExpr. `randFloat`/`removeKey` can be
        // renamed transparently; `subArray` cannot (call sites
        // could be downstream with the old end-semantics), so we
        // emit a diagnostic instead.
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
            let range = token_range(&ident);
            if consumed_ident_ranges.contains(&range) {
                continue;
            }
            let name = ident.text();
            if name == "subArray" {
                // First-class ref to subArray — can't safely rewrite.
                diagnostics.push(deprecated_diag(
                    name,
                    "arraySlice",
                    Span::new(source_id, range.0, range.1),
                    "first-class reference to `subArray`; \
                     end-index semantics differ — migrate by hand",
                ));
                continue;
            }
            if let Some((_, new)) = RENAMES.iter().find(|(old, _)| *old == name) {
                let _ = edits.replace_token(&ident, (*new).to_string());
            }
        }

        // Pass C — semantic drift at the 3/4 boundary (shared with
        // the downgrade direction, see `boundary34`): swap
        // (key, value) callback parameters, then flag everything
        // that has no faithful rewrite.
        boundary34::swap_callback_params(&file, source_id, edits, diagnostics);
        boundary34::flag_juggling_equality(&file, source_id, diagnostics);
        boundary34::flag_container_drift(&file, source_id, diagnostics);
    }
}

fn deprecated_diag(old: &str, new: &str, span: Span, note: &str) -> Diagnostic {
    Diagnostic::warning(
        codes::DEPRECATED_FEATURE,
        span,
        format!("`{old}` was renamed to `{new}` in v4"),
    )
    .with_note(note.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::run_pass;

    fn migrate(src: &str) -> String {
        run_pass(&V3ToV4, src, SourceId::new(1).unwrap()).text
    }

    #[test]
    fn renames_rand_float() {
        let out = migrate("var x = randFloat(0, 1)\n");
        assert!(out.contains("randReal(0, 1)"));
        assert!(!out.contains("randFloat"));
    }

    #[test]
    fn renames_remove_key() {
        let out = migrate("removeKey(m, 'a')\n");
        assert!(out.contains("mapRemove(m, "));
    }

    #[test]
    fn sub_array_renames_and_bumps_end() {
        let out = migrate("var s = subArray([1, 2, 3], 0, 2)\n");
        assert!(
            out.contains("arraySlice([1, 2, 3], 0, (2) + 1)"),
            "got: {out}"
        );
        assert!(!out.contains("subArray"));
    }

    #[test]
    fn sub_array_preserves_complex_end_arg() {
        // The end arg might be an expression — wrap it in parens
        // so the `+ 1` doesn't bind to the wrong subexpression.
        let out = migrate("var s = subArray(a, i, j - 1)\n");
        assert!(out.contains("arraySlice(a, i, (j - 1) + 1)"), "got: {out}");
    }

    #[test]
    fn renames_first_class_rand_float_reference() {
        let out = migrate("var f = randFloat\n");
        assert!(out.contains("var f = randReal"));
    }

    #[test]
    fn does_not_rewrite_first_class_sub_array_reference() {
        // First-class `subArray` ref: cannot fix safely, so we
        // leave it as-is and emit a warning.
        let result = run_pass(&V3ToV4, "var f = subArray\n", SourceId::new(1).unwrap());
        assert!(
            result.text.contains("var f = subArray"),
            "got: {}",
            result.text
        );
        assert!(
            result
                .diagnostics
                .iter()
                .any(|d| d.message.contains("subArray"))
        );
    }

    #[test]
    fn does_not_rename_field_access() {
        // `obj.randFloat` names a user member, not the builtin.
        let out = migrate("obj.randFloat()\n");
        assert!(out.contains("obj.randFloat"));
    }

    #[test]
    fn preserves_comments_and_layout() {
        let src = "// generate a random scalar\nvar x = randFloat(0, 1) // inline\nreturn x\n";
        let out = migrate(src);
        assert!(out.contains("// generate a random scalar"));
        assert!(out.contains("// inline"));
        assert!(out.contains("var x = randReal(0, 1)"));
    }

    #[test]
    fn bumps_version_pragma() {
        let out = migrate("// @version:3\nvar x = subArray([1], 0, 0)\n");
        assert!(out.starts_with("// @version:4\n"));
    }

    #[test]
    fn swaps_two_param_callback_names() {
        // v3 calls the callback with (key, value); v4 with
        // (value, key). Swapping the names keeps the body's meaning.
        let out = migrate("return arrayMap([1, 2], (k, v) => v * 2)\n");
        assert!(out.contains("(v, k) => v * 2"), "got: {out}");
    }

    #[test]
    fn one_param_callback_untouched() {
        let out = migrate("return arrayMap([1, 2], v => v * 2)\n");
        assert!(out.contains("v => v * 2"), "got: {out}");
    }

    #[test]
    fn non_inline_callback_is_flagged() {
        let res = run_pass(
            &V3ToV4,
            "function f(k, v) { return v }\nreturn arrayMap([1], f)\n",
            SourceId::new(1).unwrap(),
        );
        assert!(
            res.diagnostics
                .iter()
                .any(|d| d.message.contains("(key, value)")),
            "missing flag: {:?}",
            res.diagnostics
        );
    }

    #[test]
    fn bool_literal_equality_is_flagged() {
        let res = run_pass(&V3ToV4, "return 1 == true\n", SourceId::new(1).unwrap());
        assert!(res.text.contains("1 == true"), "got: {}", res.text);
        assert!(
            res.diagnostics
                .iter()
                .any(|d| d.message.contains("boolean literal")),
            "missing flag: {:?}",
            res.diagnostics
        );
    }

    #[test]
    fn index_assignment_is_flagged() {
        let res = run_pass(
            &V3ToV4,
            "var a = []\na[3] = 1\nreturn a\n",
            SourceId::new(1).unwrap(),
        );
        assert!(
            res.diagnostics
                .iter()
                .any(|d| d.message.contains("out-of-range index")),
            "missing flag: {:?}",
            res.diagnostics
        );
    }

    #[test]
    fn map_literal_base_index_assignment_not_flagged() {
        let res = run_pass(
            &V3ToV4,
            "var m = [:]\nm[3] = 1\nreturn m\n",
            SourceId::new(1).unwrap(),
        );
        assert!(
            !res.diagnostics
                .iter()
                .any(|d| d.message.contains("out-of-range index")),
            "spurious flag: {:?}",
            res.diagnostics
        );
    }

    #[test]
    fn mixed_literal_equality_is_flagged() {
        let res = run_pass(&V3ToV4, "return 0 == '0'\n", SourceId::new(1).unwrap());
        assert!(res.text.contains("0 == '0'"), "got: {}", res.text);
        assert!(
            res.diagnostics
                .iter()
                .any(|d| d.message.contains("different types")),
            "missing flag: {:?}",
            res.diagnostics
        );
    }

    #[test]
    fn same_class_literal_equality_not_flagged() {
        // int vs real is fine (`1 == 1.0` is true in both versions).
        let res = run_pass(&V3ToV4, "return 1 == 1.0\n", SourceId::new(1).unwrap());
        assert!(
            !res.diagnostics
                .iter()
                .any(|d| d.message.contains("different types")),
            "spurious flag: {:?}",
            res.diagnostics
        );
    }

    #[test]
    fn self_indexed_map_assignment_is_flagged() {
        // Even with a map-literal base, `a[a] = 1` uses the container
        // as its own key — that drifts, so the map skip must not win.
        let res = run_pass(
            &V3ToV4,
            "var a = [:]\na[a] = 1\nreturn a\n",
            SourceId::new(1).unwrap(),
        );
        assert!(
            res.diagnostics
                .iter()
                .any(|d| d.message.contains("out-of-range index")),
            "missing flag: {:?}",
            res.diagnostics
        );
    }

    #[test]
    fn real_key_remove_is_flagged() {
        let res = run_pass(
            &V3ToV4,
            "var m = [:]\nremoveKey(m, 12.12)\nreturn m\n",
            SourceId::new(1).unwrap(),
        );
        assert!(
            res.diagnostics
                .iter()
                .any(|d| d.message.contains("real-number key")),
            "missing flag: {:?}",
            res.diagnostics
        );
    }

    #[test]
    fn string_of_container_literal_is_flagged() {
        let res = run_pass(
            &V3ToV4,
            "return string([['a']])\n",
            SourceId::new(1).unwrap(),
        );
        assert!(
            res.diagnostics
                .iter()
                .any(|d| d.message.contains("without quotes")),
            "missing flag: {:?}",
            res.diagnostics
        );
    }

    #[test]
    fn array_filter_keys_are_flagged() {
        let res = run_pass(
            &V3ToV4,
            "return arrayFilter([1, 2, 3], x => x > 1)\n",
            SourceId::new(1).unwrap(),
        );
        assert!(
            res.diagnostics
                .iter()
                .any(|d| d.message.contains("arrayFilter")),
            "missing flag: {:?}",
            res.diagnostics
        );
    }

    #[test]
    fn json_decode_is_flagged() {
        let res = run_pass(
            &V3ToV4,
            "return jsonDecode('[1]')\n",
            SourceId::new(1).unwrap(),
        );
        assert!(
            res.diagnostics
                .iter()
                .any(|d| d.message.contains("jsonDecode")),
            "missing flag: {:?}",
            res.diagnostics
        );
    }
}
