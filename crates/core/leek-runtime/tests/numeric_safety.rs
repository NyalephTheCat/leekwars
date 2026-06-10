//! Numeric / allocation edge cases that previously overflowed or went
//! unmetered (Theme H of the robustness remediation).

use std::rc::Rc;

use leek_runtime::{IntervalValue, Value, builtin_op_cost, neg};

fn interval(start: f64, end: f64) -> Value {
    Value::Interval(Rc::new(IntervalValue {
        start: Some(start),
        end: Some(end),
        start_inclusive: true,
        end_inclusive: true,
        integer_typed: true,
        start_is_int: true,
        end_is_int: true,
        start_forces_real: false,
        end_forces_real: false,
    }))
}

#[test]
fn neg_of_i64_min_does_not_overflow() {
    // `-Number.MIN_VALUE`: `wrapping_neg(i64::MIN) == i64::MIN`, and must not
    // panic (debug) or trigger UB. Previously `-i` overflowed.
    match neg(&Value::Int(i64::MIN)) {
        Value::Int(n) => assert_eq!(n, i64::MIN),
        other => panic!("expected Int, got {other:?}"),
    }
    // Spot-check ordinary negation still works.
    match neg(&Value::Int(5)) {
        Value::Int(n) => assert_eq!(n, -5),
        other => panic!("expected Int, got {other:?}"),
    }
}

#[test]
fn wide_interval_count_does_not_overflow() {
    // `[-1e18 .. 1e18]` has ~2e18 elements — the count computation used to do
    // raw `i64` subtraction `(hi - lo + 1)` and overflow. It must now produce
    // a large positive count (saturating) without panicking.
    let n = interval(-1.0e18, 1.0e18).to_long();
    assert!(
        n > 0,
        "wide interval count should be a large positive, got {n}"
    );
}

#[test]
fn range_is_metered_by_result_size() {
    // `range(0, i64::MAX)` would allocate ~9.2e18 integers. Its op cost must
    // reflect that size so the interpreter's op budget trips *before* it
    // allocates (it charges `builtin_op_cost` prior to dispatch).
    let cost = builtin_op_cost("range", &[Value::Int(0), Value::Int(i64::MAX)], 4);
    assert!(
        cost >= u64::from(u32::MAX),
        "range over a huge span should cost a huge number of ops, got {cost}",
    );

    // A small range stays cheap.
    let small = builtin_op_cost("range", &[Value::Int(0), Value::Int(9)], 4);
    assert!(small < 1_000, "range(0,9) should be cheap, got {small}");

    // A reversed range allocates nothing → no batch cost.
    let empty = builtin_op_cost("range", &[Value::Int(10), Value::Int(0)], 4);
    assert!(empty < 1_000, "reversed range should be cheap, got {empty}");
}
