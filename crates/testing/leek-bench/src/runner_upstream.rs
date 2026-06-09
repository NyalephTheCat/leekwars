//! [`UpstreamJava`] — invoke upstream's `LeekScript.compileFile` once
//! inside a Java wrapper, then loop `runIA()` N times.
//!
//! Avoids the per-iteration JVM cold-start and per-iteration recompile
//! that the off-the-shelf `leekscript.TopLevel` driver incurs.
//!
//! `prepare()` writes our wrapper to a temp dir, javac's it against
//! the upstream classpath, and stashes the source path. `bench_runs()`
//! spawns one `java UpstreamRunner <path> <runs>` invocation and
//! parses one `INNER_NS=<n>` line per iteration on stderr. A
//! `COMPILE_NS=<n>` line is captured separately as the prepare step
//! "(jvm-internal compile)" so the breakdown isn't empty.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};

use crate::{Backend, BenchOptions, RunResult};

pub struct UpstreamJava {
    classpath: Option<PathBuf>,
    full_classpath: String,
    work_dir: Option<PathBuf>,
    source: Option<PathBuf>,
    steps: Vec<(String, Duration)>,
}

impl UpstreamJava {
    pub fn auto() -> Self {
        Self {
            classpath: detect_classpath(),
            full_classpath: String::new(),
            work_dir: None,
            source: None,
            steps: Vec::new(),
        }
    }
}

impl Backend for UpstreamJava {
    fn name(&self) -> &'static str {
        "upstream-java"
    }
    fn prepare(&mut self, source: &Path, _opts: &BenchOptions) -> Result<()> {
        let cp_root = self
            .classpath
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("upstream classpath not found"))?;
        which("javac").ok_or_else(|| anyhow::anyhow!("javac not in PATH"))?;
        which("java").ok_or_else(|| anyhow::anyhow!("java not in PATH"))?;

        self.full_classpath = build_classpath(cp_root);

        let dir = std::env::temp_dir().join(format!("leek-bench-upstream-{}", std::process::id()));
        std::fs::create_dir_all(&dir).with_context(|| format!("mkdir {}", dir.display()))?;
        let wrapper_path = dir.join("UpstreamRunner.java");
        std::fs::write(&wrapper_path, UPSTREAM_RUNNER_SRC)
            .with_context(|| format!("writing {}", wrapper_path.display()))?;

        let t = Instant::now();
        let javac = Command::new("javac")
            .arg("-d")
            .arg(&dir)
            .arg("-cp")
            .arg(&self.full_classpath)
            .arg(&wrapper_path)
            .output()
            .with_context(|| "running javac on UpstreamRunner")?;
        let javac_t = t.elapsed();
        if !javac.status.success() {
            anyhow::bail!(
                "javac UpstreamRunner failed: {}",
                String::from_utf8_lossy(&javac.stderr),
            );
        }

        let canon = std::fs::canonicalize(source)
            .with_context(|| format!("canonicalize {}", source.display()))?;
        self.work_dir = Some(dir);
        self.source = Some(canon);
        self.steps = vec![("javac-wrapper".into(), javac_t)];
        Ok(())
    }
    fn bench_runs(&mut self, runs: usize) -> Result<Vec<RunResult>> {
        let dir = self.work_dir.as_ref().expect("not prepared");
        let src = self.source.as_ref().expect("not prepared");
        let cp = format!("{}:{}", dir.display(), self.full_classpath);
        let parent = src.parent().unwrap_or(Path::new("."));
        let file_name = src.file_name().unwrap();
        // NativeFileSystem resolves names relative to the JVM's CWD;
        // chdir to the source's directory and pass the basename so
        // includes (and the source itself) resolve.
        let out = Command::new("java")
            // The upstream runtime formats v1 reals with a default-locale
            // `DecimalFormat`; the corpus's expected values were authored in a
            // French (comma-decimal) locale, so pin it here for both Java
            // backends — otherwise `0.5` renders as `0.5` not `0,5` and every
            // v1 real "disagrees" spuriously.
            .arg("-Duser.language=fr")
            .arg("-Duser.country=FR")
            .arg("-cp")
            .arg(&cp)
            .arg("UpstreamRunner")
            .arg(file_name)
            .arg(runs.to_string())
            .current_dir(parent)
            .output()
            .with_context(|| "running upstream java")?;
        if !out.status.success() {
            anyhow::bail!(
                "upstream java failed: {}",
                String::from_utf8_lossy(&out.stderr),
            );
        }
        let stdout = String::from_utf8_lossy(&out.stdout);
        let stderr = String::from_utf8_lossy(&out.stderr);
        let inners: Vec<u64> = stderr
            .lines()
            .filter_map(|l| l.strip_prefix("INNER_NS="))
            .filter_map(|s| s.trim().parse().ok())
            .collect();
        if inners.len() != runs {
            anyhow::bail!(
                "UpstreamRunner printed {} INNER_NS lines, expected {}",
                inners.len(),
                runs,
            );
        }
        if let Some(compile_ns) = stderr
            .lines()
            .find_map(|l| l.strip_prefix("COMPILE_NS="))
            .and_then(|s| s.trim().parse().ok())
        {
            self.steps.push((
                "(jvm-internal compile)".into(),
                Duration::from_nanos(compile_ns),
            ));
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

const UPSTREAM_RUNNER_SRC: &str = r#"import java.io.File;
import leekscript.compiler.LeekScript;
import leekscript.compiler.Options;
import leekscript.runner.AI;

public class UpstreamRunner {
    public static void main(String[] args) throws Exception {
        String path = args[0];
        int runs = args.length > 1 ? Integer.parseInt(args[1]) : 1;
        LeekScript.setFileSystem(LeekScript.getNativeFileSystem());
        Options options = new Options();
        long c0 = System.nanoTime();
        AI ai = LeekScript.compileFile(new File(path).getPath(), "AI", options);
        long c1 = System.nanoTime();
        System.err.println("COMPILE_NS=" + (c1 - c0));
        // Discard system logs (the default `BasicAILog` prints each to stdout)
        // so a soft warning during runIA doesn't pollute the captured result —
        // matching the upstream test framework, which reads logs separately.
        ai.getLogs().setStream(a -> {});
        Object first = null;
        for (int i = 0; i < runs; i++) {
            ai.resetCounter();
            long t0 = System.nanoTime();
            var v = ai.runIA();
            long t1 = System.nanoTime();
            if (first == null) first = v;
            System.err.println("INNER_NS=" + (t1 - t0));
        }
        // Match the upstream test framework's result stringification
        // (`TestCommon`: `ai.export(v, ...)`), which quotes strings — `string()`
        // does not, so a top-level string result would mismatch the expected.
        System.out.println(ai.export(first));
    }
}
"#;

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

fn detect_classpath() -> Option<PathBuf> {
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
