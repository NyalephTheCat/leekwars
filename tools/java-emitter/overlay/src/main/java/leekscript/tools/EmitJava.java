package leekscript.tools;

import java.nio.file.Files;
import java.nio.file.Path;

import leekscript.compiler.AICode;
import leekscript.compiler.AIFile;
import leekscript.compiler.IACompiler;
import leekscript.compiler.Options;

/**
 * Driver that runs the upstream LeekScript transpiler in
 * source-emission mode and prints the generated `.java` to stdout
 * (and, optionally, the `.lines` sidecar to a separate file).
 *
 * <p>Unlike {@link leekscript.compiler.JavaCompiler#compile}, this
 * tool stops after the Java source has been built — it does not
 * invoke {@code javac} or load the resulting class. That makes it
 * usable as a golden-output capture for the Rust-side parity
 * harness ({@code crates/backends/leek-backend-java}).
 *
 * <p>Usage:
 * <pre>
 *   java -cp leekscript.jar leekscript.tools.EmitJava \
 *     --version 4 --ai-id 6750 --out-dir out path/to.leek
 * </pre>
 * If {@code --out-dir} is omitted, the {@code .java} is printed to
 * stdout and the {@code .lines} sidecar is dropped.
 */
public final class EmitJava {

    private EmitJava() {}

    public static void main(String[] args) throws Exception {
        int version = 4;
        int aiId = 0;
        Path out = null;
        Path input = null;
        boolean enableOps = true;
        String pathOverride = null;

        for (int i = 0; i < args.length; i++) {
            switch (args[i]) {
                case "--version":
                    version = Integer.parseInt(args[++i]);
                    break;
                case "--ai-id":
                    aiId = Integer.parseInt(args[++i]);
                    break;
                case "--out-dir":
                    out = Path.of(args[++i]);
                    break;
                case "--no-ops":
                    enableOps = false;
                    break;
                case "--path-override":
                    // Replaces the absolute input path baked into
                    // `AIFile`. Useful for reproducible golden
                    // captures across machines.
                    pathOverride = args[++i];
                    break;
                case "-h":
                case "--help":
                    printUsage();
                    return;
                default:
                    if (args[i].startsWith("--")) {
                        System.err.println("unknown flag: " + args[i]);
                        printUsage();
                        System.exit(2);
                    }
                    input = Path.of(args[i]);
            }
        }
        if (input == null) {
            printUsage();
            System.exit(2);
        }

        String code = Files.readString(input);

        // strict=false, useCache=false (no on-disk classloader cache),
        // enableOperations honors --no-ops, no Session, useExtra=true
        // to match the default flag surface in `LeekScript.compileSnippet`.
        Options options = new Options(version, false, false, enableOps, null, true);

        String pathForFile = pathOverride != null ? pathOverride : input.toString();
        AIFile file = new AIFile(
            pathForFile,
            code,
            System.currentTimeMillis(),
            version,
            aiId,
            false
        );
        String javaClass = "AI_" + aiId;
        file.setJavaClass(javaClass);
        file.setRootClass("AI");
        file.setId(aiId);

        AICode result = new IACompiler().compile(file, javaClass, options);

        if (out == null) {
            // Print to stdout — useful for quick one-shots and piping.
            System.out.print(result.getJavaCode());
        } else {
            Files.createDirectories(out);
            Path javaPath = out.resolve(javaClass + ".java");
            Path linesPath = out.resolve(javaClass + ".lines");
            Files.writeString(javaPath, result.getJavaCode());
            // `.lines` is materialized by writeErrorFunction; if the
            // caller never reaches that path the buffer stays empty.
            // For a source-only emit we still want a parseable sidecar,
            // so render the live linesMap manually.
            StringBuilder linesBuf = new StringBuilder();
            result.getLinesMap().forEach((javaLine, mapping) -> {
                linesBuf.append(javaLine)
                    .append(' ')
                    .append(mapping.getAI())
                    .append(' ')
                    .append(mapping.getLeekScriptLine())
                    .append('\n');
            });
            Files.writeString(linesPath, linesBuf.toString());
            System.err.println("wrote " + javaPath + " and " + linesPath);
        }
    }

    private static void printUsage() {
        System.err.println("""
            usage: leekscript.tools.EmitJava [options] <input.leek>

            options:
              --version <N>       Leekscript version (default 4)
              --ai-id <N>         id baked into AI_<N> class name (default 0)
              --out-dir <dir>     write .java + .lines into this directory
              --no-ops            skip per-statement ops(1) instrumentation
              -h, --help          this message
            """);
    }
}
