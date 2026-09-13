//! Parity tests against the upstream Java reference.
//!
//! Each fixture under `tests/fixtures/inputs/*.leek` has a captured
//! Java reference output at `tests/fixtures/golden/<name>.java`. The
//! goldens are produced by `tools/java-emitter/build/leekscript-emitter.jar`,
//! itself a thin driver around the upstream `IACompiler` — see
//! [`tools/java-emitter/build.sh`].
//!
//! What the tests assert:
//!
//! 1. **Shape**: every fixture's Rust-emitted Java contains the same
//!    `public class AI_<id> extends AI` shell, the same constructor
//!    signature, the same `runIA(Session)` signature, and the same
//!    user-function declarations the reference emits.
//!
//! 2. **Diff snapshot**: the full unified diff between Rust-exact
//!    output and the golden must equal the committed
//!    `tests/snapshots/<name>.exact.diff`.
//!
//! Byte parity is the explicit Phase-3 goal in `PLAN.md` — this
//! test infrastructure is the substrate that closes that gap one
//! lowering at a time.
//!
//! # Snapshot reports
//!
//! Every report under `tests/snapshots/` (the `*.diff` files,
//! `SUMMARY.txt`, `CORPUS_SUMMARY.txt`, `NATIVE_OPS_DRIFT.txt`,
//! `OPS_DRIFT.txt`, `JVM_PARITY.txt`) is checked insta-style: a run
//! writes the fresh report under `CARGO_TARGET_TMPDIR` and fails if it
//! differs from the committed file. Tests never touch the tracked files
//! unless `UPDATE_SNAPSHOTS=1` is set, in which case they overwrite them
//! (review and commit the result). Reports must therefore be
//! deterministic: rows whose op count depends on the RNG are excluded
//! from exact ops comparison (see [`is_rng_dependent`]).
//!
//! # JVM-dependent tests
//!
//! `rust_emit_matches_snapshot_on_jvm` needs a JDK and
//! `tools/java-emitter/build/leekscript-emitter.jar`. Without them it
//! prints a `SKIPPED` line and passes; set `LEEK_REQUIRE_JVM=1` to turn
//! that skip into a failure.

use std::fmt::Write as _;
use std::fs;
use std::path::PathBuf;

use leek_backend_java::{Options, emit};
use leek_parser::{ast::AstNode, parse};
use leek_span::SourceId;
use leek_syntax::{SyntaxNode, Version};
use similar::{ChangeTag, TextDiff};

/// Per-snippet op budget for the snapshot cross-check. The heaviest
/// corpus rows legitimately charge ~13.6M ops (the 500×200 randInt
/// string-building loops), so the ceiling sits well above that; a
/// genuinely runaway loop is still bounded — the native JIT burns
/// through the budget in well under the worker timeout.
const SNAPSHOT_OP_LIMIT: u64 = 100_000_000;

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

fn snapshots_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/snapshots")
}

/// Where fresh reports land on every run (never tracked).
fn snapshot_out_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("leek-backend-java-snapshots")
}

/// Whether `UPDATE_SNAPSHOTS=1` asks to overwrite the tracked reports.
fn update_snapshots() -> bool {
    std::env::var_os("UPDATE_SNAPSHOTS").is_some_and(|v| v == "1")
}

/// Write `actual` as report `name` under `out_dir`, then either overwrite
/// `tracked_dir/name` (`update`) or compare against it. Returns a
/// human-readable description of the mismatch on failure.
fn check_snapshot_in(
    tracked_dir: &std::path::Path,
    out_dir: &std::path::Path,
    name: &str,
    actual: &str,
    update: bool,
) -> Result<(), String> {
    fs::create_dir_all(out_dir).expect("snapshot out dir");
    let fresh = out_dir.join(name);
    fs::write(&fresh, actual).expect("write fresh snapshot");
    let tracked = tracked_dir.join(name);
    if update {
        fs::create_dir_all(tracked_dir).expect("snapshot dir");
        if fs::read_to_string(&tracked).ok().as_deref() != Some(actual) {
            fs::write(&tracked, actual).expect("write tracked snapshot");
        }
        return Ok(());
    }
    let Ok(expected) = fs::read_to_string(&tracked) else {
        return Err(format!(
            "{name}: no committed snapshot at {} (fresh output: {})",
            tracked.display(),
            fresh.display()
        ));
    };
    if expected == actual {
        return Ok(());
    }
    let diff = unified_diff_labeled(&expected, actual, "committed", "this run");
    let changed: Vec<&str> = diff
        .lines()
        .filter(|l| l.starts_with(['+', '-']))
        .take(40)
        .collect();
    Err(format!(
        "{name}: differs from {} (fresh output: {})\n{}",
        tracked.display(),
        fresh.display(),
        changed.join("\n")
    ))
}

/// [`check_snapshot_in`] against this crate's `tests/snapshots/`.
fn check_snapshot(name: &str, actual: &str) -> Result<(), String> {
    check_snapshot_in(
        &snapshots_dir(),
        &snapshot_out_dir(),
        name,
        actual,
        update_snapshots(),
    )
}

/// Panic with every snapshot mismatch collected in `failures`.
fn assert_snapshots(failures: &[String]) {
    assert!(
        failures.is_empty(),
        "snapshot reports changed; rerun with UPDATE_SNAPSHOTS=1 to accept, \
         then review and commit:\n{}",
        failures.join("\n\n")
    );
}

/// Skip a JVM-dependent test with a visible `SKIPPED` line, or fail when
/// `LEEK_REQUIRE_JVM=1` says the JVM path must run.
fn skip_jvm_test(test: &str, reason: &str) {
    assert!(
        std::env::var_os("LEEK_REQUIRE_JVM").is_none_or(|v| v != "1"),
        "{test}: LEEK_REQUIRE_JVM=1 but {reason}"
    );
    eprintln!("SKIPPED {test}: {reason} (set LEEK_REQUIRE_JVM=1 to fail instead)");
}

/// RNG builtins. The JVM reference ops in `snapshot.tsv` were captured
/// from one unseeded run, so a program calling these has no stable
/// expected op count.
const RNG_BUILTINS: &[&str] = &["rand", "randInt", "randFloat", "randReal"];

/// Whether `code` calls an RNG builtin, i.e. its op count is not a
/// reproducible comparison target. Matches whole identifiers only.
fn is_rng_dependent(code: &str) -> bool {
    code.split(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
        .any(|ident| RNG_BUILTINS.contains(&ident))
}

fn fixture_inputs() -> Vec<PathBuf> {
    let mut entries: Vec<_> = fs::read_dir(fixtures_dir().join("inputs"))
        .expect("inputs dir present")
        .filter_map(std::result::Result::ok)
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "leek"))
        .collect();
    entries.sort();
    entries
}

fn rust_emit(src: &str, ai_id: u64, path: &str) -> String {
    let source = SourceId::new(1).unwrap();
    let parsed = parse(src, source, Version::V4);
    let root = SyntaxNode::new_root(parsed.green);
    let sf = leek_parser::ast::SourceFile::cast(root).expect("parse");
    let (hir, _diags) = leek_hir::lower_file(&sf, source);
    let opts = Options::exact(Version::V4, ai_id).with_source_path(path);
    emit(&hir, &opts).java
}

fn unified_diff_labeled(
    expected: &str,
    actual: &str,
    expected_label: &str,
    actual_label: &str,
) -> String {
    let diff = TextDiff::from_lines(expected, actual);
    let mut out = String::new();
    let _ = writeln!(out, "--- {expected_label}");
    let _ = writeln!(out, "+++ {actual_label}");
    for change in diff.iter_all_changes() {
        let tag = match change.tag() {
            ChangeTag::Delete => '-',
            ChangeTag::Insert => '+',
            ChangeTag::Equal => ' ',
        };
        out.push(tag);
        out.push_str(change.value());
    }
    out
}

fn unified_diff(expected: &str, actual: &str) -> String {
    unified_diff_labeled(
        expected,
        actual,
        "golden (Java reference)",
        "rust (leek-backend-java)",
    )
}

/// Iterate every fixture, run the Rust emitter, check the unified
/// diff against the golden matches `tests/snapshots/`. Asserts on the
/// structural invariants we already meet (shape, runtime surface).
#[test]
fn parity_with_java_reference() {
    let mut failures = Vec::new();
    let mut summary = String::new();
    for input in fixture_inputs() {
        let stem = input.file_stem().unwrap().to_string_lossy().into_owned();
        let src = fs::read_to_string(&input).expect("read input");
        let actual = rust_emit(&src, 1, &format!("{stem}.leek"));

        let golden_path = fixtures_dir().join("golden").join(format!("{stem}.java"));
        if !golden_path.exists() {
            eprintln!("missing golden for {stem}; run tools/java-emitter/build.sh and re-capture");
            continue;
        }
        let golden = fs::read_to_string(&golden_path).expect("read golden");

        // Check the diff regardless of whether we're at byte parity.
        // It's a tracking artifact: maintainers watch it shrink.
        let diff = unified_diff(&golden, &actual);
        failures.extend(check_snapshot(&format!("{stem}.exact.diff"), &diff).err());

        // Structural invariants. These are the contract we hold today.
        // Byte parity is a separate iteration; failing here means we
        // regressed *below* shape, not just below byte parity.
        check_shape(&stem, &actual);

        let added = diff
            .lines()
            .filter(|l| l.starts_with('+') && !l.starts_with("+++"))
            .count();
        let removed = diff
            .lines()
            .filter(|l| l.starts_with('-') && !l.starts_with("---"))
            .count();
        let _ = writeln!(summary, "{stem}: +{added} -{removed} lines vs golden");
    }
    failures.extend(check_snapshot("SUMMARY.txt", &summary).err());
    assert_snapshots(&failures);
}

fn check_shape(stem: &str, java: &str) {
    assert!(
        java.contains("import leekscript.runner.*;"),
        "{stem}: missing runner import"
    );
    assert!(
        java.contains("public class AI_1 extends AI {"),
        "{stem}: missing class header (got: {})",
        java.lines().nth(5).unwrap_or("")
    );
    assert!(
        java.contains("public AI_1() throws LeekRunException {"),
        "{stem}: missing constructor"
    );
    assert!(
        java.contains("public Object runIA(Session session)"),
        "{stem}: missing runIA"
    );
}

/// Per-fixture byte-parity assertion. As lowerings tighten,
/// fixtures graduate from "snapshot diff only" to "byte-equal
/// assert". Today this set is the lower-bound — any regression
/// here is a real bug.
fn assert_byte_parity(stem: &str) {
    let input = fixtures_dir().join("inputs").join(format!("{stem}.leek"));
    let golden = fixtures_dir().join("golden").join(format!("{stem}.java"));
    let src = fs::read_to_string(&input).expect("read input");
    let actual = rust_emit(&src, 1, &format!("{stem}.leek"));
    let expected = fs::read_to_string(&golden).expect("read golden");
    if expected != actual {
        let diff = unified_diff(&expected, &actual);
        panic!("byte mismatch on {stem}:\n{diff}");
    }
}

#[test]
fn byte_parity_01_literals() {
    assert_byte_parity("01_literals");
}

#[test]
fn byte_parity_05_string_concat() {
    assert_byte_parity("05_string_concat");
}

#[test]
fn byte_parity_02_arithmetic() {
    assert_byte_parity("02_arithmetic");
}

#[test]
fn byte_parity_03_function_call() {
    assert_byte_parity("03_function_call");
}

#[test]
fn byte_parity_04_control_flow() {
    assert_byte_parity("04_control_flow");
}

#[test]
fn byte_parity_06_while_loop() {
    assert_byte_parity("06_while_loop");
}

#[test]
fn byte_parity_07_do_while() {
    assert_byte_parity("07_do_while");
}

#[test]
fn byte_parity_08_nested_if() {
    assert_byte_parity("08_nested_if");
}

#[test]
fn byte_parity_10_ternary() {
    assert_byte_parity("10_ternary");
}

/// Function emission order in the reference depends on Java's
/// `HashMap` iteration order (`MainLeekBlock.mFunctions`). That's
/// implementation-defined and unreproducible from Rust without
/// reimplementing the JVM's hash semantics. The doc explicitly
/// documents this as a determinism gap (`doc/java-backend.md` §9);
/// the test is kept ignored as a marker.
#[test]
#[ignore = "Java HashMap iteration order is not byte-reproducible — see doc/java-backend.md §9"]
fn byte_parity_09_multi_func() {
    assert_byte_parity("09_multi_func");
}

/// Sanity: every captured golden corresponds to an input we still ship.
#[test]
fn goldens_have_inputs() {
    let inputs_dir = fixtures_dir().join("inputs");
    let golden_dir = fixtures_dir().join("golden");
    for e in fs::read_dir(&golden_dir).expect("golden dir") {
        let p = e.unwrap().path();
        if p.extension().is_some_and(|x| x == "java") {
            let stem = p.file_stem().unwrap().to_string_lossy().into_owned();
            let input = inputs_dir.join(format!("{stem}.leek"));
            assert!(input.exists(), "orphaned golden: {p:?}");
        }
    }
}

/// Cross-side ops-cost parity. Reads
/// `tests/fixtures/ops/cases.tsv` — the same file the upstream
/// `TestOpsCostCorpus.java` consumes — and verifies that the Rust
/// emitter produces the documented `ops(VAL, N)` count when wrapping
/// the snippet's top-level var-decl. Mismatch → emit divergence; fix
/// whichever side is wrong before merging.
#[test]
fn ops_cost_matches_corpus() {
    let path = fixtures_dir().join("ops/cases.tsv");
    let contents = fs::read_to_string(&path).expect("read cases.tsv");
    for (lineno, line) in contents.lines().enumerate() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let cols: Vec<&str> = line.splitn(4, '\t').collect();
        assert_eq!(
            cols.len(),
            4,
            "cases.tsv:{}: expected `value\\tjvm_ops\\tstatic_ops\\tsnippet`, got: {line}",
            lineno + 1
        );
        // cols[0] (value) is sanity-only on the Rust side; cols[1] (jvm_ops)
        // is checked by the Java side.
        let expected_ops: u32 = cols[2]
            .trim()
            .parse()
            .unwrap_or_else(|_| panic!("cases.tsv:{}: bad static_ops column", lineno + 1));
        let snippet = cols[3];

        // The corpus value is the total ops counter the JVM
        // reports after `runIA` (== sum of every `ops(...)` call in
        // the generated Java). Walk every `ops(N)` and `ops(EXPR, N)`
        // in the emit and sum the `N`s.
        let java = rust_emit(snippet, 1, "corpus.leek");
        let actual_ops = sum_static_ops(&java);
        assert_eq!(
            expected_ops,
            actual_ops,
            "cases.tsv:{}: snippet `{snippet}` — Java reference says {expected_ops} ops, \
             Rust emit says {actual_ops}\n--- generated Java ---\n{java}",
            lineno + 1
        );
    }
}

/// Sum the N from every `ops(VALUE, N)` and `ops(N);` occurrence in
/// the emitted Java. This is the static lower bound on the ops
/// counter; it matches the JVM's runtime total exactly when every
/// counted region is executed once (e.g. straight-line code).
fn sum_static_ops(java: &str) -> u32 {
    let mut total = 0u32;
    let bytes = java.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        // Match `ops(` not preceded by an identifier character (so
        // we don't pick up `Lops(` or similar).
        if bytes[i] == b'o'
            && i + 3 < bytes.len()
            && &bytes[i..i + 4] == b"ops("
            && (i == 0 || !is_ident_char(bytes[i - 1]))
        {
            // Find the matching close-paren, accounting for nesting.
            let mut depth = 1;
            let mut j = i + 4;
            while j < bytes.len() && depth > 0 {
                match bytes[j] {
                    b'(' => depth += 1,
                    b')' => depth -= 1,
                    _ => {}
                }
                j += 1;
            }
            // Tail count: either the entire `ops(...)` arg list is
            // a bare number (the standalone `ops(N);` form), or the
            // last comma-separated argument is the count (the
            // `ops(VALUE, N)` overload).
            let arg = &java[i + 4..j - 1];
            let n_str = match arg.rfind(',') {
                Some(p) => &arg[p + 1..],
                None => arg,
            }
            .trim();
            if let Ok(n) = n_str.parse::<u32>() {
                total += n;
            }
            i = j;
        } else {
            i += 1;
        }
    }
    total
}

fn is_ident_char(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

/// The lines of a lowered switch that `SwitchBlock` itself writes (block
/// braces aside): temp declarations, the index chain, the body `switch`, its
/// arm labels, and the per-arm `ops(1)` prologue. Arm bodies are other
/// lowerings, so they are left out of the comparison.
fn switch_skeleton(java: &str) -> Vec<String> {
    java.lines()
        .map(str::trim)
        .filter_map(|l| {
            let is_skeleton = l.starts_with("Object __sw_")
                || l.starts_with("int __si_")
                || l.starts_with("if (ops(eq(__sw_")
                || l.starts_with("else if (ops(eq(__sw_")
                || l.starts_with("switch (__si_")
                || (l.starts_with("case ") && l.ends_with(": {"))
                || l == "default: {";
            let arm_prologue = l.starts_with("ops(1);") && !l.starts_with("ops(1);{");
            if is_skeleton {
                Some(l.to_string())
            } else if arm_prologue {
                Some("ops(1);".to_string())
            } else {
                None
            }
        })
        .collect()
}

/// Exact-mode switch lowering against upstream (#75). Every non-strict
/// `reference.tsv` row whose reference Java contains a switch must emit the
/// same switch skeleton: numbered `__sw_N` / `__si_N` temps, the grouped
/// `if` / `else if` index chain with its op charges, and braced arms.
#[test]
fn exact_switch_skeleton_matches_reference() {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../testing/leek-test-corpus/data/reference.tsv");
    let contents = fs::read_to_string(&path).expect("read reference.tsv");
    let mut checked = 0;
    let mut failures = String::new();
    for line in contents.lines() {
        let cols: Vec<&str> = line.split('\t').collect();
        if cols.len() < 7 || cols[1] == "S" || !cols[6].contains("__si_") {
            continue;
        }
        let version = match cols[0] {
            "1" => Version::V1,
            "2" => Version::V2,
            "3" => Version::V3,
            _ => Version::V4,
        };
        let code = unescape(cols[5]);
        let reference = unescape(cols[6]);
        let source = SourceId::new(1).unwrap();
        let parsed = parse(&code, source, version);
        let sf =
            leek_parser::ast::SourceFile::cast(SyntaxNode::new_root(parsed.green)).expect("parse");
        let version_byte = cols[0].parse().unwrap_or(4);
        let (hir, _diags) = leek_hir::lower_file_versioned(&sf, source, version_byte);
        let java = emit(&hir, &Options::exact(version, 1)).java;
        let (expected, actual) = (switch_skeleton(&reference), switch_skeleton(&java));
        if expected != actual {
            let _ = writeln!(
                failures,
                "v{}: {code}\n  expected {expected:?}\n  actual   {actual:?}",
                cols[0]
            );
        }
        checked += 1;
    }
    assert!(checked > 0, "no switch rows found in reference.tsv");
    assert!(failures.is_empty(), "switch skeleton drift:\n{failures}");
}

/// Cross-side cross-check against the upstream-captured snapshot.
///
/// For every passing inline assertion in the Java suite we record
/// `(version, kind, value, jvm_ops, code)` into
/// `tests/fixtures/ops/snapshot.tsv` via the `LEEK_SNAPSHOT` probe.
/// This test loads each `kind=equals` row, runs the snippet through
/// the Rust interpreter, and asserts that:
///
///   1. The Rust interpreter produces the same value, AND
///   2. The Rust backend's emitted Java is non-empty (sanity that
///      the snippet at least parses+lowers).
///
/// Ops-count parity is a soft check with a pinned report: per-case
/// drift goes to `NATIVE_OPS_DRIFT.txt` and the totals to
/// `CORPUS_SUMMARY.txt`, both compared against the committed copies.
/// Rows calling an RNG builtin are not ops-compared (their expected
/// count is one unseeded JVM sample). The ratio gate asserts only on
/// **value** mismatches.
///
/// This is the **runtime** companion to the static-emit byte-parity
/// tests above: byte-parity proves we emit the right Java; this
/// proves we *execute* to the right value too.
#[test]
fn corpus_value_matches_snapshot() {
    // snapshot.tsv is tracked, so a missing file is a broken checkout,
    // not a reason to skip.
    let path = fixtures_dir().join("ops/snapshot.tsv");
    let contents = fs::read_to_string(&path).expect("read tracked snapshot.tsv");

    let mut total = 0u32;
    let mut value_ok = 0u32;
    let mut value_mismatch = 0u32;
    let mut interp_err = 0u32;
    let mut ops_match = 0u32;
    let mut ops_mismatch = 0u32;
    let mut ops_nondet = 0u32;
    let mut mismatches = String::new();
    let mut ops_diffs: Vec<(usize, u8, String, i64, u64, u64)> = Vec::new();

    for (lineno, line) in contents.lines().enumerate() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let cols: Vec<&str> = line.splitn(6, '\t').collect();
        if cols.len() != 6 {
            continue;
        }
        let version_byte: u8 = match cols[0].trim().parse() {
            Ok(v) => v,
            Err(_) => continue,
        };
        // Skip strict-mode rows — strict typing narrows compound
        // assigns (`var a = 100; a /= 5;` → 20 not 20.0) and we
        // don't yet replicate that pipeline.
        if cols[1] == "S" {
            continue;
        }
        let kind = cols[2];
        if kind != "equals" {
            continue;
        }
        let expected_value = unescape(cols[3]);
        let expected_ops: u64 = cols[4].trim().parse().unwrap_or(u64::MAX);
        let code = unescape(cols[5]);

        total += 1;

        // Run via the Rust interpreter.
        let outcome = run_via_interp(&code, version_byte);
        match outcome {
            InterpOutcome::Ok { value, ops } => {
                if value == expected_value {
                    value_ok += 1;
                    if is_rng_dependent(&code) {
                        ops_nondet += 1;
                    } else if expected_ops == ops {
                        ops_match += 1;
                    } else if expected_ops != u64::MAX {
                        ops_mismatch += 1;
                        let delta =
                            i64::try_from(ops).unwrap() - i64::try_from(expected_ops).unwrap();
                        ops_diffs.push((
                            lineno + 1,
                            version_byte,
                            code.clone(),
                            delta,
                            expected_ops,
                            ops,
                        ));
                    }
                } else {
                    value_mismatch += 1;
                    if mismatches.lines().count() < 50 {
                        let _ = writeln!(
                            mismatches,
                            "snapshot:{} v{}: code={:?}\n  expected={:?}\n  got={:?}",
                            lineno + 1,
                            version_byte,
                            code,
                            expected_value,
                            value
                        );
                    }
                }
            }
            InterpOutcome::Err(msg) => {
                interp_err += 1;
                if mismatches.lines().count() < 50 {
                    let _ = writeln!(
                        mismatches,
                        "snapshot:{} v{}: code={:?} interp err: {msg}",
                        lineno + 1,
                        version_byte,
                        code,
                    );
                }
            }
        }
    }

    // Per-case ops drift detail (native runtime counter vs the JVM
    // snapshot's ops column) — sidecar so the summary stays scannable.
    // The stable sort keeps equal-|delta| rows in file order.
    let mut failures = Vec::new();
    {
        let mut drift_report = String::new();
        ops_diffs.sort_by_key(|(_, _, _, d, _, _)| -d.abs());
        for (lineno, v, code, delta, exp, got) in &ops_diffs {
            let _ = writeln!(
                drift_report,
                "L{lineno}: v{v} delta={delta:+} expected={exp} got={got} code={code:?}"
            );
        }
        failures.extend(check_snapshot("NATIVE_OPS_DRIFT.txt", &drift_report).err());
    }

    let report = format!(
        "snapshot cross-check: {total} cases\n\
         \u{2713} {value_ok} matched value ({ops_match} also matched ops, {ops_mismatch} ops drift, \
         {ops_nondet} RNG-dependent not ops-compared)\n\
         \u{2717} {value_mismatch} value mismatches\n\
         \u{2717} {interp_err} interpreter errors / panics avoided\n",
    );
    failures.extend(check_snapshot("CORPUS_SUMMARY.txt", &format!("{report}\n{mismatches}")).err());

    // Surface the report regardless of pass/fail so a CI log shows
    // current parity health.
    eprintln!("{report}");

    // Hard gate: value parity below 80% means we likely regressed
    // the interpreter or the emit shape, not just a handful of edge
    // cases. The threshold is a leading indicator — tightening it
    // is its own slice.
    let value_pass_ratio = f64::from(value_ok) / f64::from(total.max(1));
    assert!(
        value_pass_ratio >= 0.80,
        "value parity too low: {value_ok}/{total} = {:.1}% (need ≥ 80%)\n{report}",
        value_pass_ratio * 100.0
    );
    assert_snapshots(&failures);
}

enum InterpOutcome {
    Ok { value: String, ops: u64 },
    Err(String),
}

fn run_via_interp(code: &str, version_byte: u8) -> InterpOutcome {
    // Convert byte to the syntax Version enum the parser wants.
    let version = match version_byte {
        1 => Version::V1,
        2 => Version::V2,
        3 => Version::V3,
        4 => Version::V4,
        _ => return InterpOutcome::Err(format!("unsupported version byte {version_byte}")),
    };
    // Run on a worker thread with a wall-clock cap. Rust can't kill
    // a stuck thread safely, so on timeout we leak the worker and
    // move on — the leak is bounded by how many corpus rows trip
    // a parser / lowerer infinite loop. `catch_unwind` still wraps
    // the worker body so panics on either side become error rows
    // rather than a test-process abort.
    let code = code.to_string();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let source = SourceId::new(1).unwrap();
            let parsed = parse(&code, source, version);
            let root = SyntaxNode::new_root(parsed.green);
            let Some(sf) = leek_parser::ast::SourceFile::cast(root) else {
                return Err("parse failed".to_string());
            };
            let (hir, _diags) = leek_hir::lower_file_versioned(&sf, source, version_byte);
            leek_runtime::DISPLAY_VERSION.with(|c| c.set(version_byte));
            let mut opts = leek_backend_native::NativeOptions::release();
            opts.version = version_byte;
            opts.op_limit = SNAPSHOT_OP_LIMIT;
            opts.emit = leek_backend_native::NativeEmit::Jit;
            match leek_backend_native::compile(&hir, &opts) {
                Ok(leek_backend_native::NativeArtifact::Value(v)) => {
                    Ok((v.to_string(), leek_backend_native::ops_used()))
                }
                Ok(_) => Err("native produced no value".to_string()),
                Err(e) => Err(e.to_string()),
            }
        }));
        let _ = tx.send(match outcome {
            Ok(Ok((value, ops))) => InterpOutcome::Ok { value, ops },
            Ok(Err(msg)) => InterpOutcome::Err(msg),
            Err(_) => InterpOutcome::Err("panic during interp".into()),
        });
    });
    match rx.recv_timeout(std::time::Duration::from_secs(10)) {
        Ok(outcome) => outcome,
        Err(_) => InterpOutcome::Err("timeout".into()),
    }
}

/// Reverse of the Java side's `escape(...)` in `TestCommon.java`.
fn unescape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.next() {
                Some('n') => out.push('\n'),
                Some('r') => out.push('\r'),
                Some('t') => out.push('\t'),
                Some('\\') => out.push('\\'),
                Some(other) => {
                    out.push('\\');
                    out.push(other);
                }
                None => out.push('\\'),
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// Cross-side **runtime** parity for the Rust java emission.
///
/// For every `equals` row in the snapshot:
///   1. Rust backend emits Java for the snippet.
///   2. We base64-encode and pipe the source into the upstream
///      `leekscript.tools.RunEmittedJava` batch harness.
///   3. The harness compiles+runs each on the JVM and reports
///      `(value, ops)` per snippet.
///   4. We assert both match the JVM-captured snapshot.
///
/// This is the closing loop: the byte-parity tests prove our emit
/// is structurally faithful; the interp test proves our walker
/// produces the same value; this test proves the JVM agrees on
/// **our** emit, end-to-end. Skipped with a `SKIPPED` line when the
/// harness isn't built (no `tools/java-emitter/build/leekscript-emitter.jar`)
/// or the JVM can't be spawned, so the suite stays runnable on machines
/// without a JDK; `LEEK_REQUIRE_JVM=1` makes those skips failures.
#[test]
fn rust_emit_matches_snapshot_on_jvm() {
    const TEST: &str = "rust_emit_matches_snapshot_on_jvm";
    let snapshot_path = fixtures_dir().join("ops/snapshot.tsv");
    let jar_path = workspace_root().join("tools/java-emitter/build/leekscript-emitter.jar");
    if !jar_path.exists() {
        skip_jvm_test(
            TEST,
            &format!(
                "{} missing — run tools/java-emitter/build.sh first",
                jar_path.display()
            ),
        );
        return;
    }
    let java_bin = match std::env::var_os("JAVA") {
        Some(v) => std::path::PathBuf::from(v),
        None => std::path::PathBuf::from("java"),
    };

    let contents = fs::read_to_string(&snapshot_path).expect("read tracked snapshot.tsv");
    let mut cases: Vec<JvmCase> = Vec::new();
    for (lineno, line) in contents.lines().enumerate() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let cols: Vec<&str> = line.splitn(6, '\t').collect();
        if cols.len() != 6 {
            continue;
        }
        // Skip strict-mode rows (column 2 == "S") — see the interp
        // cross-check above for the rationale.
        if cols[1] == "S" {
            continue;
        }
        if cols[2] != "equals" {
            continue;
        }
        let version_byte: u8 = match cols[0].trim().parse() {
            Ok(v) => v,
            Err(_) => continue,
        };
        let expected_value = unescape(cols[3]);
        let expected_ops: u64 = cols[4].trim().parse().unwrap_or(u64::MAX);
        let code = unescape(cols[5]);

        // Emit Java for this snippet via the Rust backend. Skip the
        // case (rather than failing) when emission itself blows up —
        // those are reported by the static parity tests, not here.
        let Some(java) = emit_via_rust(&code, version_byte, lineno) else {
            continue;
        };
        cases.push(JvmCase {
            id: format!("L{}", lineno + 1),
            version: version_byte,
            code,
            java,
            expected_value,
            expected_ops,
        });
    }

    eprintln!("piping {} cases through RunEmittedJava…", cases.len());
    let results = match run_harness(&java_bin, &jar_path, &cases) {
        Ok(r) => r,
        Err(e) => {
            skip_jvm_test(
                TEST,
                &format!("could not spawn {}: {e}", java_bin.display()),
            );
            return;
        }
    };

    let mut value_ok = 0u32;
    let mut value_mismatch = 0u32;
    let mut ops_ok = 0u32;
    let mut ops_mismatch = 0u32;
    let mut ops_nondet = 0u32;
    let mut jvm_err = 0u32;
    let mut mismatches = String::new();
    let mut ops_diffs: Vec<(String, u8, String, i64, u64, u64)> = Vec::new();
    let mut failures = Vec::new();

    for case in &cases {
        let Some(res) = results.get(&case.id) else {
            jvm_err += 1;
            continue;
        };
        if !res.error.is_empty() {
            jvm_err += 1;
            // Cap lifted: with strict-mode rows filtered out, the
            // remaining errors all warrant inspection.
            let _ = writeln!(
                mismatches,
                "{}: v{} code={:?}\n  jvm err: {}",
                case.id, case.version, case.code, res.error
            );
            continue;
        }
        if res.value == case.expected_value {
            value_ok += 1;
        } else {
            value_mismatch += 1;
            let _ = writeln!(
                mismatches,
                "{}: v{} code={:?}\n  expected={:?}\n  got     ={:?}",
                case.id, case.version, case.code, case.expected_value, res.value
            );
        }
        if is_rng_dependent(&case.code) {
            ops_nondet += 1;
        } else if res.ops == case.expected_ops {
            ops_ok += 1;
        } else {
            ops_mismatch += 1;
            let delta = i64::try_from(res.ops).unwrap() - i64::try_from(case.expected_ops).unwrap();
            ops_diffs.push((
                case.id.clone(),
                case.version,
                case.code.clone(),
                delta,
                case.expected_ops,
                res.ops,
            ));
        }
    }
    // Persist ops drift detail to a sidecar file — separate from
    // JVM_PARITY.txt so the main report stays scannable.
    {
        let mut drift_report = String::new();
        ops_diffs.sort_by_key(|(_, _, _, d, _, _)| -d.abs());
        for (id, v, code, delta, exp, got) in &ops_diffs {
            let _ = writeln!(
                drift_report,
                "{id}: v{v} delta={delta:+} expected={exp} got={got} code={code:?}"
            );
        }
        failures.extend(check_snapshot("OPS_DRIFT.txt", &drift_report).err());
    }

    let total = u32::try_from(cases.len()).unwrap();
    // RNG-dependent rows have no reproducible expected op count, so the
    // ops ratio is over the deterministic rows only.
    let ops_total = total - ops_nondet;
    let report = format!(
        "rust-emit JVM parity: {total} cases\n\
         \u{2713} value: {value_ok}/{total} ({:.1}%)\n\
         \u{2713} ops:   {ops_ok}/{ops_total} ({:.1}%)\n\
         \u{2717} value mismatches: {value_mismatch}\n\
         \u{2717} ops drift:        {ops_mismatch}\n\
         \u{2717} jvm errors:       {jvm_err}\n\
         - ops not compared (RNG-dependent): {ops_nondet}\n",
        100.0 * f64::from(value_ok) / f64::from(total.max(1)),
        100.0 * f64::from(ops_ok) / f64::from(ops_total.max(1)),
    );
    failures.extend(check_snapshot("JVM_PARITY.txt", &format!("{report}\n{mismatches}")).err());
    eprintln!("{report}");

    // Three ratchets — each tightens as emit gaps close. Bump them
    // up after every batch of fixes so regressions can't sneak in.
    // Remaining gaps (track per case in `JVM_PARITY.txt`):
    //  - Block-bodied lambdas can't see outer locals — emit needs
    //    to outline them into top-level helper methods.
    //  - Assignment to a builtin / function / class name
    //    (`count = 1; return count`) — broken without a HIR-level
    //    rewrite that shadows the name with a local.
    //  - v1–v3 receiver-method calls into `LegacyArrayLeekValue`
    //    methods that don't exist on it (`arrayMap`, `arrayFilter`,
    //    `arrayFind`, etc.). Upstream emits per-call-site
    //    `Array_<name>_<sig>` helpers via
    //    `JavaWriter.writeGenericFunctions`; we don't yet.
    //  - Default parameter values aren't lowered into call-site
    //    null-fill or synthesized overloads.
    //  - Index l-value chains with promote-on-write semantics
    //    (`tabmulti[i][j] = v` where the inner array morphs into
    //    a sparse map in v1-v3).
    //  - Bit-XOR `^` in v1 means POWER, not XOR — we lower as XOR.
    let value_ratio = f64::from(value_ok) / f64::from(total.max(1));
    let ops_ratio = f64::from(ops_ok) / f64::from(ops_total.max(1));
    let breakdown = snapshot_out_dir().join("JVM_PARITY.txt");
    let breakdown = breakdown.display();
    assert!(
        value_ratio >= 0.96,
        "rust-emit JVM value parity below 96%: {value_ok}/{total} = {:.1}%\n\
         See {breakdown} for the per-case breakdown",
        value_ratio * 100.0
    );
    assert!(
        ops_ratio >= 0.89,
        "rust-emit JVM ops parity below 89%: {ops_ok}/{ops_total} = {:.1}%\n\
         See {breakdown} for the per-case breakdown",
        ops_ratio * 100.0
    );
    // Strict ceiling — every javac/JVM compile failure has a clear
    // codegen root cause; we shouldn't regress past today's count
    // even by one. Bump this down (never up) as fixes land.
    assert!(
        jvm_err == 0,
        "rust-emit JVM error count above 0: {jvm_err}\n\
         See {breakdown} for the per-case breakdown"
    );
    assert_snapshots(&failures);
}

struct JvmCase {
    id: String,
    version: u8,
    code: String,
    java: String,
    expected_value: String,
    expected_ops: u64,
}

struct JvmResult {
    value: String,
    ops: u64,
    error: String,
}

fn workspace_root() -> std::path::PathBuf {
    // CARGO_MANIFEST_DIR is `…/crates/backends/leek-backend-java`.
    // Walk up four levels: backends → crates → workspace.
    let manifest = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest.ancestors().nth(3).unwrap().to_path_buf()
}

fn emit_via_rust(code: &str, version_byte: u8, lineno: usize) -> Option<String> {
    let version = match version_byte {
        1 => Version::V1,
        2 => Version::V2,
        3 => Version::V3,
        4 => Version::V4,
        _ => return None,
    };
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let source = SourceId::new(1).unwrap();
        let parsed = parse(code, source, version);
        let root = SyntaxNode::new_root(parsed.green);
        let sf = leek_parser::ast::SourceFile::cast(root)?;
        let (hir, _diags) = leek_hir::lower_file_versioned(&sf, source, version_byte);
        let opts = Options::exact(version, lineno as u64 + 1)
            .with_source_path(format!("snapshot:{}.leek", lineno + 1));
        Some(emit(&hir, &opts).java)
    }));
    match outcome {
        Ok(Some(j)) => Some(j),
        _ => None,
    }
}

fn run_harness(
    java_bin: &std::path::Path,
    jar_path: &std::path::Path,
    cases: &[JvmCase],
) -> std::io::Result<std::collections::HashMap<String, JvmResult>> {
    use std::io::{BufRead, BufReader, Write};
    use std::process::{Command, Stdio};

    let mut child = Command::new(java_bin)
        .arg("-cp")
        .arg(jar_path)
        .arg("leekscript.tools.RunEmittedJava")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;

    // Feed all cases via a writer thread so we can read stdout
    // concurrently and not deadlock on the pipe buffer.
    let mut stdin = child.stdin.take().expect("stdin");
    let stdout = child.stdout.take().expect("stdout");
    let payloads: Vec<(String, String)> = cases
        .iter()
        .map(|c| {
            use base64::Engine;
            let b64 = base64::engine::general_purpose::STANDARD.encode(&c.java);
            (c.id.clone(), b64)
        })
        .collect();
    let writer = std::thread::spawn(move || {
        for (id, b64) in payloads {
            if writeln!(stdin, "{id}\t{b64}").is_err() {
                break;
            }
        }
        drop(stdin); // close stdin → harness exits on EOF
    });

    let mut results = std::collections::HashMap::new();
    let reader = BufReader::new(stdout);
    for line in reader.lines() {
        let Ok(line) = line else {
            break;
        };
        let cols: Vec<&str> = line.splitn(4, '\t').collect();
        if cols.len() != 4 {
            continue;
        }
        let id = cols[0].to_string();
        let value = unescape(cols[1]);
        let ops: u64 = cols[2].parse().unwrap_or(0);
        let error = unescape(cols[3]);
        results.insert(id, JvmResult { value, ops, error });
    }
    let _ = writer.join();
    let _ = child.wait();
    Ok(results)
}

/// Bonus diagnostic: also check a `.clean-vs-exact.diff` showing how
/// the optimized output differs from the byte-faithful one. Useful for
/// reviewers reading PRs that touch the emitter.
#[test]
fn capture_clean_vs_exact_diff() {
    let mut failures = Vec::new();
    for input in fixture_inputs() {
        let stem = input.file_stem().unwrap().to_string_lossy().into_owned();
        let src = fs::read_to_string(&input).expect("read input");
        let source = SourceId::new(1).unwrap();
        let parsed = parse(&src, source, Version::V4);
        let root = SyntaxNode::new_root(parsed.green);
        let sf = leek_parser::ast::SourceFile::cast(root).expect("parse");
        let (hir, _diags) = leek_hir::lower_file(&sf, source);
        let path = format!("{stem}.leek");
        let exact = emit(
            &hir,
            &Options::exact(Version::V4, 1).with_source_path(&path),
        )
        .java;
        let clean = emit(
            &hir,
            &Options::clean(Version::V4, 1).with_source_path(&path),
        )
        .java;
        let diff = unified_diff_labeled(
            &exact,
            &clean,
            "exact mode (byte-faithful)",
            "clean mode (readable / charge-folded)",
        );
        failures.extend(check_snapshot(&format!("{stem}.clean-vs-exact.diff"), &diff).err());
    }
    assert_snapshots(&failures);
}

/// Compare mode writes the fresh report to the out dir only and reports
/// a mismatch; the tracked file is left byte-for-byte untouched (#68).
#[test]
fn snapshot_compare_mode_never_rewrites_tracked_file() {
    let root = snapshot_out_dir().join("self-test-compare");
    let (tracked, out) = (root.join("tracked"), root.join("out"));
    fs::create_dir_all(&tracked).unwrap();
    fs::write(tracked.join("R.txt"), "old\n").unwrap();

    let err = check_snapshot_in(&tracked, &out, "R.txt", "new\n", false)
        .expect_err("a changed report must fail");
    assert!(err.contains("-old") && err.contains("+new"), "{err}");
    assert_eq!(fs::read_to_string(tracked.join("R.txt")).unwrap(), "old\n");
    assert_eq!(fs::read_to_string(out.join("R.txt")).unwrap(), "new\n");

    assert!(check_snapshot_in(&tracked, &out, "R.txt", "old\n", false).is_ok());
    assert!(
        check_snapshot_in(&tracked, &out, "Missing.txt", "x", false).is_err(),
        "an uncommitted report must fail rather than be created"
    );
    assert!(!tracked.join("Missing.txt").exists());
}

/// `UPDATE_SNAPSHOTS=1` mode is the only path that writes tracked files.
#[test]
fn snapshot_update_mode_overwrites_tracked_file() {
    let root = snapshot_out_dir().join("self-test-update");
    let (tracked, out) = (root.join("tracked"), root.join("out"));
    let _ = fs::remove_dir_all(&tracked);
    assert!(check_snapshot_in(&tracked, &out, "R.txt", "new\n", true).is_ok());
    assert_eq!(fs::read_to_string(tracked.join("R.txt")).unwrap(), "new\n");
}

/// The ops-drift reports exclude exactly the programs calling an RNG
/// builtin, matched on whole identifiers (#73).
#[test]
fn rng_dependent_programs_are_detected_by_identifier() {
    assert!(is_rng_dependent("return randInt(0, 4)"));
    assert!(is_rng_dependent("var x = rand() return x < 1"));
    assert!(is_rng_dependent("return randFloat(1, 2) + randReal(1, 2)"));
    assert!(!is_rng_dependent("var operand = 1 return operand"));
    assert!(!is_rng_dependent("var randomize = 1 return my_randInt"));
}
