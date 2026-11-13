//! Regression pins for the syntax-gap round's engine regressions: the
//! corpus rows whose behavior the round *broke* (they errored or executed
//! before the round's commits and stopped after). One test per corpus
//! feature row, asserting the row's own observable: the query errors
//! (SyntaxError class) or returns the pinned rows.
//!
//! Every query here is copied from the acceptance feature file named in
//! the test's doc comment, so a later dialect change that reopens one of
//! them fails here first.

mod common;

use relate::Value;

use common::{connection, err_text};

/// AggregationAcceptance "Aggregation in predicate expression #2" (42I24):
/// an aggregate inside a list predicate of a projection *subclause* has no
/// group to fold. Three rows: aggregating WITH + WHERE, aggregating WITH +
/// ORDER BY, aggregating RETURN + ORDER BY.
#[test]
fn aggregate_inside_a_quantifier_in_a_projection_subclause_is_rejected() {
    let mut c = connection();
    for query in [
        "UNWIND [1,2,3] AS rows \
         WITH collect(rows) AS coll \
           WHERE none(var0 IN [1,2,3] WHERE var0 < 1 AND NOT isEmpty(collect(rows))) \
         RETURN *",
        "UNWIND [1,2,3] AS rows \
         WITH collect(rows) AS coll \
           ORDER BY none(var0 IN [1,2,3] WHERE var0 < 1 AND NOT isEmpty(collect(rows))) \
         RETURN *",
        "UNWIND [1,2,3] AS rows \
         RETURN collect(rows) AS coll \
           ORDER BY none(var0 IN [1,2,3] WHERE var0 < 1 AND NOT isEmpty(collect(rows)))",
    ] {
        let error = err_text(c.query(query));
        assert!(
            error.contains("InvalidAggregation"),
            "{query}\n  expected the InvalidAggregation rejection, got: {error}"
        );
    }
}

/// ExpressionAcceptance "[3] Pattern predicate in an aggregating WITH
/// referencing a non-grouping variable should fail" (42I18): the
/// comprehension filter's pattern reads the ungrouped `s`.
#[test]
fn a_pattern_predicate_in_a_comprehension_filter_is_grouping_checked() {
    let mut c = connection();
    for predicate in ["exists((s)-[:OTHER]->(o))", "EXISTS { (s)-[:OTHER]->(o) }"] {
        let query = format!(
            "MATCH (o) OPTIONAL MATCH (s)-[e:REL]->(o) \
             WITH o, size([x IN collect(e) WHERE {predicate}]) AS cnt RETURN cnt"
        );
        let error = err_text(c.query(&query));
        assert!(
            error.contains("AmbiguousAggregationExpression"),
            "{query}\n  expected the non-grouping rejection, got: {error}"
        );
    }
}

/// AggregationAcceptance "Should not group by a per-row value imported into
/// a correlated CALL subquery - reduce": `collect(y)` inside the reduce's
/// list folds the (empty) group, so the correlated import drives the
/// result.
#[test]
fn reduce_over_collect_inside_a_correlated_call_body_reads_the_group() {
    let mut c = connection();
    c.query("CREATE ({p: 1}), ({p: 2})").unwrap();
    let r = c
        .query(
            "MATCH (a) CALL (a) { \
               UNWIND [] AS y \
               RETURN reduce(acc = a.p, x IN collect(y) | acc + x) AS res \
             } RETURN a.p AS p, res",
        )
        .unwrap();
    let mut rows: Vec<(i64, Value)> = r
        .rows
        .iter()
        .map(|row| {
            let Value::Int(p) = row[0] else {
                panic!("p must be an integer: {row:?}")
            };
            (p, row[1].clone())
        })
        .collect();
    rows.sort_by_key(|(p, _)| *p);
    assert_eq!(
        rows,
        vec![(1, Value::Int(1)), (2, Value::Int(2))],
        "each imported row reduces its empty collect to its own a.p"
    );
}

/// The sibling comprehensions that keep the same import-scope visibility
/// (list comprehension, map projection, iterable predicate).
#[test]
fn imported_values_stay_visible_in_comprehension_bodies() {
    let mut c = connection();
    c.query("CREATE ({p: 1}), ({p: 2})").unwrap();
    let r = c
        .query(
            "MATCH (a) CALL (a) { \
               UNWIND [] AS y \
               RETURN [x IN collect(y) | x + a.p] AS l, a{t: a.p, cnt: collect(y)} AS m, \
                      any(x IN collect(y) WHERE x > a.p) AS b \
             } RETURN a.p AS p, l, m, b ORDER BY p",
        )
        .unwrap();
    assert_eq!(r.rows.len(), 2, "one row per import: {:?}", r.rows);
    assert_eq!(r.rows[0][1], Value::List(Vec::new().into()));
    assert_eq!(r.rows[0][3], Value::Bool(false));
}

/// CypherConcurrentTransactionsAcceptance "DISJOINT BY randomUUID rejected
/// as non-deterministic" (42N7B): a `DISJOINT BY` key partitions the batch
/// by value, so a fresh UUID per row defeats it.
#[test]
fn a_non_deterministic_disjoint_by_key_is_rejected() {
    let mut c = connection();
    let error = err_text(c.query(
        "UNWIND range(0, 9) AS i CALL (i) { CREATE (:A {i: i}) } \
         IN CONCURRENT TRANSACTIONS OF 5 ROWS DISJOINT BY (randomUUID())",
    ));
    assert!(
        error.contains("NonDeterministicDisjointBy"),
        "expected the determinism rejection, got: {error}"
    );
    // A deterministic key still parses.
    c.query(
        "UNWIND range(0, 9) AS i CALL (i) { CREATE (:A {i: i}) } \
         IN CONCURRENT TRANSACTIONS OF 5 ROWS DISJOINT BY (i)",
    )
    .expect("a deterministic DISJOINT BY key stays legal");
}

/// The pattern-shape restriction covers a FULLTEXT source, not only a
/// vector one: multiple bound variables, predicates on another element,
/// and multiple pattern parts are all rejected. No index is created
/// here — the rejection is a compile-time rule.
#[test]
fn fulltext_search_clauses_keep_the_rule_0_pattern_shape() {
    let mut c = connection();
    for (query, detail) in [
        (
            "MATCH (m:Movie)-[r]->() SEARCH m IN (FULLTEXT INDEX moviePlots \
             FOR 'magic genie' LIMIT 5) RETURN m.title AS title",
            "SearchClauseMultipleBoundVariables",
        ),
        (
            "MATCH (m:Movie)-[]->(:Book) SEARCH m IN (FULLTEXT INDEX moviePlots \
             FOR 'magic genie' LIMIT 5) RETURN m.title AS title",
            "SearchClauseInvalidPredicates",
        ),
        (
            "MATCH (m:Movie), () SEARCH m IN (FULLTEXT INDEX moviePlots \
             FOR 'magic genie' LIMIT 5) RETURN m.title AS title",
            "SearchClauseTooComplexPattern",
        ),
    ] {
        let error = err_text(c.query(query));
        assert!(
            error.contains(detail),
            "{query}\n  expected {detail}, got: {error}"
        );
    }
}

/// FulltextSearchAcceptance "[60] Self-referencing analyzer with a fulltext
/// index" (42I75): `WITH ANALYZER m.lang` depends on the binding variable.
#[test]
fn a_search_analyzer_may_not_read_the_binding_variable() {
    let mut c = connection();
    let error = err_text(c.query(
        "MATCH (m:Movie) SEARCH m IN (FULLTEXT INDEX moviePlots \
         FOR 'magic genie' WITH ANALYZER m.lang LIMIT 5) RETURN m.title AS title",
    ));
    assert!(
        error.contains("SelfReferencingSearch"),
        "expected the self-reference rejection, got: {error}"
    );
}

/// GpmSyntaxMixingAcceptance "Conflicting syntax in separate statements in
/// same EXISTS sub-query" (42I39): a shortest-path function and an explicit
/// match mode in two clauses of ONE subquery body are rejected.
#[test]
fn shortest_path_mixing_inside_a_subquery_body_is_rejected() {
    let mut c = connection();
    for body in [
        "RETURN EXISTS { MATCH p = shortestPath((n:A)-[:R*]-(m:!A)) \
         MATCH REPEATABLE ELEMENTS (n)-[:!S]-+(m:C) }",
        "RETURN EXISTS { MATCH p = shortestPath((n:A)-[:R*]-(m:!A)) \
         MATCH q = SHORTEST 1 (n)-[:!S]-+(:C) }",
    ] {
        let error = err_text(c.query(body));
        assert!(
            error.contains("InvalidShortestPathMixing"),
            "{body}\n  expected the 42I39 rejection, got: {error}"
        );
    }
    // A top-level statement keeps separate MATCH clauses legal (CIP-40
    // path-selector-mixing [11]/[13]/[18]).
    c.query(
        "MATCH (s:S) MATCH (t:T) \
         MATCH p = shortestPath((s)-[:ROAD*]-(g:GAS)), q = shortestPath((g)-[:ROAD*]-(t)) \
         MATCH r = SHORTEST 1 PATH (s)-[:ROAD*]-(t) \
         RETURN size(relationships(p)) + size(relationships(q)) > size(relationships(r))",
    )
    .expect("separate top-level MATCH clauses stay legal");
}

/// GpmSyntaxMixingAcceptance "Conflicting syntax in separate statements
/// within a CALL subquery" (42I10/42I29): one CALL body is one label-mixing
/// scope.
#[test]
fn a_call_subquery_body_is_one_label_mixing_scope() {
    let mut c = connection();
    for body in [
        "CALL () { MATCH (n:A:B) MATCH (n)--(:A&!B) RETURN * } RETURN *",
        "CALL () { CREATE (n:A:B) CREATE (m:C&D) RETURN * } RETURN *",
        "CALL () { MATCH (n:A:B) MATCH (n)--(IS A) RETURN * } RETURN *",
    ] {
        let error = err_text(c.query(body));
        assert!(
            error.contains("InvalidLabelExpression")
                || error.contains("MixedIsAndColonConjunction"),
            "{body}\n  expected a label-mixing rejection, got: {error}"
        );
    }
    // Statements that each stay on one side of the mixing line still run.
    c.query("CALL () { MATCH (n:A:B) MATCH (n)--+(:A) RETURN * } RETURN *")
        .expect("unrelated-feature mixing inside one CALL body stays legal");
}
