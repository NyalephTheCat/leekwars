package test;

import java.io.BufferedReader;
import java.io.FileReader;
import java.io.FileWriter;
import java.io.IOException;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.nio.file.Path;
import java.nio.file.StandardOpenOption;
import java.text.DecimalFormat;
import java.text.DecimalFormatSymbols;
import java.text.NumberFormat;
import java.util.ArrayList;
import java.util.Arrays;
import java.util.Collections;
import java.util.HashSet;
import java.util.List;
import java.util.Locale;
import java.util.concurrent.CopyOnWriteArrayList;
import java.util.concurrent.atomic.AtomicInteger;
import java.util.concurrent.atomic.AtomicLong;

import leekscript.compiler.LeekScript;
import leekscript.compiler.Options;
import leekscript.compiler.AnalyzeError.AnalyzeErrorLevel;
import leekscript.compiler.exceptions.LeekCompilerException;
import leekscript.runner.AI;
import leekscript.runner.LeekRunException;
import leekscript.common.Error;

import static org.junit.jupiter.api.Assertions.assertEquals;

public class TestCommon {

	private static String GREEN_BOLD = "\033[1;32m";
	private static String C_RED = "\033[1;31m";
	private static String END_COLOR = "\033[0m";
	private static String C_PINK = "\033[1;95m";
	private static String C_GREY = "\033[0;90m";

	private static AtomicInteger tests = new AtomicInteger();
	private static AtomicInteger success = new AtomicInteger();
	private static AtomicInteger disabled = new AtomicInteger();
	private static AtomicLong analyze_time = new AtomicLong();
	private static AtomicLong compile_time = new AtomicLong();
	// private static long load_time = 0;
	private static AtomicLong execution_time = new AtomicLong();
	private static ArrayList<Long> operationsReference = new ArrayList<>();
	private static List<Long> operations = Collections.synchronizedList(new ArrayList<>());

	private static List<String> failedTests = new CopyOnWriteArrayList<>();
	private static List<String> disabledTests = new CopyOnWriteArrayList<>();

	/// Snapshot probe. When the `LEEK_SNAPSHOT` env var points at a
	/// file path, every passing inline assertion (`.equals(...)` and
	/// friends) appends one row of `(code, version, value, jvm_ops)`
	/// to that file. The Rust parity harness consumes the snapshot
	/// to cross-check both *value* and *runtime ops* end-to-end —
	/// not just structural emit shape. Inactive when the env var is
	/// unset, so normal test runs aren't affected.
	private static final Path SNAPSHOT_PATH = resolveSnapshotPath();
	private static final Object SNAPSHOT_LOCK = new Object();
	private static boolean snapshotHeaderWritten = false;

	private static Path resolveSnapshotPath() {
		String env = System.getenv("LEEK_SNAPSHOT");
		return env == null ? null : Path.of(env);
	}

	/// Reference probe. A richer sibling of the snapshot probe: when the
	/// `LEEK_REFERENCE` env var points at a file path, every passing
	/// value-bearing assertion appends one row of
	/// `(version, strict, kind, value, jvm_ops, code, generated_java)`.
	/// This is the golden dataset the Rust `leek-test-corpus` build
	/// embeds (value + ops + the official Java emission per case). It is
	/// deliberately a *separate* file/format from `LEEK_SNAPSHOT` so the
	/// existing 6-column snapshot consumers stay untouched. Inactive
	/// when the env var is unset.
	private static final Path REFERENCE_PATH = resolveReferencePath();
	private static final Object REFERENCE_LOCK = new Object();
	private static boolean referenceHeaderWritten = false;

	private static Path resolveReferencePath() {
		String env = System.getenv("LEEK_REFERENCE");
		return env == null ? null : Path.of(env);
	}

	/// Append one row to the reference dataset if the probe is active.
	/// Same gating as the snapshot (value-bearing passes only) but also
	/// captures the generated Java source (`AI_<id>.java`) so the Rust
	/// side can byte-compare its own emitter against the official one.
	private static void recordReference(int version, boolean strict, String code, Result result, String checkerKind) {
		if (REFERENCE_PATH == null) return;
		if (!"equals".equals(checkerKind) && !"almost".equals(checkerKind)
			&& !"ops".equals(checkerKind)) {
			return;
		}
		String value = result.result == null ? "" : result.result;
		String java = "";
		try {
			if (result.ai != null && result.ai.getFile() != null
				&& result.ai.getFile().getCompiledCode() != null) {
				java = result.ai.getFile().getCompiledCode().getJavaCode();
				if (java == null) java = "";
			}
		} catch (Throwable ignored) {
			// Generated Java is best-effort; a missing getter must never
			// break the run. Leave the column empty on any failure.
		}
		String row = version + "\t"
			+ (strict ? "S" : "-") + "\t"
			+ checkerKind + "\t"
			+ escape(value) + "\t"
			+ result.operations + "\t"
			+ escape(code) + "\t"
			+ escape(java)
			+ "\n";
		synchronized (REFERENCE_LOCK) {
			try {
				if (!referenceHeaderWritten) {
					Files.writeString(REFERENCE_PATH,
						"# version\tstrict\tkind\tvalue\tjvm_ops\tcode\tjava\n",
						StandardCharsets.UTF_8,
						StandardOpenOption.CREATE,
						StandardOpenOption.TRUNCATE_EXISTING);
					referenceHeaderWritten = true;
				}
				Files.writeString(REFERENCE_PATH, row,
					StandardCharsets.UTF_8,
					StandardOpenOption.CREATE,
					StandardOpenOption.APPEND);
			} catch (IOException e) {
				System.err.println("reference write failed: " + e.getMessage());
			}
		}
	}

	/// Append one row to the snapshot if the probe is active. Skipped
	/// for `error` / `warning` / `noWarning` style checkers (we only
	/// want value-asserting passes so the Rust side has something
	/// concrete to compare). Synchronized because tests run in
	/// parallel under JUnit's default executor.
	private static void recordSnapshot(int version, boolean strict, String code, Result result, String checkerKind) {
		if (SNAPSHOT_PATH == null) return;
		if (!"equals".equals(checkerKind) && !"almost".equals(checkerKind)
			&& !"ops".equals(checkerKind)) {
			return;
		}
		String value = result.result == null ? "" : result.result;
		// Tab-escape the snippet and value so the TSV stays parseable
		// across embedded newlines / tabs.
		String row = version + "\t"
			+ (strict ? "S" : "-") + "\t"
			+ checkerKind + "\t"
			+ escape(value) + "\t"
			+ result.operations + "\t"
			+ escape(code)
			+ "\n";
		synchronized (SNAPSHOT_LOCK) {
			try {
				if (!snapshotHeaderWritten) {
					Files.writeString(SNAPSHOT_PATH,
						"# version\tstrict\tkind\tvalue\tjvm_ops\tcode\n",
						StandardCharsets.UTF_8,
						StandardOpenOption.CREATE,
						StandardOpenOption.TRUNCATE_EXISTING);
					snapshotHeaderWritten = true;
				}
				Files.writeString(SNAPSHOT_PATH, row,
					StandardCharsets.UTF_8,
					StandardOpenOption.CREATE,
					StandardOpenOption.APPEND);
			} catch (IOException e) {
				System.err.println("snapshot write failed: " + e.getMessage());
			}
		}
	}

	private static String escape(String s) {
		// Newlines / tabs / backslashes get C-style escapes so the
		// row stays on one line and on three columns.
		StringBuilder b = new StringBuilder(s.length());
		for (int i = 0; i < s.length(); i++) {
			char c = s.charAt(i);
			switch (c) {
				case '\\': b.append("\\\\"); break;
				case '\n': b.append("\\n"); break;
				case '\r': b.append("\\r"); break;
				case '\t': b.append("\\t"); break;
				default: b.append(c);
			}
		}
		return b.toString();
	}

	public static class Case {
		String code;
		boolean enabled = true;
		int version_min = 1;
		int version_max = LeekScript.LATEST_VERSION;
		long maxOperations = Long.MAX_VALUE;
		long maxRAM = AI.MAX_RAM;
		boolean debug = false;
		boolean strict = false;
		/// Captured `Result` from each version's most recent run. A
		/// chained terminal (`.equals("x").ops(7)`) reuses these
		/// instead of recompiling — that way the ops check runs
		/// against the *same* execution whose return value was just
		/// asserted, which is the only way two assertions about one
		/// test stay self-consistent.
		List<Result> lastResults = new ArrayList<>();

		public Case(String code, boolean enabled) {
			this.code = code;
			this.enabled = enabled;
		}

		public Case(String code, boolean enabled, int version_min, int version_max) {
			this.code = code;
			this.enabled = enabled;
			this.version_min = version_min;
			this.version_max = version_max;
		}

		public Case(String code, boolean enabled, int version_min, int version_max, boolean strict) {
			this.code = code;
			this.enabled = enabled;
			this.version_min = version_min;
			this.version_max = version_max;
			this.strict = strict;
		}

		public Case equals(String expected) {
			run(new Checker() {
				public boolean check(Result result) {
					return result.result.equals(expected);
				}
				public String getExpected() { return expected; }
				public String getResult(Result result) { return result.result; }
				public String kind() { return "equals"; }
			});
			return this;
		}

		/// Assert the result *and* the ops cost in a single call.
		/// Equivalent to `.equals(expected).ops(expectedOps)` and
		/// preferred when both are known up-front — keeps the
		/// expectations close to the source code.
		public Case equalsOps(String expected, long expectedOps) {
			return equals(expected).ops(expectedOps);
		}

		public Case error(Error type) {
			run(new Checker() {
				public boolean check(Result result) {
					if (result.error != null) {
						return result.error == type;
					} else if (result.ai != null) {
						var errors = result.ai.getFile().getErrors();
						return errors.size() > 0 && errors.get(0).level == AnalyzeErrorLevel.ERROR && errors.get(0).error == type;
					}
					return false;
				}
				public String getExpected() { return "error " + type.name(); }
				public String getResult(Result result) {
					if (result.error != null) {
						return "error " + result.error.name() + " " + Arrays.toString(result.parameters);
					} else if (result.ai != null) {
						var errors = result.ai.getFile().getErrors();
						if (errors.size() > 0) return "error " + errors.get(0).error.name();
					}
					if (result.error != Error.NONE) {
						return result.error.name();
					}
					return "no error";
				}
				public String kind() { return "error"; }
			});
			return this;
		}

		public Case warning(Error type) {
			run(new Checker() {
				public boolean check(Result result) {
					if (result.ai != null) {
						var errors = result.ai.getFile().getErrors();
						return errors.size() > 0 && errors.get(0).level == AnalyzeErrorLevel.WARNING && errors.get(0).error == type;
					}
					return false;
				}
				public String getExpected() { return "warning " + type.name(); }
				public String getResult(Result result) {
					if (result.ai != null) {
						var errors = result.ai.getFile().getErrors();
						if (errors.size() > 0) return "warning " + errors.get(0).error.name();
					}
					if (result.error != Error.NONE) {
						return result.error.name();
					}
					return "no warning";
				}
				public String kind() { return "warning"; }
			});
			return this;
		}

		public Case noWarning() {
			run(new Checker() {
				public boolean check(Result result) {
					if (result.ai != null) {
						var errors = result.ai.getFile().getErrors();
						return errors.isEmpty();
					}
					return result.error == Error.NONE;
				}
				public String getExpected() { return "no warning"; }
				public String getResult(Result result) {
					if (result.ai != null) {
						var errors = result.ai.getFile().getErrors();
						if (errors.size() > 0) return errors.get(0).level + " " + errors.get(0).error.name();
					}
					if (result.error != Error.NONE) {
						return result.error.name();
					}
					return "no warning";
				}
				public String kind() { return "noWarning"; }
			});
			return this;
		}

		public Case any_error() {
			run(new Checker() {
				public boolean check(Result result) {
					return result.error != Error.NONE;
				}
				public String getExpected() { return "no error"; }
				public String getResult(Result result) { return result.error.name(); }
				public String kind() { return "any_error"; }
			});
			return this;
		}

		public Case almost(double expected) {
			return almost(expected, 1e-10);
		}

		public Case almost(double expected, double delta) {
			run(new Checker() {
				public boolean check(Result result) {
					try {
						double r = Double.parseDouble(result.result);
						return Math.abs(r - expected) < delta;
					} catch (Exception e) {
						return false;
					}
				}
				public String getExpected() { return String.valueOf(expected); }
				public String getResult(Result result) { return result.result; }
				public String kind() { return "almost"; }
			});
			return this;
		}

		public Case ops(long ops) {
			Checker checker = new Checker() {
				public boolean check(Result result) {
					return result.operations == ops;
				}
				public String getExpected() { return String.valueOf(ops); }
				public String getResult(Result result) { return String.valueOf(result.operations); }
				public String kind() { return "ops"; }
			};
			// Chained form: `.equals("x").ops(7)` reuses the cached
			// results from the previous terminal so the ops check
			// runs against the *same* execution. Standalone form:
			// no cache, run fresh — preserves the historical
			// `.ops(N)`-as-only-terminal usage in TestInterval &c.
			if (!lastResults.isEmpty()) {
				checkAll(checker);
			} else {
				run(checker);
			}
			return this;
		}

		public String run(Checker checker) {
			if (!enabled) {
				disabled.incrementAndGet();
				var s = C_PINK + "[DISA] " + END_COLOR + "[v" + version_min + "-" + version_max + "] " + code;
				System.out.println(s);
				disabledTests.add(s);
				return "disabled";
			}
			// Each fresh run discards prior cached results — a
			// chain like `.equals(...).ops(N)` lives within one
			// terminal's lifetime; calling `.equals(...)` again
			// later compiles+runs anew.
			lastResults.clear();
			for (int v = version_min; v <= version_max - 1; ++v) {
				run_version(v, checker);
			}
			return run_version(version_max, checker);
		}

		/// Re-evaluate `checker` against every cached result without
		/// recompiling. Used by chained terminals so a follow-up
		/// assertion (typically `.ops(N)`) lands on the same
		/// execution that produced the value the previous terminal
		/// already vetted.
		public void checkAll(Checker checker) {
			int v = version_min;
			for (Result result : lastResults) {
				report(v, result, checker);
				v++;
			}
		}

		/// Last-run result of the highest version evaluated. Some
		/// callers (`TestOperators` data-driven matrix) need the
		/// rendered value string back for further bookkeeping; this
		/// replaces the historical `String` return of `.equals(...)`.
		public String getResultString() {
			return lastResults.isEmpty()
				? null
				: lastResults.get(lastResults.size() - 1).result;
		}

		public String run_version(int version, Checker checker) {
			int aiID = 0;
			Result result;
			long compile_time = 0;
			long ops = 0;
			AI ai = null;
			var options = new Options(version, strict, this.debug, true, null, true);
			long t = System.nanoTime();
			try {
				boolean is_file = code.contains(".leek");

				ai = is_file ? LeekScript.compileFile(code, "AI", options) : LeekScript.compileSnippet(code, "AI", options);
				ai.init();
				ai.staticInit();
				ai.resetCounter();
				aiID = ai.getId();

				compile_time = ai.getCompileTime() / 1000000;
				TestCommon.analyze_time.addAndGet(ai.getAnalyzeTime() / 1000000);
				TestCommon.compile_time.addAndGet(ai.getCompileTime() / 1000000);
				// TestCommon.load_time += ai.getLoadTime() / 1000000;

				ai.maxOperations = this.maxOperations;
				ai.maxRAM = this.maxRAM;

				t = System.nanoTime();
				var v = ai.runIA();
				long exec_time = (System.nanoTime() - t) / 1000;
				TestCommon.execution_time.addAndGet(exec_time / 1000);

				ops = ai.operations();

				var vs = ai.export(v, new HashSet<>());
				result = new Result(vs, ai, Error.NONE, new String[0], ops, exec_time);

			} catch (LeekCompilerException e) {
				// e.printStackTrace();
				// System.out.println("Error = " + e.getError());
				result = new Result(e.getError().toString() + " " + Arrays.toString(e.getParameters()), ai, e.getError(), e.getParameters(), 0, 0);
			} catch (LeekRunException e) {
				long exec_time = (System.nanoTime() - t) / 1000;
				result = new Result(e.getError().toString(), ai, e.getError(), new String[0], ai.getOperations(), exec_time);
			} catch (Throwable e) {
				if (ai != null) {
					var error = ai.throwableToError(e);
					result = new Result(error.type.toString(), ai, error.type, error.parameters, 0, 0);
				} else {
					e.printStackTrace(System.out);
					result = new Result("unknown error!", ai, Error.UNKNOWN_ERROR, new String[0], 0, 0);
				}
			}

			// Cache so a chained terminal (`.equals("x").ops(7)`)
			// can re-evaluate against the same execution without
			// re-compiling.
			lastResults.add(result);
			report(version, result, checker);
			operations.add(ops);
			return result.result;
		}

		/// Render a pass/fail line for one execution against one
		/// checker. Shared by the fresh-run path (`run_version`)
		/// and the cache-reuse path (`checkAll`). Counts each
		/// invocation as a separate test — chained assertions
		/// (`.equals("x").ops(7)`) therefore tally as 2.
		private void report(int version, Result result, Checker checker) {
			tests.incrementAndGet();
			int aiID = result.ai != null ? result.ai.getId() : 0;
			long compile_time = result.ai != null ? result.ai.getCompileTime() / 1000000 : 0;
			if (checker.check(result)) {
				int ops_per_ms = result.exec_time > 0
					? (int) Math.round(1000 * (double) result.operations / result.exec_time)
					: 0;
				System.out.println(GREEN_BOLD + " [OK]  " + END_COLOR + "[v" + version + "]" + (strict ? "[strict]" : "") + " " + code + " === " + checker.getResult(result) + "	" + C_GREY + compile_time + "ms + " + fn(result.exec_time) + "µs" + ", " + fn(result.operations) + " ops, " + ops_per_ms + " ops/ms" + END_COLOR);
				success.incrementAndGet();
				// Snapshot the passing case. Skip file-based tests
				// (`code` ends in `.leek`) — those are loaded from
				// the resources tree and aren't standalone snippets.
				if (SNAPSHOT_PATH != null && !code.contains(".leek")) {
					recordSnapshot(version, strict, code, result, checker.kind());
				}
				if (REFERENCE_PATH != null && !code.contains(".leek")) {
					recordReference(version, strict, code, result, checker.kind());
				}
			} else {
				var err = C_RED + "[FAIL] " + END_COLOR + "[v" + version + "]" + (strict ? "[strict]" : "") + " " + code + " =/= " + checker.getExpected() + " got " + checker.getResult(result) + "\n" +
				"/home/pierre/dev/leek-wars/generator/leekscript/ai/AI_" + aiID + ".java";
				System.out.println(err);
				failedTests.add(err);
			}
		}

		public Case max_ops(long ops) {
			this.maxOperations = ops;
			return this;
		}

		public Case max_ram(long ram) {
			this.maxRAM = ram;
			return this;
		}

		public Case debug() {
			this.debug = true;
			return this;
		}
	}

	public static class Result {
		String result;
		AI ai;
		Error error;
		Object[] parameters;
		long operations;
		long exec_time;

		public Result(String result, AI ai, Error error, Object[] parameters, long operations, long exec_time) {
			this.result = result;
			this.ai = ai;
			this.operations = operations;
			this.exec_time = exec_time;
			this.error = error;
			this.parameters = parameters;
		}
	}

	public static interface Checker {
		public boolean check(Result result);

		public String getResult(Result result);

		public String getExpected();

		/// Short tag identifying which terminal built this checker
		/// (`equals`, `ops`, `almost`, `error`, `warning`,
		/// `noWarning`, `any_error`). The snapshot probe records
		/// this so consumers can filter for value-bearing rows
		/// vs error-only rows.
		default String kind() { return "unknown"; }
	}

	public Case code(String code) {
		return new Case(code, true);
	}
	public Case code_strict(String code) {
		return new Case(code, true, 1, LeekScript.LATEST_VERSION, true);
	}
	public Case file(String code) {
		return new Case(code, true);
	}
	public Case file_v1(String code) {
		return new Case(code, true, 1, 1);
	}
	public Case file_v2_(String code) {
		return new Case(code, true, 2, LeekScript.LATEST_VERSION);
	}
	public Case file_v3(String code) {
		return new Case(code, true, 3, 3);
	}
	public Case file_v4_(String code) {
		return new Case(code, true, 4, LeekScript.LATEST_VERSION);
	}
	public Case DISABLED_file(String code) {
		return new Case(code, false);
	}
	public Case DISABLED_file_v2_(String code) {
		return new Case(code, false, 2, LeekScript.LATEST_VERSION);
	}
	public Case code_v1(String code) {
		return new Case(code, true, 1, 1);
	}
	public Case code_strict_v1(String code) {
		return new Case(code, true, 1, 1, true);
	}
	public Case code_v1_2(String code) {
		return new Case(code, true, 1, 2);
	}
	public Case code_v1_3(String code) {
		return new Case(code, true, 1, 3);
	}
	public Case code_v2(String code) {
		return new Case(code, true, 2, 2);
	}
	public Case code_v2_(String code) {
		return new Case(code, true, 2, LeekScript.LATEST_VERSION);
	}
	public Case code_strict_v2_(String code) {
		return new Case(code, true, 2, LeekScript.LATEST_VERSION, true);
	}
	public Case code_v2_3(String code) {
		return new Case(code, true, 2, 3);
	}
	public Case code_v2_4(String code) {
		return new Case(code, true, 2, 4);
	}
	public Case code_v3(String code) {
		return new Case(code, true, 3, 3);
	}
	public Case code_v3_(String code) {
		return new Case(code, true, 3, LeekScript.LATEST_VERSION);
	}
	public Case code_v1_4(String code) {
		return new Case(code, true, 1, 4);
	}
	public Case code_v4(String code) {
		return new Case(code, true, 4, 4);
	}
	public Case code_v4_(String code) {
		return new Case(code, true, 4, LeekScript.LATEST_VERSION);
	}
	public Case code_strict_v4_(String code) {
		return new Case(code, true, 4, LeekScript.LATEST_VERSION, true);
	}
	public Case DISABLED_code_v4_(String code) {
		return new Case(code, false, 4, LeekScript.LATEST_VERSION);
	}
	public Case DISABLED_code(String code) {
		return new Case(code, false);
	}
	public Case DISABLED_code_v1(String code) {
		return new Case(code, false, 1, 1);
	}
	public Case DISABLED_code_v2_(String code) {
		return new Case(code, false, 2, LeekScript.LATEST_VERSION);
	}

	public void section(String title) {
		System.out.println("========== " + title + " ==========");
	}
	public void header(String title) {
		System.out.println("================================================");
		System.out.println("========== " + title + " ==========");
		System.out.println("================================================");
	}

	public static void summary() {
		int t = tests.get(), s = success.get(), d = disabled.get();
		long at = analyze_time.get(), ct = compile_time.get(), et = execution_time.get();
		System.out.println("================================================");
		System.out.println(s + " / " + t + " tests passed, " + (t - s) + " errors, " + d + " disabled");
		System.out.println("Total time: " + fn(at + ct + et) + " ms"
			+ " = Analyze: " + fn(at) + " ms"
			+ " + Compile: " + fn(ct) + " ms"
			+ " + Execution: " + fn(et) + " ms");
		System.out.println("================================================");

		for (String test : disabledTests) {
			System.out.println(test);
		}
		for (String test : failedTests) {
			System.out.println(test);
		}
		assertEquals(t, s, "Some tests failed");
	}

	public static String fn(long n) {
		DecimalFormat formatter = (DecimalFormat) NumberFormat.getInstance(Locale.US);
		DecimalFormatSymbols symbols = formatter.getDecimalFormatSymbols();

		symbols.setGroupingSeparator(' ');
		formatter.setDecimalFormatSymbols(symbols);
		return formatter.format(n);
	}
	public static void ouputOperationsFile() {
		try {
			FileWriter myWriter = new FileWriter("opérations.txt");
			for (Long ops : operations) {
				myWriter.write(String.valueOf(ops) + "\n");
			}
			myWriter.close();
			System.out.println("opérations.txt written");
		} catch (IOException e) {
			System.out.println("An error occurred.");
			e.printStackTrace();
		}
	}
	public static void loadReferenceOperations() {
		BufferedReader reader;
		try {
			reader = new BufferedReader(new FileReader("opérations_v1.txt"));
			String line = reader.readLine();
			while (line != null) {
				operationsReference.add(Long.parseLong(line));
				line = reader.readLine();
			}
			System.out.println(operationsReference.size() + " test operations references loaded.");
			reader.close();
		} catch (IOException e) {
			e.printStackTrace();
		}
	}
}
