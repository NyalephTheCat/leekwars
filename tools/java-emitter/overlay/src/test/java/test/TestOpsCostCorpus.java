package test;

import java.nio.file.Files;
import java.nio.file.Path;
import java.util.ArrayList;
import java.util.List;

import org.junit.jupiter.api.Test;
import org.junit.jupiter.api.extension.ExtendWith;

/**
 * Data-driven equivalent of {@link TestOpsCost} that loads its
 * `(value, ops, snippet)` tuples from
 * {@code crates/backends/leek-backend-java/tests/fixtures/ops/cases.tsv}.
 *
 * <p>Both the upstream Java tests (this class) and the Rust parity
 * tests (`tests/parity.rs::ops_cost_matches_corpus`) consume the same
 * file, so an ops-cost change in either side surfaces as a CI failure
 * immediately. Add a row to the TSV and both sides must agree.
 */
@ExtendWith(SummaryExtension.class)
public class TestOpsCostCorpus extends TestCommon {

    @Test
    public void corpus_value_and_ops_match() throws Exception {
        // The TSV lives in the Rust crate so it ships with the
        // parity test that consumes it. Walk up from this class
        // file's working directory to find it.
        Path tsv = findCorpus();
        if (tsv == null) {
            // The corpus is optional from the Java side — if the
            // Rust workspace hasn't been laid out, we just skip
            // rather than fail the whole suite.
            System.out.println("[skip] cases.tsv not found; skipping ops-cost corpus");
            return;
        }
        List<String> lines = Files.readAllLines(tsv);
        int row = 0;
        for (String line : lines) {
            row++;
            String trimmed = line.strip();
            if (trimmed.isEmpty() || trimmed.startsWith("#")) continue;
            String[] cols = line.split("\t", 4);
            if (cols.length != 4) {
                throw new IllegalStateException(
                    "cases.tsv:" + row + ": expected `value\\tjvm_ops\\tstatic_ops\\tsnippet`, got: " + line);
            }
            String expectedValue = cols[0];
            long expectedJvmOps = Long.parseLong(cols[1].strip());
            // cols[2] is the static-emit ops count — consumed by the
            // Rust parity test, not by us.
            String snippet = cols[3];
            code_v4_(snippet).equalsOps(expectedValue, expectedJvmOps);
        }
    }

    /**
     * Resolve `cases.tsv` from any of the likely working directories
     * the test might run from — `leekscript/`, the repo root, the
     * gradle subproject. Returns null if none of them have it.
     */
    private static Path findCorpus() {
        List<Path> candidates = new ArrayList<>();
        candidates.add(Path.of(
            "crates/backends/leek-backend-java/tests/fixtures/ops/cases.tsv"));
        candidates.add(Path.of(
            "../crates/backends/leek-backend-java/tests/fixtures/ops/cases.tsv"));
        candidates.add(Path.of(
            "../../crates/backends/leek-backend-java/tests/fixtures/ops/cases.tsv"));
        candidates.add(Path.of(
            "../../../crates/backends/leek-backend-java/tests/fixtures/ops/cases.tsv"));
        // Absolute path override via env var, useful in CI.
        String env = System.getenv("LEEK_OPS_CORPUS");
        if (env != null) candidates.add(0, Path.of(env));
        for (Path p : candidates) {
            if (Files.exists(p)) return p;
        }
        return null;
    }
}
