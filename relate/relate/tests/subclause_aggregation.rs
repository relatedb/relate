//! ADR-0453: aggregations in subclauses — the Cypher 25 forms the
//! acceptance suite pins, as repo tests. Every newly accepted shape has a
//! pinning test here, and every preserved rejection (the openCypher TCK
//! 2024.3 floor scenarios that must keep their verdicts) has one too.

mod common;

use relate::Value;

use common::{connection, err_text};

fn rows_of(result: &relate::QueryResult) -> Vec<String> {
    result.rows.iter().map(|row| format!("{row:?}")).collect()
}

// AggregationsInSubclauses [0]/[2]: an ORDER BY aggregation that is not
// projected recomputes over the group, alone and mixed with grouping keys.
#[test]
fn order_by_recomputes_a_non_projected_aggregation() {
    let mut c = connection();
    let r = c
        .query(
            "WITH 10 AS a, 2 AS b, 5 AS c UNWIND [1, 2, 3] AS x \
             RETURN a, b, c, count(x) AS cnt ORDER BY sum(a + b + c)",
        )
        .unwrap();
    assert_eq!(r.rows.len(), 1, "one group: {:?}", r.rows);
    assert_eq!(r.rows[0][3], Value::Int(3));

    let r = c
        .query(
            "WITH 10 AS a, 2 AS b, 5 AS c UNWIND [1, 2, 3] AS x \
             RETURN a, b, c, count(x) AS cnt ORDER BY sum(a + b + c) + a + b + c",
        )
        .unwrap();
    assert_eq!(r.rows.len(), 1);
}

// AggregationsInSubclauses [36]: an ORDER BY aggregation may reference a
// variable the projection does not carry.
#[test]
fn order_by_aggregation_references_non_projected_variable() {
    let mut c = connection();
    let r = c
        .query(
            "WITH 10 AS a, 2 AS b, 5 AS c UNWIND [1, 2, 3] AS x \
             WITH 1 AS y, c, count(a) AS cnt ORDER BY count(x) RETURN y, c, cnt",
        )
        .unwrap();
    assert_eq!(r.rows.len(), 1);
    assert_eq!(r.rows[0][2], Value::Int(3));
    let r = c
        .query(
            "WITH 10 AS a, 2 AS b, 5 AS c UNWIND [1, 2, 3] AS x \
             WITH 1 AS y, c, count(a) AS cnt ORDER BY count(*) RETURN y, c, cnt",
        )
        .unwrap();
    assert_eq!(r.rows[0][2], Value::Int(3));
}

// CIP-236: an explicit GROUP BY aggregates the clause even with no
// aggregate item, so its ORDER BY may aggregate over the group — the same
// rule the planner's `projection_aggregates` applies to the lowered
// projection.
#[test]
fn explicit_group_by_accepts_an_aggregating_order_by() {
    let mut c = connection();
    c.query("CREATE (:A {p: 'x'}), (:A {p: 'x'}), (:A {p: 'y'})")
        .unwrap();
    let r = c
        .query("MATCH (n:A) RETURN n.p GROUP BY n.p ORDER BY count(*) DESC, n.p")
        .unwrap();
    assert_eq!(
        r.rows,
        vec![
            vec![Value::String("x".into())],
            vec![Value::String("y".into())],
        ],
        "the group with two rows sorts first"
    );
}

// ProjectionSubclauseDeprecation [68]/[75]: an aggregating item's alias is
// a legal non-aggregate part beside a recomputed aggregation.
#[test]
fn order_by_mixes_projected_aggregate_alias_with_recomputation() {
    let mut c = connection();
    for query in [
        "UNWIND [1, 2, 3] AS a WITH 'k' AS k, sum(a) AS s ORDER BY s + sum(a) RETURN s",
        "UNWIND [1, 2, 3] AS a WITH 'k' AS k, sum(a) AS s ORDER BY sum(a) + count(a) RETURN s",
    ] {
        let r = c.query(query).unwrap();
        assert_eq!(r.rows.len(), 1, "{query}: {:?}", r.rows);
        assert_eq!(r.rows[0][0], Value::Int(6), "{query}");
    }
}

// AggregationsInSubclauses [2]/[6]: a WHERE after an aggregating WITH is
// HAVING — the predicate's aggregations recompute per group.
#[test]
fn where_after_aggregating_with_is_having() {
    let mut c = connection();
    let r = c
        .query(
            "UNWIND [{p: 1}, {p: 1}, {p: 2}] AS n \
             WITH n.p AS p, count(*) AS c WHERE sum(n.p) > 1 RETURN p, c",
        )
        .unwrap();
    // Group p=1 sums to 2 (kept); group p=2 sums to 2 (kept); a failing
    // group is dropped — pin both sides.
    assert_eq!(r.rows.len(), 2, "rows: {:?}", r.rows);
    let r = c
        .query(
            "UNWIND [{p: 1}, {p: 5}] AS n WITH n.p AS p, count(*) AS c WHERE sum(n.p) > 2 RETURN p, c",
        )
        .unwrap();
    assert_eq!(r.rows.len(), 1, "the sum=1 group is dropped: {:?}", r.rows);
    assert_eq!(r.rows[0][0], Value::Int(5));
}

// AggregationsInSubclauses [8]/[10]: a DISTINCT projection groups by its
// items; ORDER BY and WHERE aggregations recompute over those groups.
#[test]
fn distinct_projection_groups_for_subclause_aggregation() {
    let mut c = connection();
    let r = c
        .query(
            "WITH 10 AS a, 2 AS b, 5 AS c UNWIND [1, 2, 3] AS x \
             RETURN DISTINCT a, b, c ORDER BY sum(a + b + c)",
        )
        .unwrap();
    assert_eq!(r.rows.len(), 1, "rows: {:?}", r.rows);
    let r = c
        .query(
            "WITH 10 AS a, 2 AS b, 5 AS c UNWIND [1, 2, 2, 3, 3] AS x \
             WITH DISTINCT a, b, c, x WHERE sum(a + b + c) > 0 RETURN *",
        )
        .unwrap();
    assert_eq!(r.rows.len(), 3, "distinct x values: {:?}", r.rows);
}

// The `DISTINCT *` twin of the shapes above: `*` supplies no item list to
// group by, so an aggregating ORDER BY key or WHERE predicate fails closed
// with a typed error naming the combination — the WHERE form used to return
// zero rows and the ORDER BY form to sort on null keys, both by evaluating
// the aggregate per row through scalar `eval` and swallowing its error.
#[test]
fn distinct_star_subclause_aggregation_fails_closed() {
    let mut c = connection();
    for query in [
        "WITH 1 AS a, 2 AS b UNWIND [1, 2, 3] AS x \
         WITH DISTINCT * WHERE count(*) > 0 RETURN *",
        "WITH 1 AS a, 2 AS b UNWIND [1, 2, 3] AS x \
         RETURN DISTINCT * ORDER BY count(*)",
    ] {
        let err = err_text(c.query(query));
        assert!(
            err.contains("InvalidAggregation") && err.contains("DISTINCT *"),
            "{query}: {err}"
        );
    }
}

// AggregationAcceptance 298: an aggregation inside a quantified predicate
// of a HAVING WHERE folds the group.
#[test]
fn where_quantifier_over_an_aggregation_is_having() {
    let mut c = connection();
    let r = c
        .query(
            "UNWIND [1, 2, 3] AS rows \
             WITH collect(rows) AS coll WHERE none(v IN collect(rows) WHERE v < 1) RETURN *",
        )
        .unwrap();
    assert_eq!(r.rows.len(), 1, "rows: {:?}", r.rows);
    assert_eq!(
        r.rows[0][0],
        common::list(vec![Value::Int(1), Value::Int(2), Value::Int(3)])
    );
}

// AggregationAcceptance 696: a subquery expression's body may aggregate
// over an outer variable the outer projection groups by.
#[test]
fn subquery_expression_aggregates_over_outer_grouping_key() {
    let mut c = connection();
    c.query("CREATE ({p: 1}), ({p: 1}), ({p: 1}), ({p: 1})")
        .unwrap();
    let r = c
        .query("MATCH (a) RETURN a, COUNT { RETURN a.p + 1 + count(a) } + count(a) AS cnt")
        .unwrap();
    assert_eq!(r.rows.len(), 4, "one group per node: {:?}", r.rows);
    for row in &r.rows {
        assert_eq!(row[1], Value::Int(2), "cnt: {row:?}");
    }
    // The twin that must keep failing: the outer projection groups by
    // `a.p`, not `a`, so the body's `a` reference is non-grouping.
    let err =
        err_text(c.query("MATCH (a) RETURN a.p, COUNT { RETURN a.p + 1 + count(a) } + count(a)"));
    assert!(
        err.contains("AmbiguousAggregationExpression"),
        "non-grouping subquery reference: {err}"
    );
}

// AggregationAcceptance 769/771/869: a CALL subquery's imports are
// per-row constants — usable inside the body's aggregations without
// grouping by them, present even in the keyless empty group.
#[test]
fn call_subquery_imports_are_constants_in_aggregations() {
    let mut c = connection();
    let r = c
        .query(
            "WITH 0.2 AS tax \
             CALL (tax) { UNWIND [] AS y RETURN sum(y) + tax AS res } \
             RETURN res",
        )
        .unwrap();
    assert_eq!(r.rows.len(), 1);
    assert_eq!(r.rows[0][0], Value::Float(0.2));

    let mut c = connection();
    c.query("CREATE ({p: 1}), ({p: 2})").unwrap();
    let r = c
        .query(
            "MATCH (a) \
             CALL (a) { UNWIND [] AS y RETURN sum(y) + a.p AS res } \
             RETURN a.p AS p, res",
        )
        .unwrap();
    assert_eq!(r.rows.len(), 2, "rows: {:?}", r.rows);

    let r = c
        .query(
            "MATCH (a) \
             CALL (a) { UNWIND [] AS y RETURN a{t: a.p, cnt: collect(y)} AS res } \
             RETURN a.p AS p, res",
        )
        .unwrap();
    assert_eq!(
        r.rows.len(),
        2,
        "map projection over an import: {:?}",
        r.rows
    );
    let Value::Map(res) = &r.rows[0][1] else {
        panic!("res must be a map: {:?}", r.rows[0][1])
    };
    assert_eq!(res["cnt"], common::list(vec![]));
}

// The floor the relaxation must not cross (MatchWhere1 [15]): aggregation
// in a MATCH WHERE stays invalid.
#[test]
fn match_where_aggregation_stays_rejected() {
    let mut c = connection();
    let err = err_text(c.query("MATCH (a) WHERE count(a) > 10 RETURN a"));
    assert!(err.contains("InvalidAggregation"), "{err}");
}

// The floor the relaxation must not cross (ReturnOrderBy2 [14] /
// ReturnOrderBy6 [5]): aggregation in ORDER BY of a non-aggregating,
// non-DISTINCT projection stays invalid, and a complex non-aggregate part
// mixed with aggregation stays ambiguous.
#[test]
fn non_aggregating_order_by_shapes_stay_rejected() {
    let mut c = connection();
    let err = err_text(c.query("MATCH (n) RETURN n.num1 ORDER BY max(n.num2)"));
    assert!(err.contains("InvalidAggregation"), "{err}");
    // A complex non-aggregate part that is NOT a projection item stays
    // ambiguous...
    let err = err_text(c.query(
        "MATCH (me)--(you) RETURN me.age + you.age AS agg, count(*) AS cnt \
         ORDER BY me.age + you.age * 2 + count(*)",
    ));
    assert!(
        err.contains("AmbiguousAggregationExpression"),
        "complex non-aggregate part: {err}"
    );
    // ...while the verbatim projected expression is a grouping key, so
    // decomposing it beside an aggregation is valid (ReturnOrderBy6 [5]).
    c.query(
        "MATCH (me)--(you) RETURN me.age + you.age, count(*) AS cnt \
         ORDER BY me.age + you.age + count(*)",
    )
    .unwrap();
}

// AggregationsInSubclauses [24]-[33]: an aggregation sub-expression of a
// subclause may not reference a name the same clause declares as a
// constant — inside or outside the aggregate call — while an alias of a
// variable-derived item stays referenceable (ReturnOrderBy6 [2]).
#[test]
fn same_clause_constant_declarations_are_rejected_in_subclause_aggregation() {
    let mut c = connection();
    for query in [
        "WITH 10 AS a, 2 AS b, 5 AS c UNWIND [1, 2, 3] AS x \
         WITH 1 AS a, 10 AS b, c, count(a) AS cnt ORDER BY sum(a) RETURN *",
        "WITH 10 AS a, 2 AS b, 5 AS c UNWIND [1, 2, 3] AS x \
         WITH 1 AS a, 10 AS b, c, count(a) AS cnt ORDER BY count(a) + a RETURN *",
        "WITH 10 AS a, 2 AS b, 5 AS c UNWIND [1, 2, 3] AS x \
         RETURN 1 AS y, 10 AS b, c, count(a) AS cnt ORDER BY sum(c) + y",
    ] {
        let err = err_text(c.query(query));
        assert!(
            err.contains("cannot reference variables declared in the same clause"),
            "{query}: {err}"
        );
    }
    // A variable-derived alias (the TCK floor shape) and the verbatim
    // projected aggregation ([20]) stay valid.
    c.query("CREATE (:Person {age: 1})-[:KNOWS]->(:Person {age: 1})")
        .unwrap();
    let r = c
        .query(
            "MATCH (me:Person)--(you:Person) \
             RETURN me.age AS age, count(you.age) AS cnt ORDER BY age, age + count(you.age)",
        )
        .unwrap();
    assert_eq!(r.rows.len(), 1, "rows: {:?}", r.rows);
    let r = c
        .query(
            "WITH 10 AS a, 2 AS b, 5 AS c UNWIND [1, 2, 3] AS x \
             RETURN 1 AS a, 10 AS b, c, count(a) AS cnt ORDER BY count(a)",
        )
        .unwrap();
    assert_eq!(
        r.rows.len(),
        1,
        "verbatim projected aggregation: {:?}",
        r.rows
    );
}

// ProjectionSubclauseDeprecation [77]/[78]: `a.p AS a` re-introduces `a`
// from the outer variable — the deprecated ambiguous form, still valid in
// a subclause aggregation.
#[test]
fn outer_derived_reintroduction_stays_valid() {
    let mut c = connection();
    let r = c
        .query(
            "UNWIND [{a: {p: 1}, b: {p: 10}}, {a: {p: 2}, b: {p: 1}}] AS _row \
             WITH _row.a AS a, _row.b AS b \
             WITH a.p AS a, sum(b.p) AS s WHERE a + sum(b.p) > 0 RETURN a, s",
        )
        .unwrap();
    assert_eq!(r.rows.len(), 2, "rows: {:?}", r.rows);
}

// The ADR-0453 documentation example: the deprecated ambiguous reference
// resolves to the projected value.
#[test]
fn order_by_aggregation_sorts_by_recomputed_value() {
    let mut c = connection();
    let r = c
        .query(
            "UNWIND [{a: {p: 2}}, {a: {p: 1}}] AS _row WITH _row.a AS a \
             RETURN a.p AS p, count(*) AS c ORDER BY sum(a.p) DESC",
        )
        .unwrap();
    let rows = rows_of(&r);
    assert_eq!(r.rows.len(), 2);
    assert_eq!(
        r.rows[0][0],
        Value::Int(2),
        "descending recomputed sum: {rows:?}"
    );
    assert_eq!(r.rows[0][1], Value::Int(1));
}

// A plain (non-aggregating) WHERE after a GROUP BY reads the grouping
// key's variable — the key the projection does not carry. The
// aggregation's seed retains what the predicate reads, so the
// post-aggregation filter answers from the group's key instead of an
// undefined variable.
#[test]
fn plain_where_after_group_by_reads_the_grouping_key() {
    let mut c = connection();
    let r = c
        .query(
            "UNWIND [{p: 1}, {p: 2}, {p: 3}] AS x WITH x AS a \
             WITH count(*) AS cnt GROUP BY a WHERE 1 + a.p > 3 RETURN cnt",
        )
        .unwrap();
    assert_eq!(r.rows.len(), 1, "rows: {:?}", r.rows);
    assert_eq!(r.rows[0][0], Value::Int(1), "the surviving group's count");
}
