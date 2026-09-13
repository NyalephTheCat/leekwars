//! Run the upstream JUnit suite on every linked backend (the manifest
//! `build.rs` embeds) and check it against the committed baseline.

use leek_test_corpus::{MultiReport, baseline_path, embedded_manifest, run_upstream_suite};

#[test]
fn manifest_has_cases() {
    let m = embedded_manifest();
    assert!(
        m.cases.len() > 5_000,
        "embedded manifest has only {} cases — extractor regression?",
        m.cases.len(),
    );
    assert_eq!(m.schema_version, leek_test_corpus::Manifest::SCHEMA_VERSION);
}

/// The committed baseline must stay in canonical failures-only form: it
/// round-trips through `MultiReport::save` byte for byte, so refreshing it
/// shows the outcomes that actually moved instead of megabytes of churn.
#[test]
fn committed_baseline_is_canonical_failures_only() {
    let path = baseline_path();
    let text = std::fs::read_to_string(&path).expect("read baseline");
    let baseline = MultiReport::load(&path).expect("malformed baseline");

    for (backend, report) in &baseline.backends {
        for (id, outcome) in &report.outcomes {
            assert!(
                !outcome.is_pass(),
                "[{backend}] {id} is stored as {outcome:?}; baselines keep the \
                 non-passing outcomes only — re-create with \
                 `cargo run -p leek-test-corpus -- run --save-baseline`",
            );
        }
    }

    let canonical = toml::to_string_pretty(&baseline.failures_only()).expect("serialize baseline");
    assert_eq!(
        canonical,
        text,
        "{} is not in the shape `run --save-baseline` writes",
        path.display(),
    );
}

/// The three columns are checked by visibly different logic — `native` runs
/// the program and compares the value, `pipeline` only compiles it, and
/// `java-emit` only emits Java — so they cannot all agree on every one of
/// ~11k cases. When they do, the baseline was saved while the backends shared
/// a code path (or was never re-run), and a green corpus says nothing about
/// any individual backend.
///
/// `native` is the discriminator: it is the only column that can report a
/// wrong value, and the only one that skips constructs outside the compiled
/// subset.
#[test]
fn backends_with_different_check_logic_do_not_share_a_column() {
    let path = baseline_path();
    let baseline = MultiReport::load(&path).expect("malformed baseline");

    let Some(native) = baseline.backends.get("native") else {
        return; // native isn't linked in this build — nothing to compare.
    };

    for other in ["pipeline", "java-emit"] {
        let Some(report) = baseline.backends.get(other) else {
            continue;
        };
        assert!(
            native.outcomes != report.outcomes || native.summary != report.summary,
            "baseline columns `native` and `{other}` are identical across the \
             whole corpus, but they are checked by different logic (native \
             runs and value-checks; pipeline is a compile gate; java-emit only \
             emits). The baseline was saved while the backends shared a code \
             path, or predates the split — re-create it with \
             `cargo run -p leek-test-corpus -- run --save-baseline`",
        );
    }
}

#[test]
fn no_regressions_against_baseline() {
    let multi = run_upstream_suite();

    for (name, report) in &multi.backends {
        let s = &report.summary;
        eprintln!(
            "\n[{name}] {} pass / {} fail / {} skip / {} active ({} total, {} disabled)",
            s.pass_total(),
            s.fail_total(),
            s.skip_total(),
            s.active_total(),
            s.total,
            s.skipped_disabled,
        );
    }

    // Fail closed on a missing submodule. `build.rs` embeds an empty manifest
    // when `official-generator/` is not checked out; the diff below then
    // iterates zero outcomes and reports zero regressions, so without this the
    // gate passes vacuously on exactly the checkout where it is most likely to
    // be misconfigured.
    let largest = multi
        .backends
        .values()
        .map(|r| r.summary.total)
        .max()
        .unwrap_or(0);
    assert!(
        largest > 5_000,
        "the suite ran {largest} cases — the upstream submodules are probably \
         not checked out (`git submodule update --init --recursive`). The \
         regression check must not pass by having nothing to run.",
    );

    let baseline_path = baseline_path();
    assert!(
        baseline_path.exists(),
        "no baseline at {} — the regression check cannot run without one, so \
         this test fails closed rather than passing silently. Create it with \
         `cargo run -p leek-test-corpus -- run --save-baseline`.",
        baseline_path.display(),
    );

    let baseline =
        MultiReport::load(&baseline_path).expect("malformed baseline — delete and re-create");
    let diff = multi.diff_against(&baseline);

    // A column the baseline has never seen diffs against nothing, so every one
    // of its cases reads as "was passing, still passing". Renaming or adding a
    // backend without refreshing the baseline would therefore leave the new
    // column completely ungated while the gate stays green.
    assert!(
        diff.new_backends.is_empty(),
        "backend(s) {:?} have no baseline entry, so they are ungated — refresh \
         with `cargo run -p leek-test-corpus -- run --save-baseline`",
        diff.new_backends,
    );

    if !diff.regressions.is_empty() {
        for (backend, regs) in &diff.regressions {
            eprintln!("\nFAIL [{backend}]: {} regressions:", regs.len());
            for c in regs.iter().take(10) {
                eprintln!("  {} : {:?} -> {:?}", c.id, c.before, c.after);
            }
        }
        panic!("upstream suite regressed on one or more backends");
    }

    let improvements: usize = diff.improvements.values().map(std::vec::Vec::len).sum();
    if improvements > 0 {
        eprintln!("\n{improvements} improvements vs baseline (update with --save-baseline)");
    }
}
