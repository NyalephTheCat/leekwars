//! Run the upstream JUnit suite on every linked backend (`upstream_cases.toml`).

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
        eprintln!("\n{improvements} improvements vs baseline (update with --save-baseline)",);
    }
}
