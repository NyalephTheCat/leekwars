//! `check_native_compat` agrees with a real native compile (#173).
//!
//! The compat check is a *prediction*: "compiling this for native would
//! fail". A prediction is only useful if it is right on programs nobody wrote
//! to test it with, so this checks it against what the compile actually does,
//! over a sample of the upstream corpus.
//!
//! Both directions matter, and for different reasons:
//!
//! - a false positive (check complains, compile succeeds) puts a caret under
//!   working code and sends an author rewriting it — strictly worse than
//!   having no check;
//! - a false negative (check silent, compile fails) is the check failing to
//!   do its one job, and the author finds out when the fight starts.
//!
//! The sample is strided rather than a prefix: cases from one upstream test
//! file sit together and exercise the same constructs, so the first N cases
//! would cover a handful of features thoroughly and everything else not at
//! all.
//!
//! What this does and does not cover, measured rather than assumed: every
//! sampled case compiles today (0 of 1462 were rejected at a 1500-case
//! sample), so what this pins *at scale* is the false-positive direction —
//! the check staying silent on thousands of real programs. The other
//! direction is pinned per-construct in
//! `leek-backend-native/tests/native_compat.rs`, where each test asserts the
//! compile fails alongside the diagnostic the check produced.

use leek_backend_native::{NativeOptions, check_native_compat, compile_program};
use leek_hir::pipeline::HirArtifact;
use leek_pipeline::{FeatureFlags, Input};
use leek_session::{RecipeParams, Target};
use leek_span::SourceId;
use leek_test_cases::TestCase;
use leek_test_corpus::{embedded_manifest, run_on_large_stack};

/// How many cases to check. The check translates every reachable function and
/// the compile then also codegens them, so this is a per-case cost of
/// milliseconds; a few hundred keeps the test inside a normal `cargo test`
/// while still spanning every upstream test file.
const SAMPLE: usize = 400;

struct Disagreement {
    id: String,
    /// What the check said, when it spoke.
    check: Option<String>,
    /// What the compile said, when it failed.
    compile: Option<String>,
}

fn hir_of(case: &TestCase, source: SourceId) -> Option<std::sync::Arc<leek_hir::HirFile>> {
    let pipeline =
        leek_session::pipeline(Target::Hir, &RecipeParams::permissive()).expect("recipe");
    let run = pipeline.run(Input {
        source,
        text: case.code.clone().into(),
        version_byte: case.version,
        strict: case.strict,
        flags: FeatureFlags::from_env(),
    });
    // A case the frontend rejects never reaches the backend in anger, and the
    // two paths agree trivially on it (both report the lowering diagnostics).
    if run
        .diagnostics()
        .iter()
        .any(|d| d.severity == leek_diagnostics::Severity::Error)
    {
        return None;
    }
    run.get::<HirArtifact>()
        .map(|a| std::sync::Arc::clone(&a.0))
}

#[test]
fn the_check_and_the_compile_agree_on_the_corpus() {
    let manifest = embedded_manifest();
    assert!(
        manifest.cases.len() > 5_000,
        "no corpus to check against ({} cases) — the upstream submodule is \
         not initialized, and this test would pass vacuously",
        manifest.cases.len(),
    );
    let stride = manifest.cases.len() / SAMPLE;
    let sample: Vec<&TestCase> = manifest.cases.iter().step_by(stride.max(1)).collect();

    let (checked, disagreements) = run_on_large_stack("native-compat-agreement", move || {
        let source = SourceId::new(1).unwrap();
        let mut checked = 0usize;
        let mut out: Vec<Disagreement> = Vec::new();
        for case in sample {
            let Some(hir) = hir_of(case, source) else {
                continue;
            };
            let opts = NativeOptions::release().with_lang(case.version, case.strict);
            let predicted = check_native_compat(&hir, &opts);
            let compiled = compile_program(&hir, &opts);
            checked += 1;
            if predicted.is_empty() != compiled.is_ok() {
                out.push(Disagreement {
                    id: case.id.clone(),
                    check: predicted.first().map(|d| d.message.clone()),
                    compile: compiled.err().map(|e| e.to_string()),
                });
            }
        }
        (checked, out)
    });

    assert!(
        checked > SAMPLE / 2,
        "only {checked} of the sample produced HIR — the sampling is broken",
    );
    let mut listing = String::new();
    for d in disagreements.iter().take(20) {
        use std::fmt::Write as _;
        let _ = writeln!(
            listing,
            "  {}\n    check:   {}\n    compile: {}",
            d.id,
            d.check.as_deref().unwrap_or("(no diagnostic)"),
            d.compile.as_deref().unwrap_or("(compiled fine)"),
        );
    }
    assert!(
        disagreements.is_empty(),
        "{} of {checked} sampled cases disagree:\n{listing}",
        disagreements.len(),
    );
}
