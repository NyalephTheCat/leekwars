import org.junit.platform.engine.discovery.DiscoverySelectors;
import org.junit.platform.launcher.LauncherDiscoveryRequest;
import org.junit.platform.launcher.core.LauncherDiscoveryRequestBuilder;
import org.junit.platform.launcher.core.LauncherFactory;
import org.junit.platform.launcher.listeners.SummaryGeneratingListener;
import org.junit.platform.launcher.listeners.TestExecutionSummary;

import java.io.PrintWriter;

/**
 * Discovers and runs the *entire* upstream {@code test} package with the
 * {@code LEEK_REFERENCE} probe enabled (see {@code TestCommon}). Each
 * passing value-bearing assertion (`.equals(...)`, `.ops(...)`,
 * `.almost(...)`) appends one row of
 * `(version, strict, kind, value, jvm_ops, code, generated_java)` to the
 * file named by the env var.
 *
 * <p>Unlike {@code GenerateSnapshot} (a curated 7-class slice that the
 * Rust interpreter fully handles), this runner intentionally covers
 * every {@code test.Test*} class so the Rust {@code leek-test-corpus}
 * build can embed a reference for the whole corpus — the official
 * value, op count, and Java emission per case. Classes without
 * {@code @Test} methods (the {@code TestCommon}/{@code TestAI} bases,
 * {@code BenchRAM}) contribute nothing and are skipped automatically.
 */
public final class GenerateReference {

    public static void main(String[] args) {
        LauncherDiscoveryRequest request = LauncherDiscoveryRequestBuilder.request()
            .selectors(DiscoverySelectors.selectPackage("test"))
            .build();

        var listener = new SummaryGeneratingListener();
        try (var launcher = LauncherFactory.openSession()) {
            var l = launcher.getLauncher();
            l.registerTestExecutionListeners(listener);
            l.execute(request);
        }

        TestExecutionSummary summary = listener.getSummary();
        var out = new PrintWriter(System.out, true);
        summary.printTo(out);

        // Don't fail the run on individual test failures — we capture
        // every *passing* case's reference row; the Rust cross-check
        // surfaces real disagreements.
        long total = summary.getTotalFailureCount();
        if (total > 0) {
            System.out.println(
                "(note: " + total + " upstream test failures — captured passing rows only)");
        }
    }
}
