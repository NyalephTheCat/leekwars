//! Pins for the "honest safety-net" guarantees (Theme F). These guard
//! against the harness silently over-reporting green: the *native* backend —
//! the value-checking run path since the interpreter backend was removed —
//! must value-check `Equals` (not pass on clean compilation alone), and the
//! `.almost(...)` path must evaluate Java-math expectations rather than
//! waving through an expectation it can't parse. The pipeline backend is a
//! compile-gate only and is pinned as such.

use leek_span::SourceId;
use leek_test_driver::CaseOutcome;
use leek_test_driver::backends::{SuiteBackend, run_case_backend};
use leek_test_driver::cases::{Expectation, TestCase};

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
fn native_equals_is_value_checked_not_compile_only() {
    let src = SourceId::new(1).unwrap();

    // Correct value → Pass.
    let ok = case("return 1 + 1", Expectation::Equals { value: "2".into() });
    assert_eq!(
        run_case_backend(&ok, src, SuiteBackend::Native),
        CaseOutcome::Pass,
    );

    // Compiles cleanly but the expected value is wrong. A compile-only check
    // would wave this through; it must be a wrong-value failure.
    let bad = case(
        "return 1 + 1",
        Expectation::Equals {
            value: "999".into(),
        },
    );
    assert_eq!(
        run_case_backend(&bad, src, SuiteBackend::Native),
        CaseOutcome::FailWrongValue,
    );
}

#[test]
fn native_almost_evaluates_java_math_expectation() {
    let src = SourceId::new(1).unwrap();

    // `Math.PI / 2` used to be waved through (unparseable as bare f64 →
    // `return true`). It is now evaluated via the shared Java-math grammar,
    // so a program that genuinely yields π/2 passes...
    let ok = case(
        "return 1.5707963267948966",
        Expectation::Almost {
            value: "Math.PI / 2".into(),
        },
    );
    assert_eq!(
        run_case_backend(&ok, src, SuiteBackend::Native),
        CaseOutcome::Pass,
    );

    // ...and a wrong value is a real failure, not a silent pass.
    let bad = case(
        "return 0.0",
        Expectation::Almost {
            value: "Math.PI / 2".into(),
        },
    );
    assert_eq!(
        run_case_backend(&bad, src, SuiteBackend::Native),
        CaseOutcome::FailWrongValue,
    );
}

#[test]
fn pipeline_is_a_compile_gate() {
    let src = SourceId::new(1).unwrap();

    // The pipeline backend only confirms the program parses/compiles cleanly
    // (the interpreter that re-checked values there was removed) — so even a
    // wrong expected value passes, while a compile error still fails.
    let wrong_value = case(
        "return 1 + 1",
        Expectation::Equals {
            value: "999".into(),
        },
    );
    assert_eq!(
        run_case_backend(&wrong_value, src, SuiteBackend::Pipeline),
        CaseOutcome::Pass,
    );

    let broken = case("return 1 +", Expectation::Equals { value: "2".into() });
    assert_eq!(
        run_case_backend(&broken, src, SuiteBackend::Pipeline),
        CaseOutcome::FailParseError,
    );
}
