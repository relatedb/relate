//! The percentile aggregates' tests: the argument's two rejection classes,
//! the rank arithmetic at its edges, and REL8-2030's discrete pick — split
//! from `aggregate_tests` so the percentile behaviours have one file
//! (stellar REL8-2030 F-1-2).

use super::aggregate_tests::{run, run_result};
use super::test_support::{distinct_percentile, group_with_vals, group_with_xs, percentile_fn};
use relate_core::{Error, Value};
use relate_ir::Expr;

/// The percentile argument's two failures are distinct: a non-numeric value
/// is the wrong-type class, a number outside [0, 1] the `NumberOutOfRange`
/// kind on the run-time `ArgumentValue` variant.
#[test]
fn percentile_argument_splits_wrong_type_from_out_of_range() {
    let group = group_with_vals(&[Value::Int(1), Value::Int(2)]);
    let non_numeric = Expr::Function {
        name: "percentileCont".into(),
        distinct: false,
        args: vec![Expr::Variable("x".into()), Expr::String("0.5".into())],
    };
    let error = run_result(&group, &non_numeric).unwrap_err();
    assert!(
        matches!(&error, Error::InvalidArgumentType(detail) if detail == "percentile must be a number"),
        "{error}"
    );
    let error = run_result(&group, &percentile_fn("percentileCont", 2.0)).unwrap_err();
    assert!(matches!(&error, Error::ArgumentValue { .. }), "{error}");
    assert_eq!(
        error.to_string(),
        "semantic error: NumberOutOfRange: percentile"
    );
}

/// LABS-709: an empty group has no percentile to report — and no row for the
/// second argument to be evaluated against. The `AggScope` seed policy
/// (`group.first().or(import)`) never fabricates a row, so the percentile
/// evaluation does not either.
#[test]
fn percentile_over_an_empty_group_is_null() {
    for name in ["percentileCont", "percentileDisc"] {
        assert_eq!(run(&[], &percentile_fn(name, 0.5)), Value::Null, "{name}");
    }
}

/// LABS-709: the percentile sample sorts with the engine's total order
/// (`f64::total_cmp`), which places NaN after every number — a defined
/// rule, never `partial_cmp(...).unwrap_or(Equal)`'s fabricated position.
/// With NaN sorted last, the p=0.5 position reads the middle value.
#[test]
fn percentile_samples_sort_nan_by_total_order() {
    let group = group_with_vals(&[Value::Float(f64::NAN), Value::Float(1.0), Value::Float(3.0)]);
    for name in ["percentileCont", "percentileDisc"] {
        assert_eq!(
            run(&group, &percentile_fn(name, 0.5)),
            Value::Float(3.0),
            "{name}"
        );
    }
}

/// LABS-709: percentile position 0 is the first sample value for both
/// kinds. The discrete rank arithmetic is total (`saturating_sub`), so
/// `p == 0.0` cannot wrap `(len * p).ceil() as usize - 1` to the last
/// element in a release build. The continuous kind interpolates to an f64;
/// the discrete kind answers the INTEGER operand itself.
#[test]
fn percentile_zero_is_the_first_sample_value() {
    assert_eq!(
        run(
            &group_with_xs(&[3, 1, 2]),
            &percentile_fn("percentileCont", 0.0)
        ),
        Value::Float(1.0)
    );
    assert_eq!(
        run(
            &group_with_xs(&[3, 1, 2]),
            &percentile_fn("percentileDisc", 0.0)
        ),
        Value::Int(1)
    );
}

/// REL8-2030 (corpus 27): `percentileDisc` returns the picked operand
/// itself — an INTEGER keeps its type and its exact identity above 2^53 —
/// while the continuous kind interpolates through an f64.
#[test]
fn percentile_disc_answers_the_input_value() {
    let high = 9_007_199_254_740_993_i64; // 2^53 + 1, not representable in f64
    let group = group_with_vals(&[Value::Int(high), Value::Int(1)]);
    assert_eq!(
        run(&group, &percentile_fn("percentileDisc", 1.0)),
        Value::Int(high)
    );
    assert_eq!(
        run(&group, &percentile_fn("percentileDisc", 0.0)),
        Value::Int(1)
    );
    // The maximum i64, and a mixed sample keeping the picked element's type.
    let group = group_with_vals(&[Value::Int(i64::MAX)]);
    assert_eq!(
        run(&group, &percentile_fn("percentileDisc", 0.5)),
        Value::Int(i64::MAX)
    );
    let group = group_with_vals(&[Value::Int(1), Value::Float(2.5), Value::Int(3)]);
    assert_eq!(
        run(&group, &percentile_fn("percentileDisc", 0.9)),
        Value::Int(3)
    );
}

/// LABS-709: the single-sample percentile returns the sample itself, which
/// is load-bearing for ±Infinity — the general interpolation multiplies the
/// one value by 0.0, and `inf * 0.0` is NaN, so the degenerate-weight branch
/// is what keeps `percentileCont([inf], 0.5)` infinite rather than NaN.
#[test]
fn percentile_single_infinite_sample_stays_infinite() {
    for sample in [f64::INFINITY, f64::NEG_INFINITY] {
        assert_eq!(
            run(
                &group_with_vals(&[Value::Float(sample)]),
                &percentile_fn("percentileCont", 0.5)
            ),
            Value::Float(sample),
            "single sample {sample}"
        );
    }
}

/// rust-review (Medium): a zero interpolation weight must never turn an
/// infinite endpoint into NaN. A percentile position landing exactly on a
/// sample (`f == 0.0`) has one weight of 1.0 and the other of 0.0, so the
/// endpoint must be selected, not multiplied in — both endpoints are
/// pinned, and a mid-interval weight keeps an infinite endpoint infinite.
#[test]
fn percentile_interpolation_keeps_infinite_endpoints_infinite() {
    let group = group_with_vals(&[
        Value::Float(1.0),
        Value::Float(2.0),
        Value::Float(f64::NEG_INFINITY),
        Value::Float(f64::INFINITY),
    ]);
    // p == 0.0 selects the first sample: weight 1.0, the second weight 0.0.
    assert_eq!(
        run(&group, &percentile_fn("percentileCont", 0.0)),
        Value::Float(f64::NEG_INFINITY),
        "p == 0.0"
    );
    // p == 1.0 selects the last sample: weight 0.0, the last weight 1.0.
    assert_eq!(
        run(&group, &percentile_fn("percentileCont", 1.0)),
        Value::Float(f64::INFINITY),
        "p == 1.0"
    );
    // A non-degenerate weight (0 < f < 1) over an infinite endpoint already
    // stays infinite; pinned so the short-circuit cannot narrow that.
    assert_eq!(
        run(
            &group_with_vals(&[Value::Float(1.0), Value::Float(f64::INFINITY)]),
            &percentile_fn("percentileCont", 0.5)
        ),
        Value::Float(f64::INFINITY),
        "mid-interval weight"
    );
}

/// rust-review round 2: the pooled (`f64::total_cmp`) and retained
/// (`Value::order_by_cmp`) samples must agree on NaN's position. A negative
/// NaN — the bit pattern `sqrt(-1)` produces — sorts below `-inf` under
/// `total_cmp` alone, while the engine's total order ranks every NaN last;
/// the pooled sort now applies that same rule, so the plain, DISTINCT and
/// discrete percentiles read one order.
#[test]
fn negative_nan_sorts_last_on_both_percentile_paths() {
    let group = group_with_vals(&[
        Value::Float(-f64::NAN),
        Value::Float(f64::NEG_INFINITY),
        Value::Float(1.0),
    ]);
    for (label, call) in [
        ("pooled sample", percentile_fn("percentileCont", 0.0)),
        (
            "retained sample",
            distinct_percentile("percentileCont", 0.0),
        ),
        ("discrete pick", percentile_fn("percentileDisc", 0.0)),
    ] {
        assert_eq!(
            run(&group, &call),
            Value::Float(f64::NEG_INFINITY),
            "{label}"
        );
    }
}
