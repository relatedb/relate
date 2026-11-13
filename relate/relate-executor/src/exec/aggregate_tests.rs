//! SumAcc / agg_eval tests (F-1-19: the test module outgrew the
//! implementation file).

use super::aggregate::desugar_star_keys;
use super::test_support::{
    agg_eval_rows, agg_fn, duration, group_with_vals, group_with_xs, percentile_fn,
};
use crate::graph::Graph;
use crate::overlay::ExecGraph;
use crate::row::Row;
use crate::test_support::ints;
use relate_core::{Error, Value};
use relate_ir::Expr;
use std::borrow::Cow;
use std::collections::BTreeMap;

/// The fallible form of [`run`]: the agg_eval error propagates so tests
/// can assert it.
fn run_result(group: &[Row], expr: &Expr) -> relate_core::Result<Value> {
    let g = Graph::default();
    let graph = ExecGraph::read(&g);
    agg_eval_rows(&graph, group, expr, &BTreeMap::new())
}

fn run(group: &[Row], expr: &Expr) -> Value {
    run_result(group, expr).unwrap()
}

// ADR-0205: every aggregation except count(*) skips null input values.
#[test]
fn sum_skips_nulls_and_empty_is_zero() {
    let mixed = group_with_vals(&[Value::Int(1), Value::Null, Value::Int(3)]);
    assert_eq!(run(&mixed, &agg_fn("sum")), Value::Int(4));
    let all_null = group_with_vals(&[Value::Null, Value::Null]);
    assert_eq!(run(&all_null, &agg_fn("sum")), Value::Int(0));
    assert_eq!(run(&[], &agg_fn("sum")), Value::Int(0));
}

#[test]
fn avg_min_max_skip_nulls_and_all_null_is_null() {
    let mixed = group_with_vals(&[Value::Int(1), Value::Null, Value::Int(3)]);
    assert_eq!(run(&mixed, &agg_fn("avg")), Value::Float(2.0));
    assert_eq!(run(&mixed, &agg_fn("min")), Value::Int(1));
    assert_eq!(run(&mixed, &agg_fn("max")), Value::Int(3));
    let all_null = group_with_vals(&[Value::Null]);
    assert_eq!(run(&all_null, &agg_fn("avg")), Value::Null);
    assert_eq!(run(&all_null, &agg_fn("min")), Value::Null);
    assert_eq!(run(&all_null, &agg_fn("max")), Value::Null);
}

/// LABS-709: `sum()` and `avg()` share one operand contract — one value
/// per row, numbers or durations (the strict offer, applied by the one
/// fold). A non-numeric, non-duration value raises instead of being
/// silently skipped (which used to change both the sum and the divisor
/// without telling the caller), in either order; a LIST operand is a
/// value like any other non-number — Neo4j's SUM()/AVG() never iterate
/// a list (`SumAggregator`'s init matches numbers and durations only),
/// so `sum(collect(x))`-shaped input is a type error in both aggregates.
/// Nulls still skip (the strict offer's null arm).
#[test]
fn sum_and_avg_reject_non_numeric_values_instead_of_skipping_them() {
    for name in ["sum", "avg"] {
        for vals in [
            vec![Value::Int(1), Value::String("x".into())],
            vec![Value::String("x".into()), Value::Int(1)],
            vec![Value::Bool(true)],
            vec![ints(&[1]), Value::Int(2)],
            vec![Value::Int(1), ints(&[2])],
        ] {
            let group = group_with_vals(&vals);
            let error = run_result(&group, &agg_fn(name)).unwrap_err();
            assert!(
                matches!(error, relate_core::Error::Type(_)),
                "{name}: {error}"
            );
            assert!(
                error
                    .to_string()
                    .contains(&format!("{name}() needs numbers")),
                "{name}: {error}"
            );
        }
    }
    // Nulls are not values: they still skip and never enter the divisor.
    let group = group_with_vals(&[Value::Int(2), Value::Null, Value::Int(4)]);
    assert_eq!(run(&group, &agg_fn("sum")), Value::Int(6));
    assert_eq!(run(&group, &agg_fn("avg")), Value::Float(3.0));
}

/// sum() promotes an integer overflow to a float, Neo4j's contract: the
/// i64 range is not a wrapping domain for aggregation
/// (`Sum.feature` "sum() of integers exceeding the long range yields a
/// float").
#[test]
fn sum_promotes_integer_overflow_to_float() {
    const MAX: i64 = i64::MAX;
    const MIN: i64 = i64::MIN;
    let cases: [(&[i64], f64); 5] = [
        (&[MAX, 1], 9.223372036854776e18),
        (&[1, MAX], 9.223372036854776e18),
        (&[MIN, -1], -9.223372036854776e18),
        (&[MAX, MAX], 1.8446744073709552e19),
        (&[MAX, 1, -1], 9.223372036854776e18),
    ];
    for (values, expected) in cases {
        assert_eq!(
            run(&group_with_xs(values), &agg_fn("sum")),
            Value::Float(expected),
            "sum({values:?})"
        );
    }
    // No overflow: the result stays an integer.
    assert_eq!(
        run(&group_with_xs(&[MAX, 0]), &agg_fn("sum")),
        Value::Int(MAX)
    );
}

/// avg() is a running mean, not a divided sum: an overflowing `f64` sum
/// would turn two max-double values into infinity, and a wrapped i64 sum
/// would turn the large-integer cases negative (`Avg.feature` "avg() over
/// large floats" / "avg() over integers whose sum exceeds the long range").
#[test]
fn avg_does_not_overflow_through_an_accumulated_sum() {
    const DBL_MAX: f64 = f64::MAX;
    let floats: [(&[f64], f64); 4] = [
        (&[DBL_MAX, DBL_MAX], DBL_MAX),
        (&[-DBL_MAX, -DBL_MAX], -DBL_MAX),
        (&[DBL_MAX, -DBL_MAX], 0.0),
        (&[DBL_MAX, 1.0], 8.988465674311579e307),
    ];
    for (values, expected) in floats {
        let group = group_with_vals(&values.iter().map(|v| Value::Float(*v)).collect::<Vec<_>>());
        assert_eq!(
            run(&group, &agg_fn("avg")),
            Value::Float(expected),
            "avg({values:?})"
        );
    }
    const MAX: i64 = i64::MAX;
    const MIN: i64 = i64::MIN;
    let ints_cases: [(&[i64], f64); 5] = [
        (&[MAX, MAX], 9.223372036854776e18),
        (&[MAX, MAX, MAX], 9.223372036854776e18),
        (&[MAX, 1], 4.611686018427388e18),
        (&[MIN, MIN], -9.223372036854776e18),
        (&[MIN, -1], -4.611686018427388e18),
    ];
    for (values, expected) in ints_cases {
        assert_eq!(
            run(&group_with_xs(values), &agg_fn("avg")),
            Value::Float(expected),
            "avg({values:?})"
        );
    }
    // Once the sum is not finite, the overflow-safe mean takes over:
    // `mean - mean/n + value/n` never forms `value - mean`, so the
    // opposite-sign step cannot overflow it.
    let group = group_with_vals(&[
        Value::Float(1e308),
        Value::Float(1e308),
        Value::Float(-1e308),
    ]);
    assert_eq!(
        run(&group, &agg_fn("avg")),
        Value::Float(3.333333333333334e307)
    );
    // Inputs that are already infinite or NaN keep the divided-sum answer:
    // the running mean goes NaN on `Infinity - Infinity`, while the pinned
    // result is the sum's own infinity.
    for (values, expected) in [
        (vec![f64::NEG_INFINITY, 1.0], f64::NEG_INFINITY),
        (vec![f64::INFINITY, 1.0], f64::INFINITY),
        (vec![f64::INFINITY, f64::NEG_INFINITY], f64::NAN),
        (vec![f64::NAN], f64::NAN),
    ] {
        let group = group_with_vals(&values.iter().map(|v| Value::Float(*v)).collect::<Vec<_>>());
        let Value::Float(actual) = run(&group, &agg_fn("avg")) else {
            panic!("avg over {values:?} is not a float");
        };
        assert!(
            actual == expected || (actual.is_nan() && expected.is_nan()),
            "avg({values:?}) = {actual}, expected {expected}"
        );
    }
}

#[test]
fn collect_ignores_nulls_and_count_splits_null_from_star() {
    let mixed = group_with_vals(&[Value::Int(1), Value::Null, Value::Int(3)]);
    assert_eq!(
        run(&mixed, &agg_fn("collect")),
        Value::List(vec![Value::Int(1), Value::Int(3)].into())
    );
    assert_eq!(run(&mixed, &agg_fn("count")), Value::Int(2));
    assert_eq!(run(&mixed, &Expr::CountStar), Value::Int(3));
}

#[test]
fn sum_and_avg_accept_durations() {
    let group = group_with_vals(&[
        duration(0, 0, 1_000_000_000),
        Value::Null,
        duration(0, 0, 3_000_000_000),
    ]);
    assert_eq!(run(&group, &agg_fn("sum")), duration(0, 0, 4_000_000_000));
    assert_eq!(run(&group, &agg_fn("avg")), duration(0, 0, 2_000_000_000));
    // Months and days accumulate component-wise.
    let group = group_with_vals(&[duration(1, 2, 0), duration(3, 4, 0)]);
    assert_eq!(run(&group, &agg_fn("sum")), duration(4, 6, 0));
}

#[test]
fn sum_and_avg_reject_mixed_numbers_and_durations() {
    let group = group_with_vals(&[Value::Int(1), duration(0, 0, 1)]);
    for name in ["sum", "avg"] {
        let err = run_result(&group, &agg_fn(name)).unwrap_err();
        assert!(
            err.to_string().contains("cannot mix numbers and durations"),
            "{name} should reject mixed input, got {err}"
        );
    }
    // Order does not matter: number first or duration first.
    let group = group_with_vals(&[duration(0, 0, 1), Value::Int(1)]);
    assert!(run_result(&group, &agg_fn("sum")).is_err());
}

/// The float a `stDev`/`stDevP` call yields, panicking on null or a
/// non-float: these tests pin the numeric cases only (the null cases
/// assert `Value::Null` directly).
fn stddev_of(vals: &[Value], name: &str) -> f64 {
    match run(&group_with_vals(vals), &agg_fn(name)) {
        Value::Float(f) => f,
        other => panic!("{name} over {vals:?}: expected a float, got {other:?}"),
    }
}

// StandardDeviation.feature: `[]` and `[null]` (and a null argument with no
// row at all) are the empty sample, and the empty sample is null — not the
// old 0.0.
#[test]
fn stdev_empty_or_null_only_sample_is_null() {
    for name in ["stdev", "stdevp"] {
        assert_eq!(
            run(&[], &agg_fn(name)),
            Value::Null,
            "{name} over a zero-row group"
        );
        assert_eq!(
            run(&group_with_vals(&[Value::Null, Value::Null]), &agg_fn(name)),
            Value::Null,
            "{name} over an all-null sample"
        );
    }
}

/// LABS-709: stDev/stDevP/percentileCont/percentileDisc reject a non-null
/// non-numeric operand with the same typed `{name}() needs numbers` error as
/// sum()/avg(), instead of silently dropping it from the sample and changing
/// the divisor. Neo4j raises here (`StdevAggregator.failType`, the
/// percentile aggregators' `CypherCoercions.asNumberValue`), so the shared
/// strict extraction states that policy once. Nulls still skip.
#[test]
fn stdev_and_percentiles_reject_non_numeric_operands() {
    // One row per aggregate: the call shapes differ only in the expression
    // constructor, so the behaviour table drives all four.
    for (name, expr) in [
        ("stdev", agg_fn("stdev")),
        ("stdevp", agg_fn("stdevp")),
        ("percentileCont", percentile_fn("percentileCont", 0.5)),
        ("percentileDisc", percentile_fn("percentileDisc", 0.5)),
    ] {
        for vals in [
            vec![Value::Int(1), Value::String("x".into())],
            vec![Value::Bool(true), Value::Int(1)],
        ] {
            let error = run_result(&group_with_vals(&vals), &expr).unwrap_err();
            assert!(
                matches!(error, relate_core::Error::Type(_)),
                "{name}: {error}"
            );
            assert!(
                error
                    .to_string()
                    .contains(&format!("{name}() needs numbers")),
                "{name}: {error}"
            );
        }
    }
    // Nulls are not operands: they still skip, so the deviation is taken
    // over the two values only.
    assert_eq!(
        stddev_of(&[Value::Int(1), Value::Null, Value::Int(3)], "stdev"),
        2.0f64.sqrt()
    );
}

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
/// element in a release build.
#[test]
fn percentile_zero_is_the_first_sample_value() {
    for name in ["percentileCont", "percentileDisc"] {
        assert_eq!(
            run(&group_with_xs(&[3, 1, 2]), &percentile_fn(name, 0.0)),
            Value::Float(1.0),
            "{name}"
        );
    }
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

/// LABS-709: the grouped path and the horizontal rewrite share one min/max
/// fold, and it keeps the FIRST of equal extremes — Neo4j's
/// `MaxAggregator.shouldUpdate`/`MinAggregator.shouldUpdate` update only on
/// a strict comparison — so `max([1, 1.0])` is the Int on both paths.
#[test]
fn min_max_keep_the_first_of_equal_extremes() {
    for name in ["min", "max"] {
        assert_eq!(
            run(
                &group_with_vals(&[Value::Int(1), Value::Float(1.0)]),
                &agg_fn(name)
            ),
            Value::Int(1),
            "{name}"
        );
        assert_eq!(
            run(
                &group_with_vals(&[Value::Float(1.0), Value::Int(1)]),
                &agg_fn(name)
            ),
            Value::Float(1.0),
            "{name}"
        );
    }
}

// StandardDeviation.feature pins `[5]` at 0.0 for both forms: one value has
// zero spread, and the sample divisor (0) must not turn that 0/0 into NaN.
#[test]
fn stdev_single_value_is_zero() {
    for name in ["stdev", "stdevp"] {
        assert_eq!(
            run(&group_with_xs(&[5]), &agg_fn(name)),
            Value::Float(0.0),
            "{name} over a single value"
        );
    }
}

// StandardDeviation.feature "over large floats": a naive sum of squared
// deviations squares values near f64::MAX, overflows to +Inf and reports Inf
// where the expected result is NaN.
#[test]
fn stdev_large_floats_do_not_overflow_to_infinity() {
    let max = f64::MAX;
    for name in ["stdev", "stdevp"] {
        let opposite = [Value::Float(max), Value::Float(-max)];
        let reversed = [Value::Float(-max), Value::Float(max)];
        let same = [Value::Float(max), Value::Float(max)];
        assert!(
            stddev_of(&opposite, name).is_nan(),
            "{name} over [MAX, -MAX]"
        );
        assert!(
            stddev_of(&reversed, name).is_nan(),
            "{name} over [-MAX, MAX]"
        );
        assert_eq!(stddev_of(&same, name), 0.0, "{name} over [MAX, MAX]");
    }
}

// StandardDeviation.feature "over NaN and infinities": any infinite or NaN
// input propagates to NaN, never to a finite number or +Inf.
#[test]
fn stdev_nan_and_infinite_inputs_are_nan() {
    let samples = [
        [Value::Float(1.0), Value::Float(f64::NAN)],
        [Value::Float(1.0), Value::Float(f64::INFINITY)],
        [Value::Float(f64::INFINITY), Value::Float(f64::NEG_INFINITY)],
        [Value::Float(f64::INFINITY), Value::Float(f64::INFINITY)],
    ];
    for name in ["stdev", "stdevp"] {
        for sample in &samples {
            assert!(stddev_of(sample, name).is_nan(), "{name} over {sample:?}");
        }
    }
}

#[test]
fn star_key_desugar_preserves_borrows_scope_and_empty_inputs() {
    let star = Expr::Star;
    let key = Expr::Variable("x".into());
    let rows = [Row::from([
        ("b".into(), Value::Int(2)),
        ("__agg_0".into(), Value::Int(9)),
        ("a".into(), Value::Int(1)),
    ])];
    let expanded = desugar_star_keys(vec![Cow::Borrowed(&star), Cow::Borrowed(&key)], &rows);
    assert!(matches!(&expanded[0], Cow::Owned(Expr::Variable(n)) if n == "a"));
    assert!(matches!(&expanded[1], Cow::Owned(Expr::Variable(n)) if n == "b"));
    assert!(matches!(&expanded[2], Cow::Borrowed(e) if std::ptr::eq(*e, &key)));

    let repeated = desugar_star_keys(vec![Cow::Borrowed(&star); 2], &rows);
    assert_eq!(repeated.len(), 4);
    assert_eq!(&repeated[..2], &repeated[2..]);

    let empty = desugar_star_keys(vec![Cow::Borrowed(&star)], &[]);
    assert!(matches!(empty.as_slice(), [Cow::Borrowed(Expr::Star)]));
    assert!(desugar_star_keys(vec![], &[]).is_empty());
    assert!(desugar_star_keys(vec![Cow::Borrowed(&star)], &[Row::new()]).is_empty());
    let no_star = desugar_star_keys(vec![Cow::Borrowed(&key)], &[]);
    assert!(matches!(no_star.as_slice(), [Cow::Borrowed(e)] if std::ptr::eq(*e, &key)));

    let no_star = vec![Cow::Borrowed(&key)];
    let allocation = no_star.as_ptr();
    let no_star = desugar_star_keys(no_star, &rows);
    assert_eq!(no_star.as_ptr(), allocation);
    assert!(matches!(no_star.as_slice(), [Cow::Borrowed(e)] if std::ptr::eq(*e, &key)));
}

#[test]
fn slice_of_collect_with_end() {
    // collect(x)[..1]
    let group = group_with_xs(&[1, 2, 3]);
    let expr = Expr::Slice {
        expr: Box::new(agg_fn("collect")),
        start: None,
        end: Some(Box::new(Expr::Int(1))),
    };
    assert_eq!(run(&group, &expr), Value::List(vec![Value::Int(1)].into()));
}

#[test]
fn subscript_of_collect() {
    // collect(x)[0]
    let group = group_with_xs(&[1, 2, 3]);
    let expr = Expr::Subscript {
        expr: Box::new(agg_fn("collect")),
        index: Box::new(Expr::Int(0)),
    };
    assert_eq!(run(&group, &expr), Value::Int(1));
}

#[test]
fn open_ended_slice_of_collect() {
    // collect(x)[1..]
    let group = group_with_xs(&[1, 2, 3]);
    let expr = Expr::Slice {
        expr: Box::new(agg_fn("collect")),
        start: Some(Box::new(Expr::Int(1))),
        end: None,
    };
    assert_eq!(
        run(&group, &expr),
        Value::List(vec![Value::Int(2), Value::Int(3)].into())
    );
}

#[test]
fn slice_of_collect_on_empty_group() {
    // Keyless global group with zero rows: collect(x)[..1] is [].
    let expr = Expr::Slice {
        expr: Box::new(agg_fn("collect")),
        start: None,
        end: Some(Box::new(Expr::Int(1))),
    };
    assert_eq!(run(&[], &expr), Value::List(vec![].into()));
}

// ADR-0198 ruling 2: an aggregate call inside a CASE arm belongs to the
// enclosing aggregating projection.
#[test]
fn case_arm_collects_over_the_group() {
    // CASE WHEN true THEN collect(x) + [3] ELSE [] END
    let group = group_with_xs(&[1, 2]);
    let expr = Expr::Case {
        when_then: vec![(
            Expr::Bool(true),
            Expr::Binary {
                op: relate_ir::BinaryOp::Add,
                left: Box::new(agg_fn("collect")),
                right: Box::new(Expr::List(vec![Expr::Int(3)])),
            },
        )],
        else_expr: Some(Box::new(Expr::List(vec![]))),
    };
    assert_eq!(
        run(&group, &expr),
        Value::List(vec![Value::Int(1), Value::Int(2), Value::Int(3)].into())
    );
}

#[test]
fn case_not_taken_arm_still_belongs_to_the_projection() {
    // CASE WHEN false THEN [] ELSE collect(x) + [3] END
    let group = group_with_xs(&[1, 2]);
    let expr = Expr::Case {
        when_then: vec![(Expr::Bool(false), Expr::List(vec![]))],
        else_expr: Some(Box::new(Expr::Binary {
            op: relate_ir::BinaryOp::Add,
            left: Box::new(agg_fn("collect")),
            right: Box::new(Expr::List(vec![Expr::Int(3)])),
        })),
    };
    assert_eq!(
        run(&group, &expr),
        Value::List(vec![Value::Int(1), Value::Int(2), Value::Int(3)].into())
    );
}

/// The gap-2 shape: a CASE whose WHEN reads the group's aggregate must
/// select before it evaluates — the unselected arm's erroring division
/// never runs (live Neo4j answers 1 for both spellings), while the
/// arm a group actually takes still evaluates and still fails.
#[test]
fn case_over_an_aggregate_skips_only_the_unselected_arms() {
    // CASE count(*) WHEN 0 THEN 1/0 ELSE 1 END, one row: count(*) is 1,
    // the WHEN falls through, and the THEN's 1/0 must not evaluate.
    let when_is_eq = |n: i64| Expr::Binary {
        op: relate_ir::BinaryOp::Eq,
        left: Box::new(Expr::CountStar),
        right: Box::new(Expr::Int(n)),
    };
    let div_by_zero = || Expr::Binary {
        op: relate_ir::BinaryOp::Div,
        left: Box::new(Expr::Int(1)),
        right: Box::new(Expr::Int(0)),
    };
    let skip_then = Expr::Case {
        when_then: vec![(when_is_eq(0), div_by_zero())],
        else_expr: Some(Box::new(Expr::Int(1))),
    };
    let group = group_with_xs(&[7]);
    assert_eq!(run(&group, &skip_then), Value::Int(1));
    // CASE count(*) WHEN 1 THEN 1 ELSE 1/0 END, one row: the WHEN holds
    // and the ELSE's 1/0 must not evaluate.
    let skip_else = Expr::Case {
        when_then: vec![(when_is_eq(1), Expr::Int(1))],
        else_expr: Some(Box::new(div_by_zero())),
    };
    assert_eq!(run(&group, &skip_else), Value::Int(1));
    // The taken arm still fails: over an empty group count(*) is 0, the
    // WHEN matches, and the THEN's 1/0 is the statement's error — live
    // Neo4j raises it too.
    let error = run_result(&[], &skip_then).unwrap_err();
    assert!(error.to_string().contains("/ by zero"), "{error}");
}

/// Gap 2 end to end, both spellings: the simple form and the searched
/// form lower to the one IR shape (the parser folds the test operand into
/// the arm's equality comparison) and both must answer `x = 1` — live
/// Neo4j's answer, with the unselected arm's `1/0` never evaluated.
#[test]
fn both_aggregate_case_forms_answer_the_selected_branch() {
    for q in [
        "UNWIND [1] AS k RETURN CASE count(*) WHEN 0 THEN 1/0 ELSE 1 END AS x",
        "UNWIND [1] AS k RETURN CASE WHEN count(*) = 0 THEN 1/0 ELSE 1 END AS x",
    ] {
        let result = super::test_support::try_exec_cypher(&Graph::default(), q)
            .unwrap_or_else(|e| panic!("{q}: {e}"));
        assert_eq!(result.rows, vec![vec![Value::Int(1)]], "{q}");
    }
}

/// A CASE without an aggregate keeps the row-scalar laziness inside an
/// aggregating projection: the `count(*)` item makes the projection
/// aggregate, but the CASE item itself carries no aggregate, so it rides
/// the scalar path and its unselected `1/0` never runs.
#[test]
fn a_plain_case_beside_an_aggregate_stays_lazy() {
    let result = super::test_support::try_exec_cypher(
        &Graph::default(),
        "UNWIND [1] AS k RETURN CASE 1 WHEN 1 THEN 1 ELSE 1/0 END AS x, count(*) AS c",
    )
    .expect("the plain CASE item keeps the scalar evaluation path");
    assert_eq!(result.rows, vec![vec![Value::Int(1), Value::Int(1)]]);
}

/// An aggregate that only an `ORDER BY` key reads keeps the documented
/// policy: a fold error there is a null sort key, never a statement
/// failure — even though the call now folds in the input row loop.
#[test]
fn an_order_by_only_aggregate_that_fails_to_fold_sorts_as_null() {
    let result = super::test_support::try_exec_cypher(
        &Graph::default(),
        "UNWIND ['a', 'b'] AS x RETURN count(*) AS c ORDER BY sum(x)",
    )
    .expect("a failing key aggregate is a null sort key");
    assert_eq!(result.rows, vec![vec![Value::Int(2)]]);
}

/// HAVING follows the predicate-positions contract (ADR-0458 §7): a
/// predicate-evaluation error aborts the statement — live 2026.10.1
/// surfaces 22N27 for a failing HAVING predicate and never silently
/// drops the group; an item's own fold error fails it the same way.
#[test]
fn a_having_only_aggregate_that_fails_to_fold_aborts_the_statement() {
    let error = super::test_support::try_exec_cypher(
        &Graph::default(),
        "UNWIND ['a', 'b'] AS x WITH count(*) AS c WHERE sum(x) > 0 RETURN c",
    )
    .err()
    .expect("a failing HAVING aggregate aborts the statement");
    assert!(error.to_string().contains("needs numbers"), "{error}");
    let error = super::test_support::try_exec_cypher(
        &Graph::default(),
        "UNWIND ['a', 'b'] AS x RETURN sum(x) AS s",
    )
    .err()
    .expect("an item's fold error fails the statement");
    assert!(error.to_string().contains("needs numbers"), "{error}");
}

/// ADR-0458 §7 at the HAVING position, through the query-level harness
/// (the same `GroupSubclauses::having` path a real statement drives): a
/// non-empty `collect(x)` converts to true so the group is kept, an empty
/// one drops it (the keyless phantom group), and a non-boolean (`count(x)`)
/// is the typed rejection live raises (22N01) — never a silent group drop.
#[test]
fn the_having_predicate_converts_lists_and_rejects_non_booleans() {
    // count(x) is an INTEGER per group: the typed rejection.
    let error = super::test_support::try_exec_cypher(
        &Graph::default(),
        "UNWIND [1] AS x WITH count(x) AS c WHERE count(x) RETURN c",
    )
    .err()
    .expect("a non-boolean HAVING is the typed rejection");
    assert!(matches!(error, Error::InvalidArgumentType(_)), "{error}");
    assert!(error.to_string().contains("boolean"), "{error}");

    // collect(x) over a non-empty input is a non-empty LIST: converted,
    // the group is kept; over an empty input the keyless phantom group
    // collects an empty LIST: false, the group is dropped.
    for (unwind, expected) in [("[1, 2]", 1), ("[]", 0)] {
        let cypher = format!("UNWIND {unwind} AS x WITH collect(x) AS c WHERE collect(x) RETURN c");
        let rows = super::test_support::try_exec_cypher(&Graph::default(), &cypher)
            .unwrap()
            .rows
            .len();
        assert_eq!(rows, expected, "{cypher}");
    }
}
