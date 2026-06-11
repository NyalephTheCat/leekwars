package test;

import org.junit.jupiter.api.MethodOrderer;
import org.junit.jupiter.api.Order;
import org.junit.jupiter.api.Test;
import org.junit.jupiter.api.TestMethodOrder;
import org.junit.jupiter.api.extension.ExtendWith;

/**
 * Covers the chained `Case.equals(...).ops(...)` API on {@link TestCommon}.
 *
 * <p>Each test pins down both the return value and the ops cost for one
 * Leekscript snippet — the same compilation feeds both assertions, so
 * the two expectations stay in sync. Cross-backend parity checks
 * (Rust's {@code leek-backend-java}) can compare against either
 * dimension here without re-running the JVM.
 *
 * <p>The `SummaryExtension` is what surfaces individual `[FAIL]`s as a
 * JUnit-level assertion failure (see `TestCommon.summary()`).
 */
@ExtendWith(SummaryExtension.class)
@TestMethodOrder(MethodOrderer.OrderAnnotation.class)
public class TestOpsCost extends TestCommon {

    @Test
    @Order(1)
    public void integerLiteral_returns_value_and_costs_one_op() {
        // `var x = 42`: rhs is a literal (cost 0), assignment adds the
        // +1 baseline that LeekVariableDeclarationInstruction folds
        // into `ops(VAL, 1)`. The `return x;` line is bare.
        code_v4_("var x = 42 return x").equalsOps("42", 1);
    }

    @Test
    @Order(2)
    public void inline_primitive_arithmetic() {
        // `1 + 2`: inline `1l + 2l`, op cost = 1 (add) + 1 (assign) = 2.
        code_v4_("var x = 1 + 2 return x").equalsOps("3", 2);
    }

    @Test
    @Order(3)
    public void multiplication_charges_mul_cost() {
        // MUL_COST = 2. Two var-decls add 1 + 3 = 4 ops on the second
        // one (mul=2 + assign=1) on top of the first (1 for `var a = 7`).
        // We attach the ops check to the second var-decl scope.
        code_v4_("var a = 7 var x = a * 3 return x").equalsOps("21", 4);
    }

    @Test
    @Order(4)
    public void chained_equals_then_ops_reuses_the_run() {
        // Two assertions, single compile+run. The cached `Result`
        // from `.equals(...)` is what `.ops(...)` checks against —
        // verifies the cache-reuse path on `Case.checkAll(...)`.
        var c = code_v4_("var x = 5 + 7 return x");
        c.equals("12").ops(2);
    }

    @Test
    @Order(5)
    public void chained_works_with_string_concat() {
        // String `+` lowers to `(String) add(...)`. Cost is 4: the
        // (Object)/(String) cast accounting differs from raw numeric
        // add — recorded here so a backend change to either side
        // surfaces immediately.
        code_v4_("var s = \"a\" + \"b\" return s").equalsOps("\"ab\"", 4);
    }

    @Test
    @Order(6)
    public void chained_works_with_function_call() {
        // Calls are zero-cost at the call site (the callee pays for
        // its own work). Outer ops cost: 1 (var-decl for `x`) + 3
        // (inside f's body: body-entry tick 1 + return expr cost 2).
        code_v4_("function f(n) { return n * n } var x = f(3) return x")
            .equalsOps("9", 4);
    }

    @Test
    @Order(7)
    public void standalone_ops_still_works_without_equals() {
        // The historical `.ops(N)`-only form (used in TestInterval &
        // friends) must keep behaving the same: it compiles+runs
        // fresh and checks ops without an equals assertion.
        code_v4_("var x = 42 return x").ops(1);
    }
}
