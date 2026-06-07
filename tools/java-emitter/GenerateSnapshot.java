import org.junit.platform.engine.discovery.DiscoverySelectors;
import org.junit.platform.launcher.LauncherDiscoveryRequest;
import org.junit.platform.launcher.core.LauncherDiscoveryRequestBuilder;
import org.junit.platform.launcher.core.LauncherFactory;
import org.junit.platform.launcher.listeners.SummaryGeneratingListener;
import org.junit.platform.launcher.listeners.TestExecutionSummary;

import java.io.PrintWriter;

/**
 * Discovers and runs a curated subset of the upstream Java test
 * suite with the {@code LEEK_SNAPSHOT} probe enabled. Each passing
 * inline assertion (`.equals(...)`, `.ops(...)`, `.almost(...)`)
 * appends one `(version, kind, value, jvm_ops, code)` row to the
 * snapshot file specified by the env var. The Rust parity test
 * (`tests/parity.rs::corpus_value_matches_snapshot`) then consumes
 * that file to cross-check both *value* and *runtime ops*.
 *
 * <p>We pick simple-construct test classes (Number, Boolean, String,
 * Operators, If, Loops, Function) — they cover the slice the Rust
 * interpreter handles today. Classes/lambdas/foreach-heavy suites
 * are left out of the curated list for now.
 */
public final class GenerateSnapshot {

    private static final String[] CLASSES = {
        "test.TestNumber",
        "test.TestBoolean",
        "test.TestString",
        "test.TestIf",
        "test.TestLoops",
        "test.TestFunction",
        "test.TestOperations",
    };

    public static void main(String[] args) {
        var selectors = new java.util.ArrayList<org.junit.platform.engine.DiscoverySelector>();
        for (String c : CLASSES) {
            selectors.add(DiscoverySelectors.selectClass(c));
        }
        LauncherDiscoveryRequest request = LauncherDiscoveryRequestBuilder.request()
            .selectors(selectors)
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

        // Don't fail the snapshot run on individual test failures —
        // we just want every *passing* case captured. The Rust
        // cross-check will surface real disagreements.
        long total = summary.getTotalFailureCount();
        if (total > 0) {
            System.out.println(
                "(note: " + total + " upstream test failures — captured passing rows only)");
        }
    }
}
