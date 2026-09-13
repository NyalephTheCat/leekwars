//! Pins for the "honest safety-net" guarantees (Theme F). These guard
//! against the harness silently over-reporting green: the *native* backend —
//! the value-checking run path since the interpreter backend was removed —
//! must value-check `Equals` (not pass on clean compilation alone), and the
//! `.almost(...)` path must evaluate Java-math expectations rather than
//! waving through an expectation it can't parse.
//!
//! The other two columns are weaker *on purpose*, and the pins below say by
//! how much, so nobody reads a green corpus column as "this backend is
//! correct": `pipeline` is a compile gate, and `java-emit` only proves the
//! Java emitter produced a file without panicking.

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

/// The `java-emit` column is emit-only, and this test pins that weakness in
/// place: a program whose expected value is plainly wrong still passes,
/// because nothing here compiles or runs the emitted Java.
///
/// That is the documented meaning of the column, not an oversight. It used to
/// be spelled `java` and checked `emitted.java.contains("class AI_")`, which
/// `emit_file` makes unconditionally true (it writes `public class AI_<id>`
/// before any body) — so the column read like a correctness result while
/// measuring "HIR was produced". If you make this test fail by teaching
/// `java-emit` to check values, rename the column with it; value checking
/// against a real JVM belongs to a separate `java-jvm` column (#70).
#[test]
fn java_emit_is_emit_only_not_value_checked() {
    let src = SourceId::new(1).unwrap();

    let wrong_value = case(
        "return 1 + 1",
        Expectation::Equals {
            value: "999".into(),
        },
    );
    assert_eq!(
        run_case_backend(&wrong_value, src, SuiteBackend::JavaEmit),
        CaseOutcome::Pass,
    );
}

/// The one thing `java-emit` does gate: the program has to compile. Lowering
/// recovers HIR from an erroring parse, so "we produced HIR" is not evidence
/// the program was accepted — before the emit-only rewrite this case emitted
/// Java from the recovered HIR and reported `Pass`, while `pipeline` reported
/// the same case as `FailParseError`.
#[test]
fn java_emit_fails_when_the_program_does_not_compile() {
    let src = SourceId::new(1).unwrap();

    let broken = case("return 1 +", Expectation::Equals { value: "2".into() });
    assert_eq!(
        run_case_backend(&broken, src, SuiteBackend::JavaEmit),
        CaseOutcome::FailParseError,
    );
}

/// The column *name* is the only place the emit-only caveat reaches someone
/// reading `baseline.toml` or a `failures` table, so it is load-bearing. The
/// old `java` spelling stays parseable as an alias: `SuiteBackend::parse`
/// returns `None` for an unknown name and the CLI then reads the argument as
/// a *category* filter, which would turn a stale `failures java` into a
/// plausible-looking empty table instead of an error.
#[test]
fn java_emit_column_is_named_for_what_it_measures() {
    assert_eq!(SuiteBackend::JavaEmit.as_str(), "java-emit");
    assert_eq!(
        SuiteBackend::parse("java-emit"),
        Some(SuiteBackend::JavaEmit),
    );
    assert_eq!(SuiteBackend::parse("java"), Some(SuiteBackend::JavaEmit));
}
