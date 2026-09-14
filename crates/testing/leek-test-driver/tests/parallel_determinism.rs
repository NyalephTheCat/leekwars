//! The corpus runner is parallel; these pin the properties that makes safe.
//!
//! The upstream gate diffs a run against a committed baseline, so a run that
//! reordered, dropped or duplicated a case would not fail — it would quietly
//! change what the baseline means. What must therefore hold, and is asserted
//! below on a synthetic manifest shaped like the real one:
//!
//! 1. the report does not depend on the worker count (`jobs = 1` and
//!    `jobs = 8` produce byte-identical outcome maps *and* summaries);
//! 2. sharding partitions the manifest — the shards are disjoint and their
//!    union is exactly the unsharded run;
//! 3. a duplicate case id is rejected up front, because outcomes are keyed by
//!    id and the merge would otherwise depend on which worker finished last.
//!
//! The synthetic cases deliberately cover the paths where cross-case state
//! would show up: the JIT value path, the op-counting path, the op-limited
//! runtime-error path, RNG (whose per-run reseed is the whole reason a
//! random-valued case is reproducible), and a deeply nested program that needs
//! the worker's big stack.

use std::collections::BTreeSet;

use leek_span::SourceId;
use leek_test_driver::CaseOutcome;
use leek_test_driver::backends::{RunConfig, Shard, SuiteBackend, probe_case, run_manifest_with};
use leek_test_driver::cases::{Expectation, Manifest, TestCase};

/// Nesting depth for the deep-recursion case. Comfortably inside the 64 MB
/// `WORKER_STACK` and comfortably past the 2 MB a thread gets by default, so
/// dropping `stack_size` from the worker builder aborts this test rather than
/// leaving a silently weaker guarantee.
const NEST: usize = 4_000;

/// Every backend, so the determinism claim covers the JIT (`native`), the
/// frontend (`pipeline`) and the emitter (`java-emit`) at once.
const BACKENDS: &[SuiteBackend] = &[
    SuiteBackend::Pipeline,
    SuiteBackend::JavaEmit,
    SuiteBackend::Native,
];

fn case(id: &str, code: &str, expected: Expectation) -> TestCase {
    TestCase {
        id: id.into(),
        source_file: "synthetic".into(),
        method_name: "parallel".into(),
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

fn manifest(cases: Vec<TestCase>) -> Manifest {
    Manifest {
        schema_version: Manifest::SCHEMA_VERSION,
        cases,
        source_files: vec!["synthetic".into()],
        skipped: Vec::new(),
    }
}

/// What the native backend returns for `code` right now, run on this thread
/// with nothing before it — i.e. exactly the state a worker's first case sees.
/// Used to pin an RNG case to the value the fixed seed produces without
/// hard-coding a number that would silently stop meaning anything if the
/// generator changed.
fn native_value(code: &str) -> String {
    let probe = probe_case(
        &case(
            "probe",
            code,
            Expectation::Equals {
                value: String::new(),
            },
        ),
        SourceId::new(1).unwrap(),
        SuiteBackend::Native,
    );
    probe.actual
}

/// ~200 cases mixing every outcome the runner can produce.
fn synthetic_manifest() -> Manifest {
    // The first `randInt` draw of a freshly seeded generator. Every RNG case
    // below expects it, so if per-run reseeding ever stopped happening — the
    // one way case order could leak into a *value* — the second and later RNG
    // cases would draw something else and fail.
    let rng_code = "return randInt(0, 1000000)";
    let rng_value = native_value(rng_code);
    assert!(
        rng_value.parse::<i64>().is_ok(),
        "the native backend no longer returns a value for `{rng_code}` (got \
         {rng_value:?}), so the RNG cases below pin nothing",
    );

    let mut cases = Vec::new();
    for round in 0..32 {
        let templates: Vec<(String, Expectation)> = vec![
            // Plain value check — passes.
            (
                format!("return {round} + 1"),
                Expectation::Equals {
                    value: (round + 1).to_string(),
                },
            ),
            // Compiles, wrong expectation — a real failure, not a skip.
            (
                "return 1 + 1".into(),
                Expectation::Equals {
                    value: "999".into(),
                },
            ),
            // Parse error against an expected compile error.
            (
                "return 1 +".into(),
                Expectation::Error {
                    code: "SYNTAX_ERROR".into(),
                },
            ),
            // Value *and* op count.
            (
                "var s = 0 for (var i = 0; i < 10; i++) { s = s + i } return s".into(),
                Expectation::EqualsOps {
                    value: "45".into(),
                    count: 0,
                },
            ),
            // Float expectation via the `.almost(...)` path.
            (
                "return 1.0 / 3.0".into(),
                Expectation::Almost {
                    value: "0.3333333333".into(),
                },
            ),
            // RNG — see `rng_value`.
            (
                rng_code.into(),
                Expectation::Equals {
                    value: rng_value.clone(),
                },
            ),
        ];
        for (i, (code, expected)) in templates.into_iter().enumerate() {
            let mut c = case(
                &format!("synthetic::parallel::{round}_{i}@v4"),
                &code,
                expected,
            );
            // A disabled case must be recorded (as `SkippedDisabled`) without
            // ever reaching a backend, on the parallel path too.
            c.enabled = !(round == 3 && i == 1);
            cases.push(c);
        }
    }

    // The two expensive shapes, a few copies each so they land on different
    // workers and in different shards rather than being an afterthought one
    // worker happens to own:
    for n in 0..3 {
        // A left-deep expression tree — needs the worker's big stack. Note it
        // is a flat `+` chain, not nested parentheses: the parser caps its own
        // descent at `MAX_RECURSION_DEPTH`, so only the stages that recurse
        // over the *tree* (lowering, typing, codegen) still go deep.
        cases.push(case(
            &format!("synthetic::deep::{n}@v4"),
            &format!("return {}", vec!["1"; NEST].join(" + ")),
            Expectation::Equals {
                value: NEST.to_string(),
            },
        ));
        // The op-limited runtime-error path: an unbounded loop that must trap
        // on the small budget `native_error_outcome` installs. The only case
        // shape whose *termination* depends on per-run state being reset.
        cases.push(case(
            &format!("synthetic::runaway::{n}@v4"),
            "var i = 0 while (true) { i = i + 1 } return i",
            Expectation::Error {
                code: "TOO_MUCH_OPERATIONS".into(),
            },
        ));
    }

    manifest(cases)
}

/// Assert two reports are the same run: same columns, same per-case outcomes,
/// same counters. Deliberately not `failures_only()` — a skip that turned into
/// a pass is exactly the kind of drift this is looking for.
fn assert_same_report(
    a: &leek_test_driver::MultiReport,
    b: &leek_test_driver::MultiReport,
    what: &str,
) {
    assert_eq!(
        a.backends.keys().collect::<Vec<_>>(),
        b.backends.keys().collect::<Vec<_>>(),
        "{what}: different backend columns",
    );
    for (name, left) in &a.backends {
        let right = &b.backends[name];
        for (id, outcome) in &left.outcomes {
            assert_eq!(
                Some(outcome),
                right.outcomes.get(id),
                "{what}: [{name}] {id} differs",
            );
        }
        assert_eq!(
            left.outcomes.len(),
            right.outcomes.len(),
            "{what}: [{name}] different case counts",
        );
        assert_eq!(
            left.summary, right.summary,
            "{what}: [{name}] summary differs"
        );
    }
}

#[test]
fn the_worker_count_does_not_change_the_report() {
    let m = synthetic_manifest();
    let serial = run_manifest_with(&m, BACKENDS, RunConfig::default().with_jobs(1));
    let parallel = run_manifest_with(&m, BACKENDS, RunConfig::default().with_jobs(8));
    assert_same_report(&serial, &parallel, "jobs=1 vs jobs=8");

    // And that the run is actually doing something: a report of nothing but
    // skips would compare equal for uninteresting reasons.
    let native = &serial.backends["native"];
    assert_eq!(native.summary.total as usize, m.cases.len());
    assert!(
        native.summary.pass_total() > 0 && native.summary.fail_total() > 0,
        "the synthetic manifest produced no passes or no failures ({:?}) — it \
         is no longer exercising the paths it was built for",
        native.summary,
    );
    assert_eq!(
        native.summary.skipped_disabled, 1,
        "the one disabled case must be recorded as such",
    );
    // The two shapes that would otherwise decay into skips without anyone
    // noticing, taking their guarantee (big stack, op budget) with them.
    assert_eq!(
        native.outcomes["synthetic::deep::0@v4"],
        CaseOutcome::Pass,
        "the deeply nested case no longer runs, so it stops pinning the \
         worker stack size",
    );
    assert_eq!(
        native.outcomes["synthetic::runaway::0@v4"],
        CaseOutcome::PassExpectedError,
        "the runaway loop no longer trips the op budget, so it stops pinning \
         the per-run op counter reset",
    );

    // Re-running with the same config must also agree, which is the property
    // the nightly gate actually depends on.
    let again = run_manifest_with(&m, BACKENDS, RunConfig::default().with_jobs(8));
    assert_same_report(&parallel, &again, "jobs=8 twice");
}

#[test]
fn shards_partition_the_manifest() {
    let m = synthetic_manifest();
    let full = run_manifest_with(&m, BACKENDS, RunConfig::default().with_jobs(4));

    let shards: Vec<_> = (0..3)
        .map(|i| {
            let shard = Shard::new(i, 3).expect("valid shard");
            assert_eq!(shard.expected_len(&m), (m.cases.len() + 2 - i) / 3);
            run_manifest_with(
                &m,
                BACKENDS,
                RunConfig::default().with_jobs(2).with_shard(shard),
            )
        })
        .collect();

    for (name, whole) in &full.backends {
        let mut union: BTreeSet<&str> = BTreeSet::new();
        let mut cases = 0usize;
        for report in &shards {
            let part = &report.backends[name];
            assert_eq!(part.summary.total as usize, part.outcomes.len());
            cases += part.outcomes.len();
            for (id, outcome) in &part.outcomes {
                assert_eq!(
                    Some(outcome),
                    whole.outcomes.get(id),
                    "[{name}] {id} differs between the shard and the full run",
                );
                assert!(union.insert(id.as_str()), "[{name}] {id} is in two shards");
            }
        }
        assert_eq!(cases, whole.outcomes.len(), "[{name}] shards lost cases");
        assert_eq!(
            union,
            whole.outcomes.keys().map(String::as_str).collect(),
            "[{name}] the shards' union is not the full run",
        );
    }
}

#[test]
#[should_panic(expected = "duplicate case id")]
fn a_duplicate_case_id_is_rejected() {
    // Two cases, one id: the outcome map can only hold one of them, so which
    // outcome survives would depend on worker scheduling.
    let m = manifest(vec![
        case(
            "dup@v4",
            "return 1",
            Expectation::Equals { value: "1".into() },
        ),
        case(
            "dup@v4",
            "return 2",
            Expectation::Equals { value: "2".into() },
        ),
    ]);
    run_manifest_with(&m, &[SuiteBackend::Native], RunConfig::default());
}

#[test]
fn a_shard_must_be_in_range() {
    assert!(Shard::new(0, 1).is_some());
    assert!(Shard::new(2, 3).is_some());
    assert!(Shard::new(3, 3).is_none(), "index == count is out of range");
    assert!(Shard::new(0, 0).is_none(), "zero shards would run nothing");
    assert!(Shard::ALL.is_all());
    assert!(!Shard::new(1, 2).unwrap().is_all());
}
