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

/// Every backend this build runs must have its own full-size column in the
/// baseline. That is what stops the baseline from saying nothing about a
/// backend: a column that is missing (a rename, a newly linked backend) diffs
/// against nothing and reads as all-passing, and a column whose summary covers
/// a handful of cases was recorded against a truncated manifest.
///
/// Note what this deliberately does *not* assert: that the columns differ from
/// each other. They are identical today, and legitimately so — `native` passes
/// every active case, and a case native runs and value-checks necessarily also
/// compiles (`pipeline`) and emits (`java-emit`). The columns can only diverge
/// once something fails, so requiring them to differ would be requiring the
/// compiler to be broken. What separates them is the *check logic*, which is
/// pinned in `leek-test-driver/tests/safety_net_honesty.rs`, not the data.
#[test]
fn every_backend_has_a_full_size_baseline_column() {
    let path = baseline_path();
    let baseline = MultiReport::load(&path).expect("malformed baseline");

    for backend in leek_test_corpus::suite_backends() {
        assert!(
            baseline.backends.contains_key(backend.as_str()),
            "backend `{}` has no column in {} — it would diff against nothing \
             and report zero regressions. Refresh with \
             `cargo run -p leek-test-corpus -- run --save-baseline`",
            backend.as_str(),
            path.display(),
        );
    }

    let totals: Vec<_> = baseline
        .backends
        .iter()
        .map(|(name, r)| (name.as_str(), r.summary.total))
        .collect();
    let largest = totals.iter().map(|(_, t)| *t).max().unwrap_or(0);
    assert!(
        largest > 5_000,
        "the largest baseline column covers only {largest} cases — it was \
         saved against a truncated or empty manifest: {totals:?}",
    );
    for (name, total) in &totals {
        assert_eq!(
            *total, largest,
            "baseline column `{name}` covers {total} of {largest} cases, so it \
             was recorded from a different run than the others: {totals:?}",
        );
    }
}

/// The suite runs on a worker pool, so "the same code gives the same report"
/// is a property, not a tautology: a case that leaked state into the next one
/// would land differently depending on which worker picked it up, and the
/// baseline diff would flicker instead of failing.
///
/// `leek-test-driver/tests/parallel_determinism.rs` pins this on a synthetic
/// manifest in seconds; this pins it on the real 11005 cases, which costs a
/// second full suite run. Hence `#[ignore]`: run it by hand or from the
/// workflow_dispatch step in `corpus.yml`, not on every PR.
///
/// ```text
/// cargo test -p leek-test-corpus --release -- --ignored run_to_run
/// ```
#[test]
#[ignore = "runs the full suite twice; wired to the manual corpus workflow"]
fn the_full_suite_is_run_to_run_identical() {
    let first = run_upstream_suite();
    let second = run_upstream_suite();

    assert_eq!(
        first.backends.keys().collect::<Vec<_>>(),
        second.backends.keys().collect::<Vec<_>>(),
    );
    for (name, a) in &first.backends {
        let b = &second.backends[name];
        assert!(
            a.summary.total > 5_000,
            "[{name}] only {} cases ran — submodules missing?",
            a.summary.total,
        );
        assert_eq!(a.summary, b.summary, "[{name}] summary is not reproducible");
        let differing: Vec<_> = a
            .outcomes
            .iter()
            .filter(|(id, outcome)| b.outcomes.get(*id) != Some(*outcome))
            .take(10)
            .collect();
        assert!(
            differing.is_empty(),
            "[{name}] {} case(s) changed outcome between two identical runs, \
             e.g. {differing:?}",
            a.outcomes
                .iter()
                .filter(|(id, outcome)| b.outcomes.get(*id) != Some(*outcome))
                .count(),
        );
        assert_eq!(a.outcomes.len(), b.outcomes.len(), "[{name}] case count");
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
