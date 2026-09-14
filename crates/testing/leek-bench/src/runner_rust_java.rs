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
            anyhow::bail!("javac failed: {}", String::from_utf8_lossy(&javac.stderr));
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
            anyhow::bail!("java failed: {}", String::from_utf8_lossy(&out.stderr));
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

pub(crate) fn detect_upstream_classpath() -> Option<PathBuf> {
    let cwd = std::env::current_dir().ok()?;
    for ancestor in cwd.ancestors() {
        let p = ancestor.join("official-generator/leek-wars-generator/leekscript/build/classes");
        if p.is_dir() {
            return Some(p);
        }
    }
    None
}

pub(crate) fn build_classpath(upstream: &Path) -> String {
    let mut parts: Vec<String> = Vec::new();
    // Prefer the fat reference jar — it is the exact build the corpus
    // `equals(...)` expectations were generated from. It MUST come first: the
    // `build/classes` root holds a stale partial tree (`build/classes/leekscript`,
    // an older build) that shadows the fresh `build/classes/java/main` classes
    // on the classpath, so without the jar first, e.g. `MapLeekValue`'s
    // Object[] constructor resolves to an old array-style version and every map
    // literal renders wrong. Jar-first makes the fresh classes win everywhere.
    // `upstream` is `.../leekscript/build/classes`; the jar sits at
    // `.../leekscript/leekscript.jar` (two levels up).
    if let Some(jar) = upstream
        .ancestors()
        .map(|a| a.join("leekscript.jar"))
        .find(|p| p.is_file())
    {
        parts.push(jar.display().to_string());
    }
    // Fresh compiled tree (matches the jar), then the legacy root as a fallback
    // for anything only present there.
    let fresh = upstream.join("java/main");
    if fresh.is_dir() {
        parts.push(fresh.display().to_string());
    }
    parts.push(upstream.display().to_string());
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
            && path
                .extension()
                .is_some_and(|e| e.eq_ignore_ascii_case("jar"))
            && !name.ends_with("-sources.jar")
            && !name.ends_with("-javadoc.jar")
        {
            out.push(path.display().to_string());
        }
    }
}

#[cfg(test)]
mod tests {
    //! The out-of-process Java runner's three pure helpers.
    //!
    //! None of them needs a JVM or the upstream submodule, and each fails
    //! *silently or misleadingly* in production: a `parse_inner_ns_all`
    //! regression turns every Java benchmark into "Runner printed 0 INNER_NS
    //! lines", and a `build_classpath` ordering regression produces wrong
    //! values with no error at all (see the comment on that function).

    use std::path::{Path, PathBuf};

    use super::{build_classpath, collect_jars, parse_inner_ns_all};

    /// One scratch directory per test. The test name keeps parallel threads
    /// apart without depending on the clock — `SystemTime::now()` is coarse
    /// enough on macOS that two calls in one tick collide.
    fn scratch(test: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("leek-bench-{test}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create scratch dir");
        dir
    }

    fn touch(path: &Path) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("create parent dir");
        }
        std::fs::write(path, b"").expect("create file");
    }

    #[test]
    fn inner_ns_lines_are_parsed_in_order() {
        // `bench_runs` zips these positionally onto the run results, so the
        // order is part of the contract, not an accident of iteration.
        let stderr = "INNER_NS=300\nINNER_NS=100\nINNER_NS=200\n";
        assert_eq!(parse_inner_ns_all(stderr), vec![300, 100, 200]);
    }

    #[test]
    fn unrelated_stderr_lines_are_ignored() {
        // A JVM that prints warnings (`-Xshare`, agent notices, the
        // `sun.misc.Unsafe` deprecation on 21+) must not shift the count —
        // the caller hard-fails when it doesn't match `runs`.
        let stderr = concat!(
            "OpenJDK 64-Bit Server VM warning: Options -Xverify:none…\n",
            "INNER_NS=42\n",
            "WARNING: A terminally deprecated method in sun.misc.Unsafe\n",
            "INNER_NS=43\n",
        );
        assert_eq!(parse_inner_ns_all(stderr), vec![42, 43]);
    }

    #[test]
    fn a_trailing_carriage_return_still_parses() {
        // The value is `trim`med, so CRLF-terminated output still yields a
        // number rather than being dropped as malformed.
        assert_eq!(parse_inner_ns_all("INNER_NS=7\r\n"), vec![7]);
        assert_eq!(parse_inner_ns_all("INNER_NS= 7 \n"), vec![7]);
    }

    #[test]
    fn malformed_and_empty_input_yield_nothing_rather_than_panicking() {
        assert!(parse_inner_ns_all("").is_empty());
        assert!(parse_inner_ns_all("INNER_NS=\n").is_empty());
        assert!(parse_inner_ns_all("INNER_NS=not-a-number\n").is_empty());
        assert!(parse_inner_ns_all("INNER_NS=-1\n").is_empty());
        // The prefix must match at the start of the line, not anywhere in it.
        assert!(parse_inner_ns_all("total INNER_NS=5\n").is_empty());
    }

    /// The jar must come first. `build/classes` holds a stale partial tree
    /// that shadows the fresh `build/classes/java/main` classes, so a
    /// reordering here silently renders every map literal wrong instead of
    /// failing — the exact failure the function's comment describes.
    #[test]
    fn the_reference_jar_wins_over_both_class_trees() {
        let root = scratch("classpath-jar-first");
        let upstream = root.join("leekscript/build/classes");
        let jar = root.join("leekscript/leekscript.jar");
        let fresh = upstream.join("java/main");
        touch(&jar);
        std::fs::create_dir_all(&fresh).expect("create fresh class tree");

        let cp = build_classpath(&upstream);
        let parts: Vec<&str> = cp.split(':').collect();
        assert_eq!(
            &parts[..3],
            &[
                jar.display().to_string().as_str(),
                fresh.display().to_string().as_str(),
                upstream.display().to_string().as_str(),
            ],
            "jar, then the fresh tree, then the legacy root: {cp}"
        );
        // Anything after that comes from the developer's ~/.gradle cache,
        // which this test deliberately does not constrain.
    }

    #[test]
    fn a_missing_jar_or_fresh_tree_is_skipped_without_reordering_the_rest() {
        let root = scratch("classpath-fallbacks");

        // No jar, but a fresh tree: fresh tree first, legacy root second.
        let a = root.join("a/build/classes");
        std::fs::create_dir_all(a.join("java/main")).expect("create fresh tree");
        let parts: Vec<String> = build_classpath(&a).split(':').map(str::to_string).collect();
        assert_eq!(parts[0], a.join("java/main").display().to_string());
        assert_eq!(parts[1], a.display().to_string());

        // Neither: the legacy root is the only entry we contribute, and it is
        // pushed unconditionally even though it may not exist yet.
        let b = root.join("b/build/classes");
        let parts: Vec<String> = build_classpath(&b).split(':').map(str::to_string).collect();
        assert_eq!(parts[0], b.display().to_string());
    }

    #[test]
    fn collect_jars_takes_real_jars_at_any_depth_and_skips_the_decorations() {
        let root = scratch("collect-jars");
        touch(&root.join("a.jar"));
        touch(&root.join("b-sources.jar"));
        touch(&root.join("c-javadoc.jar"));
        // The extension check is case-insensitive; the `-sources` /
        // `-javadoc` suffix check is not, and that asymmetry is worth
        // pinning so nobody "tidies" one to match the other.
        touch(&root.join("nested/deep/d.JAR"));
        touch(&root.join("nested/notes.txt"));
        touch(&root.join("nested/e.jar.bak"));

        let mut out = Vec::new();
        collect_jars(&root, &mut out);
        out.sort();

        let mut expected = vec![
            root.join("a.jar").display().to_string(),
            root.join("nested/deep/d.JAR").display().to_string(),
        ];
        expected.sort();
        assert_eq!(out, expected);
    }

    #[test]
    fn collect_jars_on_a_missing_directory_is_a_no_op() {
        // The caller guards with `is_dir()`, but a cache directory can vanish
        // between the check and the read; that must not panic mid-benchmark.
        let mut out = vec!["kept".to_string()];
        collect_jars(Path::new("/definitely/not/a/real/gradle/cache"), &mut out);
        assert_eq!(out, vec!["kept".to_string()]);
    }
}
