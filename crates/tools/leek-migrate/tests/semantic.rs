//! Semantic round-trip tests.
//!
//! For each migration scenario we:
//!  1. Run the ORIGINAL source under `from_version`.
//!  2. Migrate it to `to_version`.
//!  3. Run the MIGRATED source under `to_version`.
//!  4. Assert the two `Value`s compare equal under
//!     [`Value::loose_eq`].
//!
//! That's the bar the user set: "make sure we get the same result
//! as the original value, not what the same test gives out as the
//! output version target with the same code." A textual rename
//! that changes runtime behaviour fails here.

use std::sync::Arc;

use leek_diagnostics::Severity;
use leek_hir::pipeline::HirArtifact;
use leek_migrate::migrate_text;
use leek_pipeline::Input;
use leek_recipes::{RecipeParams, Target};
use leek_runtime::Value;
use leek_span::SourceId;
use leek_syntax::Version;

fn id() -> SourceId {
    SourceId::new(1).unwrap()
}

fn version_num(v: Version) -> u8 {
    match v {
        Version::V1 => 1,
        Version::V2 => 2,
        Version::V3 => 3,
        Version::V4 => 4,
    }
}

/// Full versioned frontend (parser + resolver + HIR lowering) and a
/// native JIT run — the same path `examples/corpus_verify.rs` uses.
/// Panics loudly on compile errors so a malformed fixture surfaces
/// fast rather than silently returning `null`.
fn run(src: &str, version: Version) -> Value {
    let input = Input {
        source: id(),
        text: src.to_string().into(),
        version_byte: version_num(version),
        strict: false,
        flags: leek_pipeline::FeatureFlags::from_env(),
    };
    let pipeline =
        leek_recipes::pipeline(Target::Hir, &RecipeParams::permissive()).expect("recipe");
    let outcome = pipeline.run(input);
    let fatal: Vec<_> = outcome
        .diagnostics()
        .iter()
        .filter(|d| d.severity == Severity::Error)
        .cloned()
        .collect();
    assert!(
        fatal.is_empty(),
        "compile errors in fixture under {version:?}:\n{src}\n{fatal:?}",
    );
    let hir = outcome
        .get::<HirArtifact>()
        .map(|a| Arc::clone(&a.0))
        .expect("hir artifact");
    let opts = leek_backend_native::NativeOptions::release()
        .with_lang(version_num(version), false)
        .with_op_limit(1_000_000);
    match leek_backend_native::run(&hir, &opts) {
        Ok(v) => v,
        Err(e) => panic!("native run failed under {version:?}: {e:?}\nsource:\n{src}"),
    }
}

/// The core assertion: original under `from`, migrated under `to`,
/// values must `loose_eq`.
fn assert_round_trip(src: &str, from: Version, to: Version) {
    let migrated = migrate_text(src, id(), from, to).text;
    let before = run(src, from);
    let after = run(&migrated, to);
    assert!(
        before.loose_eq(&after),
        "semantic drift: {from:?} -> {to:?}\n\
         original source:\n{src}\n\
         original value: {before:?}\n\
         migrated source:\n{migrated}\n\
         migrated value: {after:?}",
    );
}

// ─── v1 → v2 : `^=` ─────────────────────────────────────────────────

#[test]
fn v1_to_v2_caret_assign_power() {
    // v1: `^=` is power-assign. 5 ^= 2  →  25.
    // v2+: same operation written `**=`.
    assert_round_trip("var x = 5\nx ^= 2\nreturn x\n", Version::V1, Version::V2);
}

#[test]
fn v1_to_v2_caret_assign_with_real_base() {
    assert_round_trip("var x = 2.5\nx ^= 3\nreturn x\n", Version::V1, Version::V2);
}

#[test]
fn v1_to_v2_caret_assign_negative_exponent_via_chain() {
    // Two `^=` in a row.  ( (3 ^= 2) ^= 2 )  ==  (9 ^= 2)  ==  81.
    assert_round_trip(
        "var x = 3\nx ^= 2\nx ^= 2\nreturn x\n",
        Version::V1,
        Version::V2,
    );
}

// ─── v1 → v2 : lexer quirks & constant folds ───────────────────────

#[test]
fn v1_to_v2_short_comment_rewrites() {
    // `/*/` is a complete comment at v1; at v2 it would swallow the
    // rest of the file. Must become `/**/` so `return 1` survives.
    assert_round_trip("/*/ return 1\n", Version::V1, Version::V2);
}

#[test]
fn v1_to_v2_escaped_delimiter_keeps_content() {
    // v1 keeps the backslash of `\"` in the content (8 chars); the
    // migrated spelling must preserve that at v2.
    assert_round_trip("return length(\"abc\\\"def\")\n", Version::V1, Version::V2);
}

#[test]
fn v1_to_v2_constant_division_by_zero_folds() {
    // 1 / 0 is null at v1 but ∞ at v2+ — folded to null.
    assert_round_trip("return 1 / 0\n", Version::V1, Version::V2);
    assert_round_trip("return 8 / null\n", Version::V1, Version::V2);
}

// ─── v2 → v3 : keyword case ─────────────────────────────────────────

#[test]
fn v2_to_v3_is_noop_on_clean_source() {
    assert_round_trip("var x = 7\nreturn x + 1\n", Version::V2, Version::V3);
}

#[test]
fn v2_to_v3_lowercases_keywords() {
    // v1/v2 keywords are case-insensitive; v3+ is case-sensitive.
    assert_round_trip("VAR x = 2\nReturn TRUE\n", Version::V2, Version::V3);
}

#[test]
fn v2_to_v3_mis_cased_null_value() {
    // `Null` at v2 is the null keyword; at v3 it would become a
    // fresh (unknown) variable — silent drift without the rewrite.
    assert_round_trip("return Null\n", Version::V2, Version::V3);
}

// ─── v3 → v4 : renames ──────────────────────────────────────────────

#[test]
fn v3_to_v4_rand_float_to_rand_real() {
    // The interp is deterministic for randFloat/randReal — it
    // returns the lower bound — so we can compare values exactly.
    assert_round_trip(
        "var n = randFloat(7, 12)\nreturn n\n",
        Version::V3,
        Version::V4,
    );
}

#[test]
fn v3_to_v4_remove_key_on_map_round_trips() {
    // `removeKey` and `mapRemove` are semantically equivalent on
    // maps. We trigger the map arm by indexing with a string key.
    let src = "\
var m = [:]\n\
m['a'] = 1\n\
m['b'] = 2\n\
removeKey(m, 'a')\n\
return m\n\
";
    assert_round_trip(src, Version::V3, Version::V4);
}

#[test]
fn v3_to_v4_sub_array_round_trips() {
    // `subArray(arr, 0, 2)` is INCLUSIVE on both ends — returns
    // three elements. A naive textual rename to `arraySlice(arr,
    // 0, 2)` is EXCLUSIVE on the end and returns only two
    // elements. The migration pass must compensate (e.g. bump the
    // third arg by 1) for the round-trip to hold.
    let src = "\
var head = subArray([10, 20, 30, 40, 50], 0, 2)\n\
return head\n\
";
    assert_round_trip(src, Version::V3, Version::V4);
}

#[test]
fn v3_to_v4_sub_array_mid_range() {
    let src = "\
var mid = subArray([10, 20, 30, 40, 50], 1, 3)\n\
return mid\n\
";
    assert_round_trip(src, Version::V3, Version::V4);
}

#[test]
fn v3_to_v4_first_class_ref_can_be_called() {
    // First-class reference to a builtin should still be callable
    // post-rename.
    let src = "\
var f = randFloat\n\
return f(7, 9)\n\
";
    assert_round_trip(src, Version::V3, Version::V4);
}

#[test]
fn v3_to_v4_callback_param_order_swaps() {
    // v3 passes (key, value); v4 passes (value, key). The swap keeps
    // each body name bound to the same data.
    assert_round_trip(
        "return arrayMap([7, 8], (k, v) => v + k)\n",
        Version::V3,
        Version::V4,
    );
}

// ─── full chain v1 → v4 ─────────────────────────────────────────────

#[test]
fn v1_to_v4_full_chain_preserves_semantics() {
    let src = "\
var x = 3\n\
x ^= 2\n\
var n = randFloat(4, 8)\n\
var head = subArray([1, 2, 3, 4], 1, 2)\n\
return [x, n, head]\n\
";
    assert_round_trip(src, Version::V1, Version::V4);
}

// ─── downgrades ────────────────────────────────────────────────────

#[test]
fn v4_to_v3_array_slice_to_sub_array() {
    // `arraySlice(arr, 0, 3)` returns [arr[0], arr[1], arr[2]]
    // `subArray(arr, 0, 2)` (= 3 - 1) returns the same three.
    let src = "var head = arraySlice([10, 20, 30, 40, 50], 0, 3)\nreturn head\n";
    assert_round_trip(src, Version::V4, Version::V3);
}

#[test]
fn v4_to_v3_rand_real_to_rand_float() {
    assert_round_trip(
        "var n = randReal(7, 12)\nreturn n\n",
        Version::V4,
        Version::V3,
    );
}

#[test]
fn v4_to_v3_map_remove_on_map() {
    let src = "\
var m = [:]\n\
m['a'] = 1\n\
m['b'] = 2\n\
mapRemove(m, 'a')\n\
return m\n\
";
    assert_round_trip(src, Version::V4, Version::V3);
}

#[test]
fn v4_to_v3_bool_literal_equality_strictifies() {
    // v4's `x == true` is plain false for non-bool x; v3's `==`
    // type-juggles (`1 == true` is true there), so the downgrade
    // must emit `===` to stay faithful.
    assert_round_trip("var x = 1\nreturn x == true\n", Version::V4, Version::V3);
    assert_round_trip("var x = 1\nreturn x != false\n", Version::V4, Version::V3);
}

#[test]
fn v4_to_v3_callback_param_order_swaps() {
    assert_round_trip(
        "return arrayMap([7, 8], (v, k) => v * 2 + k)\n",
        Version::V4,
        Version::V3,
    );
}

#[test]
fn v2_to_v1_star_star_eq_to_caret_eq() {
    // `**=` is power-assign in v2+. After downgrade, `^=` is
    // power-assign in v1 — same value.
    assert_round_trip("var x = 4\nx **= 3\nreturn x\n", Version::V2, Version::V1);
}

#[test]
fn v2_to_v1_caret_eq_xor_expands_to_long_form() {
    // `^=` is xor-assign in v2. After downgrade it must become
    // `x = x ^ (rhs)` in v1, because `^=` in v1 means power-
    // assign — a textual no-op would silently flip semantics.
    assert_round_trip("var x = 5\nx ^= 3\nreturn x\n", Version::V2, Version::V1);
}

#[test]
fn v2_to_v1_mixed_power_and_xor_assigns() {
    // Both forms in one file — the pass must apply both edits
    // against the original CST so they don't interfere.
    assert_round_trip(
        "\
var a = 5\n\
a **= 2\n\
var b = 5\n\
b ^= 3\n\
return [a, b]\n\
",
        Version::V2,
        Version::V1,
    );
}

#[test]
fn v4_to_v1_full_downgrade_chain() {
    // The mother test: a v4 source that exercises every rewrite
    // we make on the way down to v1.
    let src = "\
var x = 4\n\
x **= 2\n\
var y = 7\n\
y ^= 3\n\
var n = randReal(5, 12)\n\
var head = arraySlice([10, 20, 30, 40], 0, 3)\n\
return [x, y, n, head]\n\
";
    assert_round_trip(src, Version::V4, Version::V1);
}

#[test]
fn v4_to_v1_map_remove_works_on_v1_unified_collection() {
    // v1 treats map and array as the same heterogeneous
    // container. After downgrade, `mapRemove(m, 'a')` becomes
    // `removeKey(m, 'a')`, which the v1 interpreter handles
    // against that unified type.
    let src = "\
var m = [:]\n\
m['a'] = 1\n\
m['b'] = 2\n\
m['c'] = 3\n\
mapRemove(m, 'b')\n\
return m\n\
";
    assert_round_trip(src, Version::V4, Version::V1);
}

#[test]
fn v4_to_v1_array_literal_survives() {
    // Plain array literal — should round-trip with no changes
    // beyond the pragma stamp. v1's unified type happily holds
    // these.
    let src = "\
var arr = [1, 2, 3, 4, 5]\n\
return arr\n\
";
    assert_round_trip(src, Version::V4, Version::V1);
}

#[test]
fn round_trip_v3_to_v4_to_v3_callback_swap_is_involutive() {
    // The (key, value) ↔ (value, key) swap must be its own inverse.
    let src = "return arrayMap([7, 8], (k, v) => v + k)\n";
    let up = migrate_text(src, id(), Version::V3, Version::V4).text;
    let down = migrate_text(&up, id(), Version::V4, Version::V3).text;
    assert!(
        down.contains("(k, v) => v + k"),
        "swap not involutive:\nup:\n{up}\ndown:\n{down}"
    );
    let orig_val = run(src, Version::V3);
    let down_val = run(&down, Version::V3);
    assert!(orig_val.loose_eq(&down_val));
}

#[test]
fn round_trip_v1_to_v4_to_v1_is_stable() {
    // Migrate v1 source forward to v4, then back to v1; the
    // final runtime value must still match the original. Catches
    // any rewrite asymmetry where upgrade and downgrade aren't
    // exact inverses of each other on the runtime side.
    let original_src = "\
var x = 3\n\
x ^= 2\n\
var head = subArray([10, 20, 30, 40], 0, 2)\n\
return [x, head]\n\
";
    let up = leek_migrate::migrate_text(original_src, id(), Version::V1, Version::V4).text;
    let down = leek_migrate::migrate_text(&up, id(), Version::V4, Version::V1).text;

    let orig_val = run(original_src, Version::V1);
    let down_val = run(&down, Version::V1);
    assert!(
        orig_val.loose_eq(&down_val),
        "round-trip drift:\noriginal:\n{original_src}\n→ v4:\n{up}\n→ back to v1:\n{down}\n\
         original value: {orig_val:?}\n\
         round-tripped value: {down_val:?}",
    );
}
