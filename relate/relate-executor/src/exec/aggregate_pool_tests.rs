//! LABS-1417: the plain percentile calls of one group over one operand
//! share a single `f64` buffer. The pins: grouped results equal an oracle
//! computed per group (no cross-group sharing), operands that differ stay
//! separate, DISTINCT and error behavior are unchanged, and the charge is
//! flat in the number of calls.

use std::collections::BTreeMap;

use super::test_support::{assert_expression_cap, group_with_xs, percentile_fn, try_exec_cypher};
use crate::exec::aggregate::AggSites;
use crate::graph::Graph;
use crate::overlay::ExecGraph;
use relate_core::Value;
use relate_ir::Expr;

fn float(value: &Value) -> Option<f64> {
    match value {
        Value::Float(x) => Some(*x),
        Value::Null => None,
        other => panic!("a percentile is a float or null: {other:?}"),
    }
}

/// The reference percentile of one sample, spelled independently of the
/// engine's helper.
fn oracle(sample: &[f64], p: f64, continuous: bool) -> Option<f64> {
    let mut v = sample.to_vec();
    v.sort_by(f64::total_cmp);
    let n = v.len();
    if n == 0 {
        return None;
    }
    if continuous {
        let h = p * (n - 1) as f64;
        let (lo, hi) = (h.floor() as usize, h.ceil() as usize);
        let f = h - lo as f64;
        Some(if f == 0.0 {
            v[lo]
        } else {
            v[lo] * (1.0 - f) + v[hi] * f
        })
    } else {
        let idx = ((n as f64 * p).ceil() as usize).saturating_sub(1);
        Some(v[idx])
    }
}

/// Grouped percentiles over several keys, every call reading the same
/// operand, equal a per-group oracle: groups of different sizes, a
/// single-row group, and a group whose operands are all null.
#[test]
fn grouped_percentiles_equal_a_per_group_oracle() {
    let g = Graph::default();
    // key -> operand values (None = null)
    let groups: Vec<(i64, Vec<Option<f64>>)> = vec![
        (0, (0..40).map(|i| Some((i * 7 % 13) as f64)).collect()),
        (1, vec![Some(5.0)]),
        (2, vec![None, None]),
        (3, (0..9).map(|i| Some(100.0 + i as f64 * 0.5)).collect()),
        (4, vec![Some(1.0), None, Some(3.0), Some(2.0)]),
    ];
    let mut rows = Vec::new();
    for (k, vals) in &groups {
        for v in vals {
            let n = v.map_or("null".to_string(), |x| format!("{x:?}"));
            rows.push(format!("{{k: {k}, n: {n}}}"));
        }
    }
    let ps = [0.0, 0.1, 0.5, 0.75, 0.999, 1.0];
    let calls: Vec<String> = ps
        .iter()
        .flat_map(|p| {
            [
                format!("percentileCont(r.n, {p:?})"),
                format!("percentileDisc(r.n, {p:?})"),
            ]
        })
        .collect();
    let query = format!(
        "UNWIND [{}] AS r RETURN r.k AS k, {} ORDER BY k",
        rows.join(", "),
        calls.join(", ")
    );
    let result = try_exec_cypher(&g, &query).expect("the grouped percentiles run");
    assert_eq!(result.rows.len(), groups.len());
    for (row, (k, vals)) in result.rows.iter().zip(&groups) {
        assert_eq!(row[0], Value::Int(*k));
        let sample: Vec<f64> = vals.iter().flatten().copied().collect();
        for (i, p) in ps.iter().enumerate() {
            for (j, continuous) in [true, false].into_iter().enumerate() {
                let got = float(&row[1 + 2 * i + j]);
                assert_eq!(got, oracle(&sample, *p, continuous), "key {k} p {p}");
            }
        }
    }
}

/// Two different operands in one row keep separate samples; mixed int and
/// float operands compare as numbers.
#[test]
fn different_operands_stay_separate_and_mixed_numbers_fold() {
    let g = Graph::default();
    let result = try_exec_cypher(
        &g,
        "UNWIND range(1, 9) AS i \
         WITH CASE WHEN i % 2 = 0 THEN i ELSE toFloat(i) + 0.5 END AS a, i * 100 AS b \
         RETURN percentileDisc(a, 0.5) AS a1, percentileCont(a, 0.5) AS a2, \
                percentileDisc(b, 0.5) AS b1, percentileCont(b, 0.25) AS b2",
    )
    .expect("the percentiles run");
    let mut a: Vec<f64> = (1..=9)
        .map(|i| if i % 2 == 0 { i as f64 } else { i as f64 + 0.5 })
        .collect();
    a.sort_by(f64::total_cmp);
    let b: Vec<f64> = (1..=9).map(|i| i as f64 * 100.0).collect();
    let row = &result.rows[0];
    assert_eq!(float(&row[0]), oracle(&a, 0.5, false));
    assert_eq!(float(&row[1]), oracle(&a, 0.5, true));
    assert_eq!(float(&row[2]), oracle(&b, 0.5, false));
    assert_eq!(float(&row[3]), oracle(&b, 0.25, true));
}

/// A DISTINCT percentile beside plain ones over the same operand folds its
/// own distinct sample; the plain ones see every row.
#[test]
fn a_distinct_percentile_beside_a_plain_one_is_correct() {
    let g = Graph::default();
    let result = try_exec_cypher(
        &g,
        "UNWIND [1, 1, 1, 1, 2, 3, 10] AS x \
         RETURN percentileDisc(x, 0.5) AS plain, percentileDisc(DISTINCT x, 0.5) AS dist, \
                percentileCont(x, 0.5) AS plain2",
    )
    .expect("the percentiles run");
    let row = &result.rows[0];
    assert_eq!(float(&row[0]), Some(1.0));
    assert_eq!(float(&row[1]), Some(2.0));
    assert_eq!(float(&row[2]), Some(1.0));
}

/// A non-numeric operand and a bad percentile argument fail as before, per
/// call: the argument is validated first, then the operand.
#[test]
fn pooled_errors_keep_their_wording_and_order() {
    let g = Graph::default();
    let error = try_exec_cypher(
        &g,
        "UNWIND [1, 'x'] AS x RETURN percentileCont(x, 0.5) AS a, percentileDisc(x, 0.5) AS b",
    )
    .err()
    .expect("the call is refused");
    assert!(
        error.to_string().contains("percentileCont() needs numbers"),
        "{error}"
    );
    let error = try_exec_cypher(
        &g,
        "UNWIND [1, 'x'] AS x RETURN percentileDisc(x, 0.5) AS b, percentileCont(x, 0.5) AS a",
    )
    .err()
    .expect("the call is refused");
    assert!(
        error.to_string().contains("percentileDisc() needs numbers"),
        "{error}"
    );
    // The argument is validated before the operand is blamed.
    let error = try_exec_cypher(
        &g,
        "UNWIND [1, 'x'] AS x RETURN percentileCont(x, 2.0) AS a, percentileDisc(x, 0.5) AS b",
    )
    .err()
    .expect("the call is refused");
    assert!(
        matches!(error, relate_core::Error::ArgumentValue { .. }),
        "{error}"
    );
    let error = try_exec_cypher(
        &g,
        "UNWIND [1, 2] AS x RETURN percentileCont(x, 'p') AS a, percentileDisc(x, 0.5) AS b",
    )
    .err()
    .expect("the call is refused");
    assert!(
        error.to_string().contains("percentile must be a number"),
        "{error}"
    );
    // No rows keyed: an empty keyed input yields no row and no error.
    let result = try_exec_cypher(
        &g,
        "UNWIND [] AS x RETURN x, percentileCont(x, 0.5) AS a, percentileDisc(x, 0.5) AS b",
    )
    .expect("no groups, no error");
    assert!(result.rows.is_empty());
}

/// Retained bytes after streaming `rows` rows through `k` percentile calls
/// over one operand: the accumulators' own running total.
fn charged_bytes(k: usize, rows: usize) -> u64 {
    let g = Graph::default();
    let graph = ExecGraph::read(&g);
    let calls: Vec<Expr> = (0..k)
        .map(|i| percentile_fn("percentileCont", (i as f64 + 1.0) / (k as f64 + 1.0)))
        .collect();
    let params = BTreeMap::new();
    let sites = AggSites::discover(&graph, calls.iter(), [], &params).expect("discovered");
    let mut accs = sites.open_group();
    let mut total = graph.retain_bytes(0).expect("a fresh total");
    for row in &group_with_xs(&(0..rows as i64).collect::<Vec<_>>()) {
        accs.offer(&graph, row, &params).expect("offered");
        accs.settle(&graph, &mut total).expect("settled");
    }
    total.total()
}

/// The memory shape: the charge does not grow with the number of calls and
/// does grow with the number of rows, at 8 bytes a row (not a `Value`).
#[test]
fn retained_charge_is_flat_in_the_number_of_percentile_calls() {
    let one = charged_bytes(1, 20_000);
    assert_eq!(charged_bytes(10, 20_000), one);
    assert!(charged_bytes(1, 40_000) > one);
    assert!(
        one < 20_000 * 8 * 2,
        "an f64 buffer, not a Value buffer: {one}"
    );
}

/// The failing-before shape: four percentiles over one operand under a cap
/// the single shared buffer fits in (k separate `Value` buffers did not).
#[test]
fn many_percentiles_fit_a_cap_one_buffer_fits() {
    let rows = 20_000i64;
    let cap = 400_000; // > 2 * rows * 8 reserved, < rows * size_of::<Value>() per call
    let mut g = Graph::default();
    g.configure_expression_memory_limit(Some(cap));
    let graph = ExecGraph::read(&g);
    let group = group_with_xs(&(0..rows).collect::<Vec<_>>());
    let call = |p: f64| percentile_fn("percentileCont", p);
    let all = Expr::List(vec![call(0.5), call(0.6), call(0.7), call(0.8)]);
    let value = super::test_support::agg_eval_rows(&graph, &group, &all, &BTreeMap::new())
        .expect("four percentiles share one buffer");
    assert!(matches!(value, Value::List(items) if items.len() == 4));
    // Non-shareable (DISTINCT) calls still retain Values and still refuse.
    let distinct = |p: f64| {
        let Expr::Function { name, args, .. } = percentile_fn("percentileCont", p) else {
            panic!("percentile_fn builds a function call")
        };
        Expr::Function {
            name,
            distinct: true,
            args,
        }
    };
    let all = Expr::List(vec![
        distinct(0.5),
        distinct(0.6),
        distinct(0.7),
        distinct(0.8),
    ]);
    let error = super::test_support::agg_eval_rows(&graph, &group, &all, &BTreeMap::new())
        .expect_err("the call is refused");
    assert_expression_cap(&error, cap);
}
