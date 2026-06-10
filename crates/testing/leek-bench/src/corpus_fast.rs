//! Fast batch rust-java corpus **correctness** checker.
//!
//! The per-case path ([`crate::runner_rust_java`]) spawns a fresh `javac` *and*
//! JVM for every case — the process startup dominates, so the full corpus takes
//! hours. This module amortizes both: it emits every case to `AI_<i>.java`,
//! compiles them all in one `javac` (with failure-exclusion, since one bad file
//! fails the whole compilation), then runs them all in **one** JVM via a
//! `BatchRunner` that executes each case in a daemon thread with a per-case
//! timeout (so a runaway case can't hang the batch). Only values are checked —
//! no timing — so it's purely a correctness sweep.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use leek_syntax::Version;

use crate::compile::compile_hir_file;
use crate::runner_rust_java::{build_classpath, detect_upstream_classpath};

/// One corpus case to check (an `equals(...)` case).
pub struct FastCase {
    pub id: String,
    pub code: String,
    pub version: u8,
    pub strict: bool,
    pub expected: String,
}

/// Why a case didn't agree with the reference.
#[derive(Debug)]
pub enum FastOutcome {
    /// Ran and produced a value, but it differs from the expected one.
    Disagree { got: String, expected: String },
    /// The Leekscript front-end / Java emit failed (no `.java` produced).
    EmitError(String),
    /// The generated Java didn't compile (`javac` reported an error).
    CompileError,
    /// The Java threw at runtime (exception class name).
    RuntimeError(String),
    /// The case exceeded the per-case wall-clock budget.
    Timeout,
    /// The case never reported a result (shouldn't normally happen).
    NoResult,
}

/// Aggregate result of a fast sweep.
pub struct FastReport {
    pub total: usize,
    pub agree: usize,
    /// `(case id, why)` for every non-agreeing case.
    pub failures: Vec<(String, FastOutcome)>,
    pub elapsed: Duration,
    /// `javac` invocations used (1 + one per failure-exclusion round).
    pub javac_rounds: usize,
}

impl FastReport {
    pub fn disagree(&self) -> usize {
        self.failures
            .iter()
            .filter(|(_, o)| matches!(o, FastOutcome::Disagree { .. }))
            .count()
    }
    pub fn errors(&self) -> usize {
        self.failures.len() - self.disagree()
    }
}

fn version_of(v: u8) -> Version {
    match v {
        1 => Version::V1,
        2 => Version::V2,
        3 => Version::V3,
        _ => Version::V4,
    }
}

/// Run a batch correctness sweep over `cases`. `version_override` (from
/// `--corpus-lang-version`) forces every case to that version when `Some`.
pub fn run_fast_java_corpus(
    cases: &[FastCase],
    version_override: Option<u8>,
) -> Result<FastReport> {
    let start = Instant::now();
    let upstream = detect_upstream_classpath()
        .context("upstream classpath not found (build the official generator's `build/classes`)")?;
    which("javac").context("`javac` not on PATH")?;
    which("java").context("`java` not on PATH")?;
    let cp = build_classpath(&upstream);

    let work = std::env::temp_dir().join(format!("leek-fastjava-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&work);
    std::fs::create_dir_all(&work).with_context(|| format!("mkdir {}", work.display()))?;

    let mut failures: Vec<(String, FastOutcome)> = Vec::new();
    // Cases that emitted a `.java` and so are candidates for compilation.
    // Index = position in `cases`.
    let mut emitted: Vec<usize> = Vec::new();

    // --- Emit phase: one `AI_<i>.java` per case (Rust-only, fast). ---
    for (i, case) in cases.iter().enumerate() {
        let version = version_override.unwrap_or(case.version);
        let leek_path = work.join(format!("c{i}.leek"));
        std::fs::write(&leek_path, &case.code)?;
        let hir = match compile_hir_file(&leek_path, version, case.strict) {
            Ok(c) => c.hir,
            Err(e) => {
                failures.push((
                    case.id.clone(),
                    FastOutcome::EmitError(short(&e.to_string())),
                ));
                continue;
            }
        };
        let opts = leek_backend_java::Options::clean(version_of(version), i as u64);
        let out = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            leek_backend_java::emit(hir.as_ref(), &opts)
        }));
        match out {
            Ok(out) => {
                std::fs::write(work.join(format!("{}.java", out.class_name)), &out.java)?;
                emitted.push(i);
            }
            Err(_) => failures.push((case.id.clone(), FastOutcome::EmitError("emit panic".into()))),
        }
    }

    // --- Compile phase. `javac` emits NO `.class` for any file if one fails,
    // and worse, a bad file can *crash* the compiler so only the errors before
    // the crash are reported. So we compile in CHUNKS (a crash only masks its
    // chunk), and within each chunk run failure-exclusion: parse the offending
    // `AI_<n>.java`, drop them, re-run, until the chunk compiles. ---
    let runner_path = work.join("BatchRunner.java");
    std::fs::write(&runner_path, BATCH_RUNNER)?;
    // BatchRunner uses `Class.forName`, so it has no compile-time deps on the AI
    // classes — compile it once on its own.
    let brun = Command::new("javac")
        .arg("-d")
        .arg(&work)
        .arg("-cp")
        .arg(&cp)
        .arg(&runner_path)
        .output()
        .context("running javac for BatchRunner")?;
    if !brun.status.success() {
        bail!(
            "BatchRunner.java did not compile:\n{}",
            short(&String::from_utf8_lossy(&brun.stderr))
        );
    }

    const CHUNK: usize = 600;
    let mut all: Vec<usize> = emitted.clone();
    all.sort_unstable();
    let mut compilable: HashSet<usize> = HashSet::new();
    let mut javac_rounds = 0;
    for chunk in all.chunks(CHUNK) {
        let mut remaining: HashSet<usize> = chunk.iter().copied().collect();
        let mut chunk_rounds = 0;
        while !remaining.is_empty() {
            let javac = compile_chunk(&work, &cp, &remaining)?;
            javac_rounds += 1;
            chunk_rounds += 1;
            if javac.0 {
                compilable.extend(&remaining);
                break;
            }
            let failed = parse_failed_ids(&javac.1);
            if failed.is_empty() || chunk_rounds > 60 {
                // Unattributable failure (a JVM crash with no useful stderr) or
                // a non-converging chunk: mark the survivors as compile-errors
                // rather than spin. Rare; bounded to one chunk.
                for &id in &remaining {
                    failures.push((cases[id].id.clone(), FastOutcome::CompileError));
                }
                break;
            }
            for id in failed {
                if remaining.remove(&id) {
                    failures.push((cases[id].id.clone(), FastOutcome::CompileError));
                }
            }
        }
    }

    // --- Run phase: one JVM, all compiled cases. ---
    let mut results: HashMap<usize, std::result::Result<String, FastOutcome>> = HashMap::new();
    if !compilable.is_empty() {
        let ids_file = work.join("ids.txt");
        let mut ids: Vec<usize> = compilable.iter().copied().collect();
        ids.sort_unstable();
        std::fs::write(
            &ids_file,
            ids.iter()
                .map(std::string::ToString::to_string)
                .collect::<Vec<_>>()
                .join("\n"),
        )?;
        let run_cp = format!("{}:{}", work.display(), cp);
        let out = Command::new("java")
            // Match the per-case runner: French locale (v1 comma decimals) and
            // discard system logs so they don't pollute stdout.
            .arg("-Duser.language=fr")
            .arg("-Duser.country=FR")
            .arg("-cp")
            .arg(&run_cp)
            .arg("BatchRunner")
            .arg(&ids_file)
            .output()
            .context("running BatchRunner")?;
        let stdout = String::from_utf8_lossy(&out.stdout);
        for line in stdout.lines() {
            let mut it = line.splitn(3, '\t');
            match (it.next(), it.next(), it.next()) {
                (Some("RES"), Some(id), Some(val)) => {
                    if let Ok(id) = id.parse::<usize>() {
                        results.insert(id, Ok(val.to_string()));
                    }
                }
                (Some("ERR"), Some(id), msg) => {
                    if let Ok(id) = id.parse::<usize>() {
                        let m = msg.unwrap_or("error");
                        let outcome = if m == "timeout" {
                            FastOutcome::Timeout
                        } else {
                            FastOutcome::RuntimeError(m.to_string())
                        };
                        results.insert(id, Err(outcome));
                    }
                }
                _ => {}
            }
        }
    }

    // --- Compare. ---
    let mut agree = 0;
    for &i in &emitted {
        if !compilable.contains(&i) {
            continue; // already recorded as CompileError
        }
        let case = &cases[i];
        match results.remove(&i) {
            Some(Ok(val)) => {
                if val.trim() == case.expected.trim() {
                    agree += 1;
                } else {
                    failures.push((
                        case.id.clone(),
                        FastOutcome::Disagree {
                            got: val,
                            expected: case.expected.clone(),
                        },
                    ));
                }
            }
            Some(Err(outcome)) => failures.push((case.id.clone(), outcome)),
            None => failures.push((case.id.clone(), FastOutcome::NoResult)),
        }
    }

    Ok(FastReport {
        total: cases.len(),
        agree,
        failures,
        elapsed: start.elapsed(),
        javac_rounds,
    })
}

/// Compile one chunk of `AI_<i>.java` files. Returns `(success, stderr)`.
/// `-Xmaxerrs` makes `javac` report every error it reaches (so one round
/// catches all non-crash failures in the chunk).
fn compile_chunk(work: &Path, cp: &str, ids: &HashSet<usize>) -> Result<(bool, String)> {
    let argfile = work.join("javac_args.txt");
    let mut args = String::new();
    for &i in ids {
        // One path per line; `javac @argfile` avoids ARG_MAX.
        args.push_str(&work.join(format!("AI_{i}.java")).display().to_string());
        args.push('\n');
    }
    std::fs::write(&argfile, &args)?;
    let out = Command::new("javac")
        .arg("-d")
        .arg(work)
        .arg("-cp")
        .arg(cp)
        .arg("-Xmaxerrs")
        .arg("1000000")
        .arg("-nowarn")
        .arg(format!("@{}", argfile.display()))
        .output()
        .context("running javac")?;
    Ok((
        out.status.success(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    ))
}

/// Extract the set of `AI_<n>` indices that `javac` reported errors for.
fn parse_failed_ids(stderr: &str) -> HashSet<usize> {
    let mut out = HashSet::new();
    for line in stderr.lines() {
        if !line.contains("error:") {
            continue;
        }
        // `…/AI_<n>.java:<line>: error: …`
        if let Some(start) = line.find("AI_") {
            let rest = &line[start + 3..];
            let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
            if rest[digits.len()..].starts_with(".java")
                && let Ok(n) = digits.parse::<usize>()
            {
                out.insert(n);
            }
        }
    }
    out
}

fn short(s: &str) -> String {
    let s = s.trim();
    let first = s.lines().next().unwrap_or(s);
    first.chars().take(160).collect()
}

fn which(prog: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|d| d.join(prog))
        .find(|p| p.is_file())
}

/// `BatchRunner` — instantiates and runs each `AI_<id>` listed in the ids file
/// (one per line), each in its own daemon thread with a per-case timeout so a
/// runaway case can't hang the batch. Prints `RES\t<id>\t<export>` or
/// `ERR\t<id>\t<msg>` per case. `System.exit(0)` reaps leaked (hung) threads.
const BATCH_RUNNER: &str = r#"import leekscript.runner.AI;
import leekscript.runner.Session;
import java.nio.file.*;
import java.util.concurrent.*;
public class BatchRunner {
    public static void main(String[] a) throws Exception {
        var ids = Files.readAllLines(Paths.get(a[0]));
        ExecutorService exec = Executors.newCachedThreadPool(r -> {
            Thread t = new Thread(r); t.setDaemon(true); return t;
        });
        StringBuilder out = new StringBuilder();
        for (String line : ids) {
            line = line.trim();
            if (line.isEmpty()) continue;
            final int id = Integer.parseInt(line);
            Future<String> f = exec.submit(() -> runCase(id));
            String res;
            try {
                res = f.get(8, TimeUnit.SECONDS);
            } catch (TimeoutException te) {
                f.cancel(true);
                res = "ERR\t" + id + "\ttimeout";
            } catch (ExecutionException ee) {
                Throwable c = ee.getCause();
                res = "ERR\t" + id + "\t" + (c == null ? "error" : c.getClass().getSimpleName());
            } catch (Throwable t) {
                res = "ERR\t" + id + "\t" + t.getClass().getSimpleName();
            }
            out.append(res).append('\n');
        }
        System.out.print(out);
        System.out.flush();
        System.exit(0);
    }
    static String runCase(int id) throws Exception {
        AI ai = (AI) Class.forName("AI_" + id).getDeclaredConstructor().newInstance();
        ai.init();
        ai.staticInit();
        ai.getLogs().setStream(x -> {});
        Object v = ai.runIA(new Session());
        // export is single-line (control chars are escaped), so tab-delimiting
        // is safe.
        return "RES\t" + id + "\t" + ai.export(v);
    }
}
"#;
