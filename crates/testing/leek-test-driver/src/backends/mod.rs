//! Run upstream cases on every linked / enabled backend.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;

use leek_backends::version_from_byte;
use leek_diagnostics::Severity;
use leek_hir::pipeline::HirArtifact;
use leek_hir::HirFile;
use leek_manifest::{BackendKind, BackendTable};
use leek_parser::ast::{AstNode, SourceFile};
use leek_pipeline::Input;
use leek_recipes::{RecipeParams, Target};
use leek_span::SourceId;
use leek_syntax::SyntaxNode;
use serde::{Deserialize, Serialize};

use crate::cases::{Expectation, Manifest, TestCase};
use crate::checks::CheckKind;
use crate::run::CaseOutcome;

/// Corpus runner target — includes the shared pipeline plus each linked backend.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SuiteBackend {
    Pipeline,
    Interp,
    Java,
    Native,
}

impl SuiteBackend {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pipeline => "pipeline",
            Self::Interp => "interp",
            Self::Java => "java",
            Self::Native => "native",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "pipeline" => Self::Pipeline,
            "interp" => Self::Interp,
            "java" => Self::Java,
            "native" => Self::Native,
            _ => return None,
        })
    }

    fn from_manifest_kind(kind: BackendKind) -> Option<Self> {
        match kind {
            BackendKind::Interp => Some(Self::Interp),
            BackendKind::Java => Some(Self::Java),
            BackendKind::Native => Some(Self::Native),
            BackendKind::Jar | BackendKind::Wasm => None,
        }
    }
}

/// Backends to exercise: always `pipeline`, plus each [`leek_backends::LINKED`]
/// entry (and optional `[backend.*]` enable flags from `Miku.toml`).
pub fn detect_backends(table: Option<&BackendTable>) -> Vec<SuiteBackend> {
    let mut out = vec![SuiteBackend::Pipeline];
    for &kind in leek_backends::LINKED {
        let Some(sb) = SuiteBackend::from_manifest_kind(kind) else {
            continue;
        };
        let enabled = table
            .and_then(|t| t.get(kind))
            .is_none_or(|s| s.enable);
        if enabled && !out.contains(&sb) {
            out.push(sb);
        }
    }
    out
}

/// Per-backend reports keyed by [`SuiteBackend::as_str`].
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MultiReport {
    pub schema_version: u32,
    pub backends: BTreeMap<String, crate::run::Report>,
}

impl MultiReport {
    pub const SCHEMA_VERSION: u32 = 2;

    pub fn load(path: &Path) -> anyhow::Result<Self> {
        Ok(toml::from_str(&std::fs::read_to_string(path)?)?)
    }

    pub fn save(&self, path: &Path) -> anyhow::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, toml::to_string_pretty(self)?)?;
        Ok(())
    }

    pub fn diff_against(&self, baseline: &Self) -> MultiDiff {
        let mut diff = MultiDiff::default();
        for (name, report) in &self.backends {
            match baseline.backends.get(name) {
                Some(base) => {
                    let d = report.diff_against(base);
                    if !d.regressions.is_empty() {
                        diff.regressions.insert(name.clone(), d.regressions);
                    }
                    if !d.improvements.is_empty() {
                        diff.improvements.insert(name.clone(), d.improvements);
                    }
                }
                None => diff.new_backends.push(name.clone()),
            }
        }
        diff
    }
}

#[derive(Debug, Default)]
pub struct MultiDiff {
    pub regressions: BTreeMap<String, Vec<crate::run::Change>>,
    pub improvements: BTreeMap<String, Vec<crate::run::Change>>,
    pub new_backends: Vec<String>,
}

struct CaseContext {
    green: Option<rowan::GreenNode>,
    hir: Option<Arc<HirFile>>,
    has_compile_error: bool,
}

fn build_context(case: &TestCase, source: SourceId) -> CaseContext {
    let input = Input {
        source,
        text: case.code.clone().into(),
        version_byte: case.version,
        strict: case.strict,
        flags: leek_pipeline::FeatureFlags::from_env(),
    };
    let pipeline =
        leek_recipes::pipeline(Target::Hir, &RecipeParams::permissive()).expect("recipe");
    let run = pipeline.run(input);
    let has_compile_error = run
        .diagnostics()
        .iter()
        .any(|d| d.severity == Severity::Error);
    let green = run
        .get::<leek_parser::pipeline::GreenTreeArtifact>()
        .map(|g| g.0.clone());
    let hir = run.get::<HirArtifact>().map(|a| Arc::clone(&a.0));
    CaseContext {
        green,
        hir,
        has_compile_error,
    }
}

/// Run the full manifest on each detected backend (one pipeline build per case).
pub fn run_manifest(manifest: &Manifest, backends: &[SuiteBackend]) -> MultiReport {
    let src = SourceId::new(1).unwrap();
    let mut multi = MultiReport {
        schema_version: MultiReport::SCHEMA_VERSION,
        backends: BTreeMap::new(),
    };
    for &backend in backends {
        multi
            .backends
            .insert(backend.as_str().to_string(), crate::run::Report::default());
    }

    for case in &manifest.cases {
        if !case.enabled {
            for &backend in backends {
                let report = multi.backends.get_mut(backend.as_str()).expect("report");
                let outcome = CaseOutcome::SkippedDisabled;
                report.summary.record(outcome);
                report.outcomes.insert(case.id.clone(), outcome);
            }
            continue;
        }
        let ctx = build_context(case, src);
        for &backend in backends {
            let outcome = run_case_with_ctx(case, src, backend, &ctx);
            let report = multi.backends.get_mut(backend.as_str()).expect("report");
            report.summary.record(outcome);
            report.outcomes.insert(case.id.clone(), outcome);
        }
    }
    multi
}

/// Run one case on a single backend.
pub fn run_case_backend(case: &TestCase, source: SourceId, backend: SuiteBackend) -> CaseOutcome {
    if !case.enabled {
        return CaseOutcome::SkippedDisabled;
    }
    if !(1..=4).contains(&case.version) {
        return CaseOutcome::SkippedUnknown;
    }

    let ctx = build_context(case, source);
    run_case_with_ctx(case, source, backend, &ctx)
}

fn run_case_with_ctx(
    case: &TestCase,
    source: SourceId,
    backend: SuiteBackend,
    ctx: &CaseContext,
) -> CaseOutcome {
    match backend {
        SuiteBackend::Pipeline => run_pipeline(case, ctx, source),
        SuiteBackend::Interp => run_interp(case, ctx, source),
        SuiteBackend::Java => run_java(case, ctx),
        SuiteBackend::Native => run_native(case, ctx),
    }
}

/// Run a case on the native (Cranelift JIT) backend. The backend only
/// handles the scalar (integer / boolean) + control-flow subset so far,
/// so anything it can't compile reports `Unsupported` → we skip it. A
/// value mismatch on something it *did* compile is a real failure;
/// compile/runtime errors are skipped (treated as not-yet-supported)
/// while the backend matures.
/// Outcome of a native JIT run, distinguishing a real value from the two
/// non-value cases the caller must treat differently: an `Unsupported` error
/// (construct not in the compiled subset → skip) versus a *panic* during
/// codegen/execution, which is a genuine backend defect and must be a failure
/// — never a silent skip or, worse, a whole-suite abort.
enum NativeRun {
    Value(String),
    /// The compiled program trapped at runtime (`NativeError::Runtime`) —
    /// e.g. an array out-of-bounds write. Distinct from `Unsupported` so the
    /// harness can verify runtime-error expectations against it. The message
    /// is logged in [`native_run`]; only the *distinction* is needed here.
    RuntimeError,
    Unsupported,
    Panicked,
}

/// Run the native backend, converting any panic during JIT compilation or
/// execution into [`NativeRun::Panicked`]. Without this, a single panicking
/// case aborts the entire corpus worker (`run_on_large_stack` re-panics on
/// `join`), so a miscompile-via-panic would be invisible. Mirrors the
/// `catch_unwind` discipline in `leek-backend-java`'s parity tests.
fn native_run(
    case: &TestCase,
    hir: &leek_hir::HirFile,
    opts: &leek_backend_native::NativeOptions,
) -> NativeRun {
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        leek_backend_native::run(hir, opts)
    }));
    match result {
        Ok(Ok(v)) => NativeRun::Value(v.to_string()),
        Ok(Err(leek_backend_native::NativeError::Runtime(m))) => {
            // A clean compile that trapped at runtime. Log for triage; the
            // caller distinguishes this from `Unsupported` to verify
            // runtime-error expectations.
            eprintln!("native runtime error on case {}: {m}", case.id);
            NativeRun::RuntimeError
        }
        Ok(Err(_)) => NativeRun::Unsupported,
        Err(payload) => {
            let msg = payload
                .downcast_ref::<&str>()
                .map(|s| (*s).to_string())
                .or_else(|| payload.downcast_ref::<String>().cloned())
                .unwrap_or_else(|| "<non-string panic>".to_string());
            // Surface for the triage pass — a native panic is a real defect.
            eprintln!("native backend panicked on case {}: {msg}", case.id);
            NativeRun::Panicked
        }
    }
}

fn run_native(case: &TestCase, ctx: &CaseContext) -> CaseOutcome {
    if !case
        .check_plan()
        .kinds
        .iter()
        .any(|k| matches!(k, CheckKind::NativeRun))
    {
        return CaseOutcome::SkippedUnknown;
    }

    // Structural cases (compile/runtime errors) are verified before we touch
    // the JIT: an erroneous program has no valid HIR for native to run, and a
    // compile error is produced by the shared frontend native depends on.
    if case.expected.implies_error() {
        return native_error_outcome(case, ctx);
    }
    // Any other expectation that nonetheless failed to compile is a real
    // regression — but native shares the frontend, so defer that signal to the
    // pipeline backend (which owns compile-error reporting) and skip here.
    if ctx.has_compile_error {
        return CaseOutcome::SkippedUnknown;
    }
    let Some(hir) = ctx.hir.as_deref() else {
        return CaseOutcome::SkippedUnknown;
    };

    leek_backend_interp::value::DISPLAY_VERSION.with(|c| c.set(case.version));
    let opts = leek_backend_native::NativeOptions::release().with_lang(case.version, case.strict);

    match &case.expected {
        // Exact value match.
        Expectation::Equals { value } => match native_run(case, hir, &opts) {
            NativeRun::Value(got) => {
                if got == *value {
                    CaseOutcome::Pass
                } else {
                    CaseOutcome::FailWrongValue
                }
            }
            // Not in the supported subset yet, or no compile-error
            // model — skip rather than count as a failure.
            NativeRun::Unsupported => CaseOutcome::SkippedUnknown,
            // A value was expected but the program trapped, or the JIT
            // panicked — both are real defects, not skips.
            NativeRun::RuntimeError | NativeRun::Panicked => CaseOutcome::FailWrongValue,
        },
        // Approximate float match (upstream `.almost(X)` — loose tolerance).
        Expectation::Almost { value } => match native_run(case, hir, &opts) {
            NativeRun::Value(got) => match native_almost_matches(&got, value) {
                Some(true) => CaseOutcome::Pass,
                Some(false) => CaseOutcome::FailWrongValue,
                // Expected is a Java expression (e.g. `Math.PI`) we can't
                // evaluate here — skip rather than false-pass/false-fail.
                None => CaseOutcome::SkippedUnknown,
            },
            NativeRun::Unsupported => CaseOutcome::SkippedUnknown,
            NativeRun::RuntimeError | NativeRun::Panicked => CaseOutcome::FailWrongValue,
        },
        // `.equalsOps("value", N)` — native charges ops at the same MIR sites
        // as the interp, so it verifies BOTH the value and the operation count.
        Expectation::EqualsOps { .. } | Expectation::Unknown { .. } => {
            let Some((value, ops)) = equals_ops_expectation(case) else {
                return CaseOutcome::SkippedUnknown;
            };
            match native_run(case, hir, &opts) {
                NativeRun::Value(got) => {
                    if got == value && leek_backend_native::ops_used() == ops {
                        CaseOutcome::Pass
                    } else {
                        CaseOutcome::FailWrongValue
                    }
                }
                NativeRun::Unsupported => CaseOutcome::SkippedUnknown,
                NativeRun::RuntimeError | NativeRun::Panicked => CaseOutcome::FailWrongValue,
            }
        }
        // No error expected (`Error{NONE}`) or a warning/no-warning case: the
        // program compiles cleanly, so native must run it without trapping. We
        // don't verify the warning *code* (that is a frontend diagnostic the
        // resolver/typechecker own) — only that the compiled program executes
        // cleanly, mirroring the interp path.
        Expectation::Error { .. } | Expectation::Warning { .. } | Expectation::NoWarning => {
            match native_run(case, hir, &opts) {
                NativeRun::Value(_) => CaseOutcome::Pass,
                // Not in the compiled subset yet — skip, don't fail.
                NativeRun::Unsupported => CaseOutcome::SkippedUnknown,
                // A clean program trapped or panicked on native — a real defect.
                NativeRun::RuntimeError | NativeRun::Panicked => CaseOutcome::FailWrongValue,
            }
        }
        // `.ops(N)` carries only an operation count. Native charges ops at the
        // same MIR sites as the interp (plus the `leek-charge` static charges),
        // so it runs the program (unbounded op budget, so it completes) and
        // compares its charged total to the expected count.
        Expectation::Ops { count } => match native_run(case, hir, &opts) {
            NativeRun::Value(_) => {
                if leek_backend_native::ops_used() == *count {
                    CaseOutcome::Pass
                } else {
                    CaseOutcome::FailWrongValue
                }
            }
            NativeRun::Unsupported => CaseOutcome::SkippedUnknown,
            NativeRun::RuntimeError | NativeRun::Panicked => CaseOutcome::FailWrongValue,
        },
        // `AnyError` is reached only via a frontend leniency gap (handled by
        // `native_error_outcome`); nothing to verify here.
        Expectation::AnyError => CaseOutcome::SkippedUnknown,
    }
}

/// Verify an upstream *error* expectation (`Error{code != "NONE"}` or
/// `AnyError`) against the native backend. Compile errors are produced by the
/// shared frontend native depends on, so a present compile diagnostic means
/// native correctly refuses the program → `PassExpectedError` (mirrors
/// [`compile_error_outcome`], which the pipeline backend uses). Runtime-error
/// codes (`TOO_MUCH_OPERATIONS`, `ARRAY_OUT_OF_BOUND`, …) have no compile
/// diagnostic; verifying those means *running* the program and observing a
/// trap, which is deferred to a later phase (running an unbounded
/// `TOO_MUCH_OPERATIONS` loop on the JIT, which has no op limit, would hang).
fn native_error_outcome(case: &TestCase, ctx: &CaseContext) -> CaseOutcome {
    if ctx.has_compile_error {
        return CaseOutcome::PassExpectedError;
    }
    // Runtime-error codes have no compile diagnostic — verifying them means
    // *running* the program and observing a fault. Native now charges ops and,
    // under a finite op budget, stops a runaway loop at a back-edge — so the
    // resource-exhaustion codes (`TOO_MUCH_OPERATIONS` / `OUT_OF_MEMORY`) trip
    // the budget instead of spinning, and `ARRAY_OUT_OF_BOUND` faults on the
    // bad write. The upstream harness accepts *any* runtime error for these.
    if let Expectation::Error { code } = &case.expected
        && is_runtime_error_code(code)
        && let Some(hir) = ctx.hir.as_deref()
    {
        leek_backend_interp::value::DISPLAY_VERSION.with(|c| c.set(case.version));
        // A *small* op budget bounds execution tightly: a runaway loop trips it
        // (recording TOO_MUCH_OPERATIONS) within a few thousand iterations. This
        // matters because native leaks intermediate composite values (handles
        // are freed only at the result boundary), so an `a += i` concat loop
        // accumulates memory until it stops — a low cap keeps that bounded. The
        // harness accepts *any* runtime error here, so the exact budget is
        // immaterial as long as the loop faults.
        let opts = leek_backend_native::NativeOptions::release()
            .with_lang(case.version, case.strict)
            .with_op_limit(10_000);
        return match native_run(case, hir, &opts) {
            NativeRun::RuntimeError => CaseOutcome::PassExpectedError,
            // Native ran to completion without faulting, couldn't compile, or
            // panicked — don't claim a pass; skip rather than fail.
            _ => CaseOutcome::SkippedUnknown,
        };
    }
    CaseOutcome::SkippedUnknown
}

/// Compare a native runtime value's string form against an upstream
/// `.almost(X)` expectation. Mirrors [`crate::run::check_almost`]: parse
/// both as `f64` (normalizing v1 comma decimals, stripping Java numeric
/// suffixes) and accept within a loose relative tolerance.
///
/// Returns `Some(true)`/`Some(false)` for a real verdict, or `None` when
/// the expected side is something we can't evaluate here — the caller
/// skips those rather than guessing.
///
/// Handles plain floats, the two-arg `value, delta` form, and the small
/// Java math grammar upstream uses (`Math.PI / 2`, `Math.sqrt(2)`,
/// `-3 * Math.PI / 4`, …) via [`eval_java_math`].
fn native_almost_matches(got: &str, expected_str: &str) -> Option<bool> {
    let normalized = got.replace(',', ".");
    let got_f = normalized.parse::<f64>().ok()?;
    let (expected, explicit_delta) = eval_java_almost_expected(expected_str)?;
    // Upstream's default tolerance is loose-relative; an explicit second
    // argument overrides it.
    let tol = explicit_delta.unwrap_or_else(|| 1e-9_f64.max(expected.abs() * 1e-9));
    Some((got_f - expected).abs() <= tol)
}

/// Parse an upstream `.almost(...)` argument list into `(value, delta?)`.
/// The list is either a single expression or `value, delta`. Shared with the
/// interpreter/pipeline `check_almost` path so both backends evaluate the
/// expected side identically (and fail-closed when it can't be evaluated).
pub(crate) fn eval_java_almost_expected(s: &str) -> Option<(f64, Option<f64>)> {
    let parts = split_top_level_commas(s.trim());
    match parts.as_slice() {
        [v] => Some((eval_java_math(v)?, None)),
        [v, d] => Some((eval_java_math(v)?, Some(eval_java_math(d)?))),
        _ => None,
    }
}

/// Split on commas that sit at paren-depth 0 (so `Math.pow(2, 3)` stays
/// whole but `12.0, 1e-14` splits into two).
fn split_top_level_commas(s: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut depth = 0i32;
    let mut start = 0usize;
    for (i, c) in s.char_indices() {
        match c {
            '(' => depth += 1,
            ')' => depth -= 1,
            ',' if depth == 0 => {
                out.push(s[start..i].trim());
                start = i + 1;
            }
            _ => {}
        }
    }
    out.push(s[start..].trim());
    out
}

/// Evaluate the bounded Java math grammar used by upstream `.almost(...)`
/// expectations: float/int literals, `Math.PI`/`Math.E`, `Math.fn(args)`
/// calls, parenthesised groups, unary `±`, and `+ - * /`. Returns `None`
/// for anything outside the grammar so the caller can skip rather than
/// fabricate a verdict.
fn eval_java_math(s: &str) -> Option<f64> {
    let tokens = lex_java_math(s)?;
    let mut p = MathParser { tokens: &tokens, pos: 0 };
    let v = p.expr()?;
    if p.pos == p.tokens.len() { Some(v) } else { None }
}

#[derive(Debug, Clone, PartialEq)]
enum MathTok {
    Num(f64),
    Ident(String),
    Plus,
    Minus,
    Star,
    Slash,
    LParen,
    RParen,
    Comma,
}

fn lex_java_math(s: &str) -> Option<Vec<MathTok>> {
    let chars: Vec<char> = s.chars().collect();
    let mut i = 0;
    let mut out = Vec::new();
    while i < chars.len() {
        let c = chars[i];
        if c.is_ascii_whitespace() {
            i += 1;
        } else if c == '+' {
            out.push(MathTok::Plus);
            i += 1;
        } else if c == '-' {
            out.push(MathTok::Minus);
            i += 1;
        } else if c == '*' {
            out.push(MathTok::Star);
            i += 1;
        } else if c == '/' {
            out.push(MathTok::Slash);
            i += 1;
        } else if c == '(' {
            out.push(MathTok::LParen);
            i += 1;
        } else if c == ')' {
            out.push(MathTok::RParen);
            i += 1;
        } else if c == ',' {
            out.push(MathTok::Comma);
            i += 1;
        } else if c.is_ascii_digit() || c == '.' {
            let start = i;
            while i < chars.len() && (chars[i].is_ascii_digit() || chars[i] == '.') {
                i += 1;
            }
            // Optional exponent: e[+/-]digits
            if i < chars.len() && (chars[i] == 'e' || chars[i] == 'E') {
                i += 1;
                if i < chars.len() && (chars[i] == '+' || chars[i] == '-') {
                    i += 1;
                }
                while i < chars.len() && chars[i].is_ascii_digit() {
                    i += 1;
                }
            }
            // Drop a trailing Java numeric suffix (f/F/d/D/l/L).
            let lit: String = chars[start..i].iter().collect();
            if i < chars.len() && matches!(chars[i], 'f' | 'F' | 'd' | 'D' | 'l' | 'L') {
                i += 1;
            }
            out.push(MathTok::Num(lit.parse::<f64>().ok()?));
        } else if c.is_ascii_alphabetic() {
            // Identifier may contain dots (`Math.PI`, `Math.cos`).
            let start = i;
            while i < chars.len() && (chars[i].is_ascii_alphanumeric() || chars[i] == '.') {
                i += 1;
            }
            out.push(MathTok::Ident(chars[start..i].iter().collect()));
        } else {
            return None;
        }
    }
    Some(out)
}

struct MathParser<'a> {
    tokens: &'a [MathTok],
    pos: usize,
}

impl MathParser<'_> {
    fn peek(&self) -> Option<&MathTok> {
        self.tokens.get(self.pos)
    }
    fn bump(&mut self) -> Option<&MathTok> {
        let t = self.tokens.get(self.pos);
        if t.is_some() {
            self.pos += 1;
        }
        t
    }
    fn eat(&mut self, t: &MathTok) -> Option<()> {
        if self.peek() == Some(t) {
            self.pos += 1;
            Some(())
        } else {
            None
        }
    }

    fn expr(&mut self) -> Option<f64> {
        let mut acc = self.term()?;
        while let Some(t) = self.peek() {
            match t {
                MathTok::Plus => {
                    self.pos += 1;
                    acc += self.term()?;
                }
                MathTok::Minus => {
                    self.pos += 1;
                    acc -= self.term()?;
                }
                _ => break,
            }
        }
        Some(acc)
    }

    fn term(&mut self) -> Option<f64> {
        let mut acc = self.factor()?;
        while let Some(t) = self.peek() {
            match t {
                MathTok::Star => {
                    self.pos += 1;
                    acc *= self.factor()?;
                }
                MathTok::Slash => {
                    self.pos += 1;
                    acc /= self.factor()?;
                }
                _ => break,
            }
        }
        Some(acc)
    }

    fn factor(&mut self) -> Option<f64> {
        match self.peek()? {
            MathTok::Minus => {
                self.pos += 1;
                Some(-self.factor()?)
            }
            MathTok::Plus => {
                self.pos += 1;
                self.factor()
            }
            _ => self.primary(),
        }
    }

    fn primary(&mut self) -> Option<f64> {
        match self.bump()?.clone() {
            MathTok::Num(n) => Some(n),
            MathTok::LParen => {
                let v = self.expr()?;
                self.eat(&MathTok::RParen)?;
                Some(v)
            }
            MathTok::Ident(name) => {
                // A function call if followed by `(`, otherwise a constant.
                if self.eat(&MathTok::LParen).is_some() {
                    let mut args = vec![self.expr()?];
                    while self.eat(&MathTok::Comma).is_some() {
                        args.push(self.expr()?);
                    }
                    self.eat(&MathTok::RParen)?;
                    eval_math_fn(&name, &args)
                } else {
                    eval_math_const(&name)
                }
            }
            _ => None,
        }
    }
}

fn eval_math_const(name: &str) -> Option<f64> {
    match name {
        "Math.PI" => Some(std::f64::consts::PI),
        "Math.E" => Some(std::f64::consts::E),
        _ => None,
    }
}

fn eval_math_fn(name: &str, args: &[f64]) -> Option<f64> {
    let one = |f: fn(f64) -> f64| (args.len() == 1).then(|| f(args[0]));
    match name {
        "Math.cos" => one(f64::cos),
        "Math.sin" => one(f64::sin),
        "Math.tan" => one(f64::tan),
        "Math.acos" => one(f64::acos),
        "Math.asin" => one(f64::asin),
        "Math.atan" => one(f64::atan),
        "Math.cosh" => one(f64::cosh),
        "Math.sinh" => one(f64::sinh),
        "Math.tanh" => one(f64::tanh),
        "Math.sqrt" => one(f64::sqrt),
        "Math.cbrt" => one(f64::cbrt),
        "Math.abs" => one(f64::abs),
        "Math.exp" => one(f64::exp),
        "Math.log" => one(f64::ln),
        "Math.log10" => one(f64::log10),
        "Math.ceil" => one(f64::ceil),
        "Math.floor" => one(f64::floor),
        "Math.signum" => one(f64::signum),
        "Math.toRadians" => one(f64::to_radians),
        "Math.toDegrees" => one(f64::to_degrees),
        "Math.pow" if args.len() == 2 => Some(args[0].powf(args[1])),
        "Math.atan2" if args.len() == 2 => Some(args[0].atan2(args[1])),
        "Math.hypot" if args.len() == 2 => Some(args[0].hypot(args[1])),
        "Math.min" if args.len() == 2 => Some(args[0].min(args[1])),
        "Math.max" if args.len() == 2 => Some(args[0].max(args[1])),
        _ => None,
    }
}

/// One row of the native skip-reason histogram: a normalized reason, how
/// many cases hit it, and one sample snippet.
#[derive(Debug, Clone)]
pub struct NativeSkipRow {
    pub reason: String,
    pub count: u32,
    pub sample: String,
}

/// Histogram of *why* native skips cases it attempts (enabled `Equals`
/// cases that compile to HIR). Drives coverage-growth decisions: the
/// biggest buckets are the highest-value features to add next. Rows are
/// returned sorted by descending count.
pub fn native_skip_histogram(manifest: &Manifest) -> Vec<NativeSkipRow> {
    let src = SourceId::new(1).unwrap();
    let mut hist: BTreeMap<String, (u32, String)> = BTreeMap::new();
    for case in &manifest.cases {
        if !case.enabled || !matches!(case.expected, Expectation::Equals { .. }) {
            continue;
        }
        let ctx = build_context(case, src);
        if ctx.has_compile_error {
            continue;
        }
        let Some(hir) = ctx.hir.as_deref() else {
            continue;
        };
        let opts = leek_backend_native::NativeOptions::release().with_lang(case.version, case.strict);
        if let Err(e) = leek_backend_native::run(hir, &opts) {
            let reason = normalize_native_skip(&e);
            let entry = hist.entry(reason).or_insert((0, case.code.clone()));
            entry.0 += 1;
        }
    }
    let mut rows: Vec<NativeSkipRow> = hist
        .into_iter()
        .map(|(reason, (count, sample))| NativeSkipRow {
            reason,
            count,
            sample,
        })
        .collect();
    rows.sort_by(|a, b| b.count.cmp(&a.count).then(a.reason.cmp(&b.reason)));
    rows
}

/// A single skipped case dumped for triage: full source, the normalized
/// skip reason, and the expected value.
#[derive(Debug, Clone)]
pub struct NativeSkipCase {
    pub reason: String,
    pub expected: String,
    pub code: String,
}

/// Every attempted `Equals` case whose normalized native skip reason
/// contains `filter` (case-insensitive substring) — with full source, for
/// hands-on triage of a histogram bucket.
pub fn native_skips_matching(manifest: &Manifest, filter: &str) -> Vec<NativeSkipCase> {
    let src = SourceId::new(1).unwrap();
    let needle = filter.to_lowercase();
    let mut out = Vec::new();
    for case in &manifest.cases {
        let Expectation::Equals { value } = &case.expected else {
            continue;
        };
        if !case.enabled {
            continue;
        }
        let ctx = build_context(case, src);
        if ctx.has_compile_error {
            continue;
        }
        let Some(hir) = ctx.hir.as_deref() else {
            continue;
        };
        let opts = leek_backend_native::NativeOptions::release().with_lang(case.version, case.strict);
        if let Err(e) = leek_backend_native::run(hir, &opts) {
            let reason = normalize_native_skip(&e);
            if reason.to_lowercase().contains(&needle) {
                out.push(NativeSkipCase {
                    reason,
                    expected: value.clone(),
                    code: case.code.clone(),
                });
            }
        }
    }
    out
}

/// Collapse a [`leek_backend_native::NativeError`] into a stable bucket
/// key by dropping the case-specific tail (identifiers, `Debug` payloads).
fn normalize_native_skip(e: &leek_backend_native::NativeError) -> String {
    let msg = e.to_string();
    // Drop the error-kind prefix ("unsupported: ", "compile error: ", …)
    // to get the bare reason.
    let reason = msg.split_once(": ").map_or(&*msg, |(_, rest)| rest);
    // Drop any case-specific tail (a concrete identifier or `Debug` blob)
    // so similar reasons bucket together.
    let head = reason.split([':', '(']).next().unwrap_or(reason).trim();
    let words: Vec<&str> = head.split_whitespace().collect();
    match words.as_slice() {
        ["builtin", _rest @ ..] => "builtin <name>".to_string(),
        ["assign", "to", _rest @ ..] => "assign to <place>".to_string(),
        ["const", _rest @ ..] => "const <other>".to_string(),
        ["rvalue", kind, ..] => format!("rvalue {kind}"),
        ["real", "binary", "op", ..] => "real binary op <other>".to_string(),
        ["binary", "op", ..] => "binary op <other>".to_string(),
        [name, "expected", "1", "arg"] => format!("{name}: arity != 1"),
        _ => head.to_string(),
    }
}

fn is_runtime_error_code(code: &str) -> bool {
    matches!(
        code,
        "TOO_MUCH_OPERATIONS" | "OUT_OF_MEMORY" | "ARRAY_OUT_OF_BOUND" | "STACK_OVERFLOW"
    )
}

fn compile_error_outcome(case: &TestCase, ctx: &CaseContext) -> CaseOutcome {
    if ctx.has_compile_error {
        return CaseOutcome::PassExpectedError;
    }
    if let Expectation::Error { code } = &case.expected
        && is_runtime_error_code(code)
        && let Some(hir) = ctx.hir.as_deref()
    {
        let run = leek_backend_interp::run_with_limit_version_strict(
            hir,
            200_000,
            case.version,
            case.strict,
        );
        if run.error.is_some() {
            return CaseOutcome::PassExpectedError;
        }
    }
    CaseOutcome::FailMissingError
}

fn equals_ops_expectation(case: &TestCase) -> Option<(String, u64)> {
    match &case.expected {
        Expectation::EqualsOps { value, count } => Some((value.clone(), *count)),
        Expectation::Unknown { detail } if detail == "equalsOps" => {
            parse_equals_ops_java_line(&case.java_line)
        }
        _ => None,
    }
}

/// Legacy manifest rows stored as `unknown` + `equalsOps` detail.
fn parse_equals_ops_java_line(java_line: &str) -> Option<(String, u64)> {
    let after = java_line.find(".equalsOps(")? + ".equalsOps(".len();
    let rest = java_line.get(after..)?;
    let end = rest.find(')')?;
    let inner = &rest[..end];
    let value = parse_java_string_literal(inner)?;
    let tail = tail_after_first_string_literal(inner)?;
    let count = tail
        .trim()
        .strip_prefix(',')?
        .trim()
        .trim_end_matches('L')
        .parse::<u64>()
        .ok()?;
    Some((value, count))
}

fn parse_java_string_literal(s: &str) -> Option<String> {
    let t = s.trim();
    if !t.starts_with('"') {
        return None;
    }
    let mut out = String::new();
    let mut chars = t[1..].chars();
    while let Some(c) = chars.next() {
        match c {
            '\\' => out.push(chars.next()?),
            '"' => return Some(out),
            other => out.push(other),
        }
    }
    None
}

fn tail_after_first_string_literal(s: &str) -> Option<&str> {
    let mut chars = s.char_indices().peekable();
    while chars.peek().is_some_and(|(_, c)| c.is_ascii_whitespace()) {
        chars.next();
    }
    if chars.peek().map(|(_, c)| *c) != Some('"') {
        return None;
    }
    chars.next();
    while let Some((i, c)) = chars.next() {
        match c {
            '\\' => {
                chars.next();
            }
            '"' => return s.get(i + 1..),
            _ => {}
        }
    }
    None
}

fn run_pipeline(case: &TestCase, ctx: &CaseContext, source: SourceId) -> CaseOutcome {
    let plan = case.check_plan();
    if !plan.kinds.iter().any(|k| {
        matches!(
            k,
            CheckKind::Parse
                | CheckKind::Resolve
                | CheckKind::Typecheck
                | CheckKind::Hir
        )
    }) {
        return CaseOutcome::SkippedUnknown;
    }

    let Some(green) = ctx.green.clone() else {
        return if case.expected.implies_error() {
            CaseOutcome::PassExpectedError
        } else {
            CaseOutcome::FailParseError
        };
    };
    if case.expected.implies_error() {
        return compile_error_outcome(case, ctx);
    }

    if let Some((ref value, count)) = equals_ops_expectation(case) {
        let Some(green) = ctx.green.clone() else {
            return CaseOutcome::FailParseError;
        };
        let sf = SourceFile::cast(SyntaxNode::new_root(green));
        if let Some(file) = sf
            && crate::run::check_equals_ops(&file, source, case, value, count)
        {
            return CaseOutcome::Pass;
        }
        return CaseOutcome::FailWrongValue;
    }

    if !case.expected.implies_clean_parse() {
        return CaseOutcome::SkippedUnknown;
    }

    if ctx.has_compile_error {
        return CaseOutcome::FailParseError;
    }

    let sf = SourceFile::cast(SyntaxNode::new_root(green));
    match (&case.expected, sf) {
        // Verify the value, not just that the program compiled — otherwise an
        // `Equals` case passes on clean compilation alone (a false pass).
        (Expectation::Equals { value }, Some(file)) => {
            if crate::run::check_equals(&file, source, case, value) {
                CaseOutcome::Pass
            } else {
                CaseOutcome::FailWrongValue
            }
        }
        (Expectation::Almost { value }, Some(file)) => {
            match crate::run::check_almost(&file, source, case, value) {
                Some(true) => CaseOutcome::Pass,
                Some(false) => CaseOutcome::FailWrongValue,
                // Expected side can't be evaluated here — skip, don't false-pass.
                None => CaseOutcome::SkippedUnknown,
            }
        }
        (Expectation::Ops { count }, Some(file)) => {
            if crate::run::check_ops(&file, source, case, *count) {
                CaseOutcome::Pass
            } else {
                CaseOutcome::FailWrongValue
            }
        }
        _ => CaseOutcome::Pass,
    }
}

fn run_interp(case: &TestCase, ctx: &CaseContext, source: SourceId) -> CaseOutcome {
    if !case
        .check_plan()
        .kinds
        .iter()
        .any(|k| matches!(k, CheckKind::InterpRun | CheckKind::InterpValue | CheckKind::InterpOps))
    {
        return CaseOutcome::SkippedUnknown;
    }

    if case.expected.implies_error() {
        return compile_error_outcome(case, ctx);
    }

    if let Some((ref value, count)) = equals_ops_expectation(case) {
        let Some(green) = ctx.green.clone() else {
            return CaseOutcome::FailParseError;
        };
        let sf = SourceFile::cast(SyntaxNode::new_root(green));
        if let Some(file) = sf
            && crate::run::check_equals_ops(&file, source, case, value, count)
        {
            return CaseOutcome::Pass;
        }
        return CaseOutcome::FailWrongValue;
    }

    if ctx.has_compile_error {
        return CaseOutcome::FailParseError;
    }

    let Some(hir) = ctx.hir.as_deref() else {
        return CaseOutcome::FailParseError;
    };

    leek_backend_interp::value::DISPLAY_VERSION.with(|c| c.set(case.version));

    match &case.expected {
        Expectation::Equals { value } => {
            let run =
                leek_backend_interp::run_with_limit_version_strict(hir, 100_000_000, case.version, case.strict);
            if run.error.is_some() {
                return CaseOutcome::FailWrongValue;
            }
            let got = run.value.to_string();
            if got == *value {
                CaseOutcome::Pass
            } else {
                CaseOutcome::FailWrongValue
            }
        }
        Expectation::Almost { value } => {
            let Some(green) = ctx.green.clone() else {
                return CaseOutcome::FailParseError;
            };
            match SourceFile::cast(SyntaxNode::new_root(green))
                .and_then(|file| crate::run::check_almost(&file, source, case, value))
            {
                Some(true) => CaseOutcome::Pass,
                Some(false) => CaseOutcome::FailWrongValue,
                // Unparseable tree or unevaluable expected → skip, don't false-pass.
                None => CaseOutcome::SkippedUnknown,
            }
        }
        Expectation::Ops { count } => {
            let Some(green) = ctx.green.clone() else {
                return CaseOutcome::FailParseError;
            };
            let sf = SourceFile::cast(SyntaxNode::new_root(green));
            if let Some(file) = sf
                && crate::run::check_ops(&file, source, case, *count)
            {
                CaseOutcome::Pass
            } else {
                CaseOutcome::FailWrongValue
            }
        }
        Expectation::Error { code } if code == "NONE" => {
            let run =
                leek_backend_interp::run_with_limit_version_strict(hir, 100_000_000, case.version, case.strict);
            if run.error.is_none() {
                CaseOutcome::Pass
            } else {
                CaseOutcome::FailWrongValue
            }
        }
        Expectation::Warning { .. } | Expectation::NoWarning => {
            let run =
                leek_backend_interp::run_with_limit_version_strict(hir, 100_000_000, case.version, case.strict);
            if run.error.is_none() {
                CaseOutcome::Pass
            } else {
                CaseOutcome::FailWrongValue
            }
        }
        Expectation::Error { .. }
        | Expectation::AnyError
        | Expectation::Unknown { .. }
        | Expectation::EqualsOps { .. } => CaseOutcome::SkippedUnknown,
    }
}

/// A coarse bucket for a failing case, derived from the case's
/// expectation plus the observed [`CaseOutcome`]. Used by the
/// `failures` subcommand to group failures into a readable table.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum FailureCategory {
    /// Expected to run, but the pipeline rejected it (parse / resolve /
    /// type error). "Can't run the code."
    WontCompile,
    /// Expected a compile/runtime error; none was produced.
    MissingError,
    /// `.equals(X)` value mismatch.
    ValueEquals,
    /// `.almost(X)` numeric mismatch.
    ValueAlmost,
    /// `.ops(N)` operation-count mismatch.
    Ops,
    /// `.equalsOps(X, N)` value-or-ops mismatch.
    EqualsOps,
    /// Wrong value with an expectation kind that doesn't fit above.
    OtherWrong,
}

impl FailureCategory {
    /// All variants, in display order.
    pub const ALL: [FailureCategory; 7] = [
        Self::WontCompile,
        Self::MissingError,
        Self::ValueEquals,
        Self::ValueAlmost,
        Self::Ops,
        Self::EqualsOps,
        Self::OtherWrong,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::WontCompile => "won't compile",
            Self::MissingError => "missing error",
            Self::ValueEquals => "value (equals)",
            Self::ValueAlmost => "value (almost)",
            Self::Ops => "ops count",
            Self::EqualsOps => "value+ops",
            Self::OtherWrong => "other wrong value",
        }
    }
}

/// Classify a failing outcome into a [`FailureCategory`]. Returns
/// `None` for outcomes that are not failures (pass / skip).
pub fn categorize_failure(outcome: CaseOutcome, expected: &Expectation) -> Option<FailureCategory> {
    Some(match outcome {
        CaseOutcome::FailParseError => FailureCategory::WontCompile,
        CaseOutcome::FailMissingError => FailureCategory::MissingError,
        CaseOutcome::FailWrongValue => match expected {
            Expectation::Equals { .. } => FailureCategory::ValueEquals,
            Expectation::Almost { .. } => FailureCategory::ValueAlmost,
            Expectation::Ops { .. } => FailureCategory::Ops,
            Expectation::EqualsOps { .. } => FailureCategory::EqualsOps,
            Expectation::Unknown { detail } if detail == "equalsOps" => FailureCategory::EqualsOps,
            _ => FailureCategory::OtherWrong,
        },
        _ => return None,
    })
}

/// Expected value, rendered for the failures table.
pub fn expected_display(e: &Expectation) -> String {
    match e {
        Expectation::Equals { value } => value.clone(),
        Expectation::Almost { value } => format!("≈ {value}"),
        Expectation::Ops { count } => format!("ops={count}"),
        Expectation::EqualsOps { value, count } => format!("{value} (ops={count})"),
        Expectation::Error { code } => format!("error {code}"),
        Expectation::AnyError => "<any error>".into(),
        Expectation::Warning { code } => format!("warning {code}"),
        Expectation::NoWarning => "<no warning>".into(),
        Expectation::Unknown { detail } => format!("unknown({detail})"),
    }
}

/// Expected vs. actual probe for a single failing case, rendered as
/// human-readable strings. Re-runs the relevant backend, so call only
/// for the (few) cases that actually fail.
pub struct CaseProbe {
    pub expected: String,
    pub actual: String,
}

/// Build [`CaseProbe`] strings for `case` on `backend`. The `actual`
/// side mirrors what the corpus runner observed: a compile error, an
/// interpreter value/error/ops count, or a Java-emit summary.
pub fn probe_case(case: &TestCase, source: SourceId, backend: SuiteBackend) -> CaseProbe {
    CaseProbe {
        expected: expected_display(&case.expected),
        actual: actual_display(case, source, backend),
    }
}

fn first_error_message(case: &TestCase, source: SourceId) -> String {
    let input = Input {
        source,
        text: case.code.clone().into(),
        version_byte: case.version,
        strict: case.strict,
        flags: leek_pipeline::FeatureFlags::from_env(),
    };
    let Ok(pipeline) = leek_recipes::pipeline(Target::Hir, &RecipeParams::permissive()) else {
        return "<pipeline build failed>".into();
    };
    pipeline
        .run(input)
        .diagnostics()
        .iter()
        .find(|d| d.severity == Severity::Error)
        .map_or_else(|| "<no error message>".into(), |d| format!("[{}] {}", d.code.0, d.message))
}

fn actual_display(case: &TestCase, source: SourceId, backend: SuiteBackend) -> String {
    let ctx = build_context(case, source);
    if ctx.has_compile_error {
        return format!("compile error: {}", first_error_message(case, source));
    }
    let Some(hir) = ctx.hir.as_deref() else {
        return "<no HIR produced>".into();
    };
    leek_backend_interp::value::DISPLAY_VERSION.with(|c| c.set(case.version));
    match backend {
        SuiteBackend::Interp => match &case.expected {
            Expectation::Ops { .. } => {
                let (_r, used) =
                    leek_backend_interp::run_with_ops_used(hir, 100_000_000, case.version);
                format!("ops={used}")
            }
            Expectation::EqualsOps { .. }
            | Expectation::Unknown { .. } => {
                let (r, used) =
                    leek_backend_interp::run_with_ops_used(hir, 100_000_000, case.version);
                match r.error {
                    Some(e) => format!("error: {e} (ops={used})"),
                    None => format!("{} (ops={used})", r.value),
                }
            }
            _ => {
                let r = leek_backend_interp::run_with_limit_version_strict(
                    hir,
                    100_000_000,
                    case.version,
                    case.strict,
                );
                match r.error {
                    Some(e) => format!("error: {e}"),
                    None => r.value.to_string(),
                }
            }
        },
        SuiteBackend::Java => {
            let version = version_from_byte(case.version);
            let emitted = leek_backend_java::emit_clean(hir, version, 1);
            if emitted.java.contains("class AI_") && !emitted.java.is_empty() {
                "emit ok".into()
            } else {
                "emit failed".into()
            }
        }
        SuiteBackend::Native => {
            let opts = leek_backend_native::NativeOptions::release()
                .with_lang(case.version, case.strict);
            match leek_backend_native::run(hir, &opts) {
                Ok(v) => v.to_string(),
                Err(e) => format!("{e}"),
            }
        }
        SuiteBackend::Pipeline => "compiled cleanly".into(),
    }
}

fn run_java(case: &TestCase, ctx: &CaseContext) -> CaseOutcome {
    if !case.check_plan().kinds.contains(&CheckKind::JavaEmit) {
        return CaseOutcome::SkippedUnknown;
    }

    if case.expected.implies_error() {
        return compile_error_outcome(case, ctx);
    }

    if equals_ops_expectation(case).is_some() {
        // Emit-only backend: if the program compiles to HIR, treat as pass
        // (value/ops are verified on interp).
        let Some(hir) = ctx.hir.as_deref() else {
            return CaseOutcome::FailParseError;
        };
        let version = version_from_byte(case.version);
        let emitted = leek_backend_java::emit_clean(hir, version, 1);
        if emitted.java.contains("class AI_") && !emitted.java.is_empty() {
            return CaseOutcome::Pass;
        }
        return CaseOutcome::FailWrongValue;
    }

    let Some(hir) = ctx.hir.as_deref() else {
        return CaseOutcome::FailParseError;
    };

    let version = version_from_byte(case.version);
    let emitted = leek_backend_java::emit_clean(hir, version, 1);
    if emitted.java.contains("class AI_") && !emitted.java.is_empty() {
        CaseOutcome::Pass
    } else {
        CaseOutcome::FailWrongValue
    }
}
