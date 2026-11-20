//! A mixed aggregation expression may read a grouping key through a
//! scalar function call: `WITH count(n) AS c MATCH (m) RETURN
//! toFloat(c) / count(m) AS coverage, c` (REL8-1869) — the later
//! aggregation's implicit grouping key `c` is a plain variable in the
//! current scope whatever aggregating WITH produced it, and is
//! referenceable through `toFloat` exactly like a property of a key
//! variable. The executed answers over a three-node fixture are pinned
//! here (the live-verified Neo4j 2026.10.1 behavior), beside the bisect
//! controls: the bare-alias base, the plain-variable twin that disproves
//! any provenance leak, and the rejections the relaxation must not
//! cross.

mod common;

use relate::Value;

use common::{connection, err_text};

/// Decode a (coverage, key) row set into native pairs sorted by coverage:
/// the control queries assert exact row sets, and the engine's row order
/// is group-insertion order.
fn sorted_coverage_rows(result: relate::QueryResult) -> Vec<(f64, i64)> {
    let mut rows: Vec<(f64, i64)> = result
        .rows
        .into_iter()
        .map(|row| match (&row[0], &row[1]) {
            (Value::Float(x), Value::Int(y)) => (*x, *y),
            other => panic!("expected (float, int) coverage rows, got {other:?}"),
        })
        .collect();
    rows.sort_unstable_by(|a, b| a.0.total_cmp(&b.0));
    rows
}

// The issue's exact query, executed and not just planned: the fixture is
// two :N nodes carrying p1 and one :M node — `MATCH (n) WHERE n.p1 IS
// NOT NULL` finds the two :N, the WITH collapses them to one row c = 2,
// the unlabeled `MATCH (m)` matches ALL THREE nodes (the :M included),
// and the RETURN groups by c and counts m per group: coverage =
// toFloat(2) / 3 = 0.6666666666666666, one row.
#[test]
fn aggregation_alias_reused_as_grouping_key_through_a_scalar_call() {
    let mut c = connection();
    c.query("CREATE (:N {p1: 1}), (:N {p1: 2}), (:M)").unwrap();

    // Planning is accepted: the plan nests the two aggregations with the
    // carried c as the second one's key.
    c.query(
        "EXPLAIN MATCH (n) WHERE n.p1 IS NOT NULL WITH count(n) AS c MATCH (m) \
         RETURN toFloat(c) / count(m) AS coverage, c",
    )
    .unwrap();

    let r = c
        .query(
            "MATCH (n) WHERE n.p1 IS NOT NULL WITH count(n) AS c MATCH (m) \
             RETURN toFloat(c) / count(m) AS coverage, c",
        )
        .unwrap();
    assert_eq!(
        r.rows,
        vec![vec![Value::Float(2.0 / 3.0), Value::Int(2)]],
        "rows: {:?}",
        r.rows
    );
}

// The bisect controls around the issue's shape: the alias reused bare
// beside an aggregation (the base that always worked), the
// plain-variable twin (the key's provenance is irrelevant — a
// non-aggregating WITH's alias reads identically through a call), and
// the single-clause form over the projected key expression itself.
#[test]
fn scalar_call_mixed_aggregation_controls() {
    let mut c = connection();
    c.query("CREATE (:N {p1: 1}), (:N {p1: 2}), (:M)").unwrap();

    // Bare beside the aggregation: one row — c = 2, count(m) = 3 over
    // the unlabeled (m).
    let r = c
        .query(
            "MATCH (n) WHERE n.p1 IS NOT NULL WITH count(n) AS c MATCH (m) \
             RETURN c, count(m)",
        )
        .unwrap();
    assert_eq!(r.rows, vec![vec![Value::Int(2), Value::Int(3)]]);

    // The plain-variable twin: two groups (p = 1, 2), count(m) = 3 each.
    let r = c
        .query(
            "MATCH (n) WHERE n.p1 IS NOT NULL WITH n.p1 AS p MATCH (m) \
             RETURN toFloat(p) / count(m) AS coverage, p",
        )
        .unwrap();
    assert_eq!(sorted_coverage_rows(r), [(1.0 / 3.0, 1), (2.0 / 3.0, 2)]);

    // The single-clause form: the key is the projected n.p1 itself, so
    // each group counts one n.
    let r = c
        .query(
            "MATCH (n) WHERE n.p1 IS NOT NULL \
             RETURN toFloat(n.p1) / count(n) AS coverage, n.p1",
        )
        .unwrap();
    assert_eq!(sorted_coverage_rows(r), [(1.0, 1), (2.0, 2)]);
}

// The relaxation extends exactly the existing grouping coverage, no
// further: a scalar call over a variable the clause does not group by,
// and a property of a `*`-carried variable read through a call (ADR-0198
// ruling 1 — `*` projects the variables, not their property
// expressions), both stay the ambiguous shape.
#[test]
fn scalar_call_over_a_non_grouped_variable_stays_ambiguous() {
    let mut c = connection();
    c.query("CREATE (:N {p1: 1}), (:N {p1: 2}), (:M)").unwrap();

    let err = err_text(c.query(
        "MATCH (n) WITH n.p1 AS p, n.p2 AS q MATCH (m) \
         RETURN toFloat(q) / count(m) AS x, p",
    ));
    assert!(
        err.contains("AmbiguousAggregationExpression"),
        "non-key variable through a call must stay ambiguous, got: {err}"
    );

    let err = err_text(c.query("MATCH (m) RETURN *, toFloat(m.p1) + count(*) AS x"));
    assert!(
        err.contains("AmbiguousAggregationExpression"),
        "star property leaf through a call must stay ambiguous, got: {err}"
    );
}

// A non-deterministic call over the key rides the same admission as a
// deterministic one (live Neo4j 2026.10.1 admits `coalesce(rand(), c) /
// count(m)`). The grouping differs: Neo4j groups by the per-row
// remainder — one group per row, count(m) = 1 each — while Relate's
// implicit group key stays the projected items, so the call takes one
// value per group: a single row, keyed c = 2, coverage a float below
// 1/3 (rand() in [0, 1) over the three m matches).
#[test]
fn non_deterministic_call_over_the_key_takes_one_value_per_group() {
    let mut c = connection();
    c.query("CREATE (:N {p1: 1}), (:N {p1: 2}), (:M)").unwrap();

    let r = c
        .query(
            "MATCH (n) WHERE n.p1 IS NOT NULL WITH count(n) AS c MATCH (m) \
             RETURN coalesce(rand(), c) / count(m) AS coverage, c",
        )
        .unwrap();
    let rows = sorted_coverage_rows(r);
    assert_eq!(rows.len(), 1, "rows: {rows:?}");
    let (coverage, key) = rows[0];
    assert_eq!(key, 2);
    assert!(
        (0.0..1.0 / 3.0).contains(&coverage),
        "coverage must be a rand-derived float, got {coverage}"
    );
}
