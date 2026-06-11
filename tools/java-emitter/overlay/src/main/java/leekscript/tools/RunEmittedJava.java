package leekscript.tools;

import java.io.BufferedReader;
import java.io.InputStreamReader;
import java.io.PrintWriter;
import java.io.StringWriter;
import java.nio.charset.StandardCharsets;
import java.util.Arrays;
import java.util.Base64;
import java.util.Collections;
import java.util.HashSet;

import javax.tools.JavaCompiler;
import javax.tools.ToolProvider;

import leekscript.compiler.SimpleFileManager;
import leekscript.compiler.SimpleSourceFile;
import leekscript.runner.AI;

/**
 * Stdin-driven batch runner that compiles and executes Rust-emitted
 * Java sources on the JVM, reporting `(value, ops)` per snippet.
 *
 * <p>Used by the Rust corpus parity test: for each snapshot row, the
 * Rust backend emits Java, the test pipes that source here, and we
 * report what the JVM actually computes. Mismatches against the
 * snapshot's JVM-captured `(value, ops)` reveal cases where the
 * Rust emit drifts from the reference's emit even if both compile.
 *
 * <p>Protocol:
 * <pre>
 *   stdin:  &lt;id&gt; \t &lt;base64-of-utf8-java-source&gt; \n   (per case)
 *   stdout: &lt;id&gt; \t &lt;value-escaped&gt; \t &lt;ops&gt; \t &lt;error-escaped&gt; \n
 * </pre>
 * Empty error string means success. The harness exits on stdin EOF.
 * One JVM startup amortizes across thousands of snippets — each
 * `javac` invocation is in-memory via {@link SimpleFileManager}.
 */
public final class RunEmittedJava {

    private RunEmittedJava() {}

    public static void main(String[] args) throws Exception {
        // Match the upstream test harness's locale. The default JVM
        // locale would otherwise format real numbers with `.` even
        // for v1 snippets where the reference emits French-locale
        // `,`. See `test.SummaryExtension.beforeAll`.
        java.util.Locale.setDefault(java.util.Locale.FRENCH);
        JavaCompiler javac = ToolProvider.getSystemJavaCompiler();
        if (javac == null) {
            System.err.println(
                "no system Java compiler — RunEmittedJava needs a JDK, not just a JRE");
            System.exit(2);
        }
        BufferedReader in = new BufferedReader(
            new InputStreamReader(System.in, StandardCharsets.UTF_8));
        PrintWriter out = new PrintWriter(
            new java.io.OutputStreamWriter(System.out, StandardCharsets.UTF_8), true);

        String line;
        while ((line = in.readLine()) != null) {
            int tab = line.indexOf('\t');
            if (tab < 0) continue;
            String id = line.substring(0, tab);
            String b64 = line.substring(tab + 1);
            String source;
            try {
                source = new String(Base64.getDecoder().decode(b64), StandardCharsets.UTF_8);
            } catch (IllegalArgumentException e) {
                emitError(out, id, "bad base64: " + e.getMessage());
                continue;
            }
            try {
                String className = extractClassName(source);
                Object[] r = compileAndRun(javac, className, source);
                String value = (String) r[0];
                long ops = (long) r[1];
                out.println(id + "\t" + escape(value) + "\t" + ops + "\t");
            } catch (Throwable t) {
                String msg = t.getClass().getSimpleName() + ": "
                    + (t.getMessage() == null ? "" : t.getMessage());
                emitError(out, id, msg);
            }
        }
    }

    /**
     * Compile `source` in memory, load the resulting class, run
     * `runIA(null)`, and return `[exportedString, opsCount]`.
     */
    private static Object[] compileAndRun(JavaCompiler javac, String className, String source)
            throws Exception {
        SimpleFileManager fileManager = new SimpleFileManager(
            javac.getStandardFileManager(null, null, null));
        var unit = new SimpleSourceFile(className + ".java", source);
        var argsList = Arrays.asList("-nowarn", "-proc:none");
        var diag = new StringWriter();
        var task = javac.getTask(diag, fileManager, null, argsList, null,
            Collections.singletonList(unit));
        boolean ok = task.call();
        if (!ok) {
            throw new RuntimeException("javac: " + diag.toString().trim());
        }
        ClassLoader loader = new ClassLoader() {
            @Override
            protected Class<?> findClass(String name) throws ClassNotFoundException {
                var bytes = fileManager.get(name).getCompiledBinaries();
                return defineClass(name, bytes, 0, bytes.length);
            }
        };
        var clazz = loader.loadClass(className);
        AI ai = (AI) clazz.getDeclaredConstructor().newInstance();
        // 20M matches the default budget the upstream test harness
        // uses (and accommodates the ~13.6M-op DNA stress cases at
        // L3524+ in the snapshot). 10M was the original ceiling and
        // tripped LeekRunException on those cases.
        ai.maxOperations = 20_000_000L;
        ai.maxRAM = AI.MAX_RAM;
        // Match `TestCommon.run_version`: init → staticInit → reset
        // → runIA. Reversing this would include the static init
        // ticks in the per-snippet count.
        ai.init();
        ai.staticInit();
        ai.resetCounter();
        Object v = ai.runIA(null);
        // `ai.export(...)` charges its own ops for some value types
        // (e.g. +3 per Long for the String.valueOf conversion). Capture
        // the counter BEFORE export so the harness reports the ops the
        // snippet itself consumed — which is what
        // `TestCommon.run_version` also records via `ai.operations()`
        // right after `ai.runIA()`, before formatting.
        long ops = ai.operations();
        String exported = ai.export(v, new HashSet<>());
        return new Object[] { exported, ops };
    }

    /** Best-effort class-name extractor from `public class FOO extends ...`. */
    private static String extractClassName(String source) {
        int i = source.indexOf("public class ");
        if (i < 0) throw new IllegalArgumentException("no `public class` found in source");
        i += "public class ".length();
        int j = i;
        while (j < source.length()) {
            char c = source.charAt(j);
            if (Character.isJavaIdentifierPart(c)) j++;
            else break;
        }
        return source.substring(i, j);
    }

    private static void emitError(PrintWriter out, String id, String msg) {
        out.println(id + "\t" + "\t" + 0 + "\t" + escape(msg));
    }

    /** Escape newlines / tabs / backslashes so each row stays one line. */
    private static String escape(String s) {
        if (s == null) return "";
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
}
