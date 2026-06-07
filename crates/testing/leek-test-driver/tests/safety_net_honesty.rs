//! Pins for the Phase 1 "honest safety-net" fixes (Theme F). These guard
//! against the harness silently over-reporting green: the pipeline backend
//! must value-check `Equals` (not pass on clean compilation alone), and
//! `check_almost` must fail closed rather than waving through an expectation
//! it can't evaluate.

use leek_span::SourceId;
use leek_test_driver::backends::{run_case_backend, SuiteBackend};
use leek_test_driver::cases::{Expectation, TestCase};
use leek_test_driver::CaseOutcome;

fn case(code: &str, expected: Expectation) -> TestCase {
    TestCase {
        id: "pin::safety_net::0@v4".into(),
        source_file: "pin".into(),
        method_name: "safety_net".into(),
        line: 1,
        call_index: 0,
        helper: String::new(),
        java_line: String::new(),
        version: 4,
        strict: false,
        enabled: true,
        code: code.into(),
        expected,
        audit: None,
    }
}

#[test]
fn pipeline_equals_is_value_checked_not_compile_only() {
    let src = SourceId::new(1).unwrap();

    // Correct value → Pass.
    let ok = case("return 1 + 1", Expectation::Equals { value: "2".into() });
    assert_eq!(
        run_case_backend(&ok, src, SuiteBackend::Pipeline),
        CaseOutcome::Pass,
    );

    // Compiles cleanly but the expected value is wrong. Before the fix this
    // passed on compilation alone; it must now be a wrong-value failure.
    let bad = case("return 1 + 1", Expectation::Equals { value: "999".into() });
    assert_eq!(
        run_case_backend(&bad, src, SuiteBackend::Pipeline),
        CaseOutcome::FailWrongValue,
    );
}

#[test]
fn pipeline_almost_evaluates_java_math_expectation() {
    let src = SourceId::new(1).unwrap();

    // `Math.PI / 2` used to be waved through (unparseable as bare f64 →
    // `return true`). It is now evaluated via the shared Java-math grammar,
    // so a program that genuinely yields π/2 passes...
    let ok = case(
        "return 1.5707963267948966",
        Expectation::Almost { value: "Math.PI / 2".into() },
    );
    assert_eq!(
        run_case_backend(&ok, src, SuiteBackend::Pipeline),
        CaseOutcome::Pass,
    );

    // ...and a wrong value is a real failure, not a silent pass.
    let bad = case(
        "return 0.0",
        Expectation::Almost { value: "Math.PI / 2".into() },
    );
    assert_eq!(
        run_case_backend(&bad, src, SuiteBackend::Pipeline),
        CaseOutcome::FailWrongValue,
    );
}
