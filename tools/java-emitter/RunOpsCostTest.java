import org.junit.platform.engine.discovery.DiscoverySelectors;
import org.junit.platform.launcher.LauncherDiscoveryRequest;
import org.junit.platform.launcher.core.LauncherDiscoveryRequestBuilder;
import org.junit.platform.launcher.core.LauncherFactory;
import org.junit.platform.launcher.listeners.SummaryGeneratingListener;
import org.junit.platform.launcher.listeners.TestExecutionSummary;
import java.io.PrintWriter;

public final class RunOpsCostTest {
    public static void main(String[] args) {
        LauncherDiscoveryRequest request = LauncherDiscoveryRequestBuilder.request()
            .selectors(
                DiscoverySelectors.selectClass("test.TestOpsCost"),
                DiscoverySelectors.selectClass("test.TestOpsCostCorpus"))
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
        if (summary.getTotalFailureCount() > 0) {
            summary.printFailuresTo(out);
            System.exit(1);
        }
    }
}
