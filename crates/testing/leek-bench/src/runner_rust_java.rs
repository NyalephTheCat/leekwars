//! [`RustJavaEmit`] — run our Java backend's output end-to-end.
//!
//! - [`prepare`](Backend::prepare) does the entire compile pipeline
//!   (lex → parse → resolve → typecheck → HIR-lower → emit Java →
//!   javac). Every step is timed and surfaced via
//!   [`BenchSummary::prepare_steps`].
//! - [`run_once`](Backend::run_once) launches `java Runner`. The
//!   generated `Runner.java` brackets `ai.runIA(...)` with
//!   `System.nanoTime()` and prints `INNER_NS=<n>` on stderr — that
//!   is what we report as the elapsed time. The JVM cold-start and
//!   classload cost stay out of the headline number.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use crate::{Backend, BenchOptions, RunResult, compile_hir_file};
use anyhow::{Context, Result};

pub struct RustJavaEmit {
    upstream_classpath: Option<PathBuf>,
    work_dir: Option<PathBuf>,
    class_name: String,
    full_classpath: String,
    ai_id: u64,
    steps: Vec<(String, Duration)>,
}

impl RustJavaEmit {
    pub fn auto() -> Self {
        Self {
            upstream_classpath: detect_upstream_classpath(),
            work_dir: None,
            class_name: String::new(),
            full_classpath: String::new(),
            ai_id: 0,
            steps: Vec::new(),
        }
    }
}

impl Backend for RustJavaEmit {
    fn name(&self) -> &'static str {
        "rust-java"
    }
    fn prepare(&mut self, source: &Path, opts: &BenchOptions) -> Result<()> {
        let upstream = self
            .upstream_classpath
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("upstream classpath not found"))?;
        which("javac").ok_or_else(|| anyhow::anyhow!("javac not in PATH"))?;
        which("java").ok_or_else(|| anyhow::anyhow!("java not in PATH"))?;

        let compiled = compile_hir_file(source, opts.version, opts.strict)
            .with_context(|| format!("compiling {}", source.display()))?;
        let hir = compiled.hir;
        let mut steps = compiled.steps;

        // Emit Java source via our backend; measure it inline since
        // there's no Step wrapper for the emitter today.
        let version = match opts.version {
            1 => leek_syntax::Version::V1,
            2 => leek_syntax::Version::V2,
            3 => leek_syntax::Version::V3,
            _ => leek_syntax::Version::V4,
        };
        let java_opts = leek_backend_java::Options::clean(version, self.ai_id);
        let t = Instant::now();
        let out = leek_backend_java::emit(hir.as_ref(), &java_opts);
        steps.push(("emit-java".into(), t.elapsed()));

        let dir = std::env::temp_dir().join(format!("leek-bench-{}", std::process::id()));
        std::fs::create_dir_all(&dir).with_context(|| format!("mkdir {}", dir.display()))?;
        let java_path = dir.join(format!("{}.java", out.class_name));
        let runner_src = runner_source(&out.class_name);
        let runner_path = dir.join("Runner.java");
        let t = Instant::now();
        std::fs::write(&java_path, &out.java)
            .with_context(|| format!("writing {}", java_path.display()))?;
        std::fs::write(&runner_path, runner_src)
            .with_context(|| format!("writing {}", runner_path.display()))?;
        steps.push(("write-files".into(), t.elapsed()));

        let cp = build_classpath(upstream);

        let t = Instant::now();
        let javac = Command::new("javac")
            .arg("-d")
            .arg(&dir)
            .arg("-cp")
            .arg(&cp)
            .arg(&java_path)
            .arg(&runner_path)
            .output()
            .with_context(|| "running javac")?;
        steps.push(("javac".into(), t.elapsed()));
        if !javac.status.success() {
            anyhow::bail!("javac failed: {}", String::from_utf8_lossy(&javac.stderr),);
        }

        self.work_dir = Some(dir);
        self.class_name = out.class_name;
        self.full_classpath = cp;
        self.steps = steps;
        Ok(())
    }
    fn bench_runs(&mut self, runs: usize) -> Result<Vec<RunResult>> {
        let dir = self.work_dir.as_ref().expect("not prepared");
        let cp = format!("{}:{}", dir.display(), self.full_classpath);
        // Single JVM invocation loops `runs` times inside Runner so
        // the JIT can warm up and we don't pay class-load cost N
        // times. Runner prints one `INNER_NS=…` per iteration.
        let out = Command::new("java")
            // v1 reals are formatted by the upstream runtime with a
            // default-locale `DecimalFormat`; the corpus expects the French
            // (comma-decimal) form the tests were authored in. Pin it so `0.5`
            // renders as `0,5` (matching upstream + the expected values).
            .arg("-Duser.language=fr")
            .arg("-Duser.country=FR")
            .arg("-cp")
            .arg(&cp)
            .arg("Runner")
            .arg(runs.to_string())
            .output()
            .with_context(|| "running java")?;
        if !out.status.success() {
            anyhow::bail!("java failed: {}", String::from_utf8_lossy(&out.stderr),);
        }
        let stdout = String::from_utf8_lossy(&out.stdout);
        let stderr = String::from_utf8_lossy(&out.stderr);
        let inners = parse_inner_ns_all(&stderr);
        if inners.len() != runs {
            anyhow::bail!(
                "Runner printed {} INNER_NS lines, expected {}",
                inners.len(),
                runs,
            );
        }
        let value = stdout.trim().to_string();
        Ok(inners
            .into_iter()
            .map(|ns| RunResult {
                elapsed: Duration::from_nanos(ns),
                stdout: value.clone(),
            })
            .collect())
    }
    fn prepare_steps(&self) -> Vec<(String, Duration)> {
        self.steps.clone()
    }
}

fn runner_source(class_name: &str) -> String {
    format!(
        r#"import leekscript.runner.AI;
import leekscript.runner.Session;
public class Runner {{
    public static void main(String[] args) throws Exception {{
        int runs = args.length > 0 ? Integer.parseInt(args[0]) : 1;
        AI ai = new {class_name}();
        ai.init();
        ai.staticInit();
        // Discard system logs (the default `BasicAILog` prints each to
        // stdout). The upstream test framework keeps logs separate from the
        // result, so a soft warning — e.g. indexing `null` logs
        // `VALUE_IS_NOT_AN_ARRAY` yet still returns null — must not pollute the
        // captured value on stdout.
        ai.getLogs().setStream(a -> {{}});
        Object first = null;
        for (int i = 0; i < runs; i++) {{
            ai.resetCounter();
            long t0 = System.nanoTime();
            var v = ai.runIA(new Session());
            long t1 = System.nanoTime();
            if (first == null) first = v;
            System.err.println("INNER_NS=" + (t1 - t0));
        }}
        // Match the upstream test framework's result stringification
        // (`TestCommon`: `ai.export(v, ...)`), which quotes strings — `string()`
        // does not, so a top-level string result would mismatch the expected.
        System.out.println(ai.export(first));
    }}
}}
"#,
    )
}

fn parse_inner_ns_all(stderr: &str) -> Vec<u64> {
    stderr
        .lines()
        .filter_map(|l| l.strip_prefix("INNER_NS="))
        .filter_map(|s| s.trim().parse().ok())
        .collect()
}

fn which(prog: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        let candidate = dir.join(prog);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

fn detect_upstream_classpath() -> Option<PathBuf> {
    let cwd = std::env::current_dir().ok()?;
    for ancestor in cwd.ancestors() {
        let p = ancestor.join("official-generator/leek-wars-generator/leekscript/build/classes");
        if p.is_dir() {
            return Some(p);
        }
    }
    None
}

fn build_classpath(upstream: &Path) -> String {
    let mut parts: Vec<String> = vec![upstream.display().to_string()];
    if let Some(home) = std::env::var_os("HOME") {
        let cache = PathBuf::from(home).join(".gradle/caches/modules-2/files-2.1");
        if cache.is_dir() {
            collect_jars(&cache, &mut parts);
        }
    }
    parts.join(":")
}

fn collect_jars(root: &Path, out: &mut Vec<String>) {
    let Ok(read) = std::fs::read_dir(root) else {
        return;
    };
    for entry in read.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_jars(&path, out);
        } else if let Some(name) = path.file_name().and_then(|n| n.to_str())
            && path.extension().is_some_and(|e| e.eq_ignore_ascii_case("jar"))
                && !name.ends_with("-sources.jar")
                && !name.ends_with("-javadoc.jar")
            {
                out.push(path.display().to_string());
            }
    }
}
