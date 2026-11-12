//! LABS-711 clause-shape notification detectors: the deprecated projection
//! subclause references, the `CREATE` self-reference, the repeated
//! relationship variable, the unsatisfiable relationship type expression,
//! the subquery and `SEARCH` shadowing facts, the exhaustive shortest path,
//! occurrence-scoped dedup, and the fulltext procedure deprecation. Split
//! out of `statement_notifications.rs` so each test file stays under the
//! 1k-line boundary.

mod common;

use common::{connection, notifications_of_kind};
use relate_model::wire::NotificationKind;

/// The count of one kind in a result's notification list — the shared
/// filter's length.
fn count_of(result: &relate::QueryResult, kind: NotificationKind) -> usize {
    notifications_of_kind(result, kind).len()
}

/// LABS-711: a projection `ORDER BY` expression referencing a variable the
/// projection's own alias re-binds is the deprecated ambiguous reference;
/// a bare alias and an unambiguous reference stay silent.
#[test]
fn projection_subclause_ambiguous_reference_fires() {
    let mut conn = connection();
    let ambiguous = conn
        .query(
            "EXPLAIN UNWIND [{a: {p: 1}, b: {p: 1}}] AS _row \
             WITH _row.a AS a, _row.b AS b \
             RETURN a.p AS b, b AS a, sum(a.p) AS s ORDER BY a.p, a",
        )
        .unwrap();
    assert_eq!(
        count_of(&ambiguous, NotificationKind::AmbiguousSubclauseReference),
        1
    );
    let silent = conn
        .query(
            "EXPLAIN UNWIND [{a: {p: 1}, b: {p: 1}}] AS _row \
             WITH _row.a AS a, _row.b AS b \
             RETURN a.p, b AS x, sum(a.p) AS s ORDER BY a.p, s, x",
        )
        .unwrap();
    assert_eq!(
        count_of(&silent, NotificationKind::AmbiguousSubclauseReference),
        0
    );
}

/// LABS-711: a subclause expression containing a complex projection item
/// as a proper sub-expression is the deprecated complex reference; the
/// exact expression itself is the recognized sort key.
#[test]
fn projection_subclause_complex_reference_fires_only_for_a_proper_subexpression() {
    let mut conn = connection();
    let nested = conn
        .query(
            "EXPLAIN UNWIND [{a: {p: 1}, b: {p: 1}}] AS _row \
             WITH _row.a AS a, _row.b AS b \
             RETURN a.p + 0 AS x, sum(a.p) AS s ORDER BY (a.p + 0) + 1",
        )
        .unwrap();
    assert_eq!(
        count_of(&nested, NotificationKind::ComplexSubclauseReference),
        1
    );
    let exact = conn
        .query(
            "EXPLAIN UNWIND [{a: {p: 1}, b: {p: 1}}] AS _row \
             WITH _row.a AS a, _row.b AS b \
             RETURN a.p + 0 AS x, sum(a.p) AS s ORDER BY a.p + 0",
        )
        .unwrap();
    assert_eq!(
        count_of(&exact, NotificationKind::ComplexSubclauseReference),
        0
    );
}

/// LABS-711: one `CREATE` self-reference notification per referencing
/// property definition. The top-level shape is the 42I58 error under
/// every spelling; the notification stays reachable through the bodies
/// the conservative validation tier deliberately under-approximates
/// (ADR-0513 §Consequences: a nested *bare* declaration read by its own
/// clause), so the per-occurrence pin runs there.
#[test]
fn create_self_reference_fires_per_reference_occurrence() {
    let mut conn = connection();
    let explained = conn
        .query(
            "EXPLAIN CALL () { CREATE (n1), \
             (n1)-[r1:R {np1: n1.p1}]->(n2 {np1: n1.np1}) RETURN n2 } RETURN 1 AS n",
        )
        .unwrap();
    assert_eq!(
        count_of(&explained, NotificationKind::CreateSelfReference),
        2
    );
}

/// LABS-711: a multi-pattern `MATCH` nested in an expression subquery has
/// no operator to carry the cartesian warning, so the statement detector
/// raises it.
#[test]
fn nested_match_cartesian_product_fires() {
    let mut conn = connection();
    let explained = conn
        .query("EXPLAIN RETURN COUNT { (n), (x)-[r]->(y) } AS c")
        .unwrap();
    assert_eq!(count_of(&explained, NotificationKind::CartesianProduct), 1);
}

/// LABS-711: a relationship variable repeated within one `MATCH` yields
/// no results and raises `01N63`.
#[test]
fn repeated_relationship_variable_fires() {
    let mut conn = connection();
    let explained = conn
        .query("EXPLAIN MATCH (a)-[q]-(b)-[q]-(c) RETURN q")
        .unwrap();
    assert_eq!(
        count_of(&explained, NotificationKind::RepeatedRelationshipVariable),
        1
    );
}

/// LABS-711: a type expression two distinct types must both satisfy is
/// impossible for a single-typed relationship and raises `01N61`.
#[test]
fn unsatisfiable_relationship_type_expression_fires() {
    let mut conn = connection();
    let explained = conn
        .query("EXPLAIN MATCH (a)-[:A&B]->(b) RETURN a")
        .unwrap();
    assert_eq!(
        count_of(
            &explained,
            NotificationKind::UnsatisfiableRelationshipTypeExpression
        ),
        1
    );
    let satisfiable = conn
        .query("EXPLAIN MATCH (a)-[:A|B]->(b) RETURN a")
        .unwrap();
    assert_eq!(
        count_of(
            &satisfiable,
            NotificationKind::UnsatisfiableRelationshipTypeExpression
        ),
        0
    );
}

/// LABS-711: an inner variable reusing an outer name without importing it
/// shadows the outer variable and raises `03N60`.
#[test]
fn subquery_variable_shadowing_fires() {
    let mut conn = connection();
    let explained = conn
        .query("EXPLAIN MATCH (x) CALL () { MATCH (x) RETURN x AS y } RETURN count(*)")
        .unwrap();
    assert_eq!(
        count_of(&explained, NotificationKind::SubqueryVariableShadowing),
        1
    );
    let imported = conn
        .query("EXPLAIN MATCH (x) CALL (x) { RETURN x AS y } RETURN count(*)")
        .unwrap();
    assert_eq!(
        count_of(&imported, NotificationKind::SubqueryVariableShadowing),
        0
    );
}

/// A `CALL { … }` inside an `EXISTS`/`COUNT`/`COLLECT` expression body is
/// checked against that body's own scope. The expression body opens its
/// scope with an empty frame, so the CALL shadows only names the body binds
/// before it — a name bound outside the expression subquery is not its
/// `outer` (live Neo4j 2026.10.1; the acceptance suites for `COLLECT`/
/// `COUNT` with `CALL` assert `03N60` here).
#[test]
fn subquery_variable_shadowing_fires_inside_expression_subqueries() {
    let mut conn = connection();
    let collect = conn
        .query(
            "EXPLAIN MATCH (person:Person) \
             WITH COLLECT { \
               MATCH (person)-[:FRIEND]->(friend:Person) \
               CALL (person) { \
                 MATCH (person)-[:FRIEND]->(friend:Person) \
                 RETURN friend.name AS name \
               } \
               RETURN DISTINCT friend.name AS friendNames \
             } AS foo RETURN foo",
        )
        .unwrap();
    assert_eq!(
        count_of(&collect, NotificationKind::SubqueryVariableShadowing),
        1
    );

    let count = conn
        .query(
            "EXPLAIN MATCH (person:Person) \
             WITH COUNT { \
               MATCH (person)-[:FOLLOWS]->(friend:Person) \
               CALL (person) { \
                 MATCH (person)-[:FOLLOWS]->(friend:Person) \
                 RETURN friend.name AS name \
               } \
               RETURN friend AS notHumans \
             } AS foo RETURN foo",
        )
        .unwrap();
    assert_eq!(
        count_of(&count, NotificationKind::SubqueryVariableShadowing),
        1
    );

    let outside_only = conn
        .query(
            "EXPLAIN MATCH (x:X) RETURN COLLECT { \
               CALL () { MATCH (x)-[]->(y) RETURN y AS z } RETURN z } AS c",
        )
        .unwrap();
    assert_eq!(
        count_of(&outside_only, NotificationKind::SubqueryVariableShadowing),
        0
    );
}

/// LABS-711: a `SEARCH` index identifier sharing a name with a variable in
/// scope is taken literally and raises `03N63`.
#[test]
fn search_identifier_shadowing_variable_fires() {
    let mut conn = connection();
    let explained = conn
        .query(
            "EXPLAIN WITH 'x' AS moviePlots MATCH (m:Movie) MATCH (movie: Movie) \
             SEARCH movie IN (VECTOR INDEX moviePlots FOR m.embedding LIMIT 4) \
             RETURN movie.title",
        )
        .unwrap();
    assert_eq!(
        count_of(
            &explained,
            NotificationKind::SearchIdentifierShadowingVariable
        ),
        1
    );
}

/// LABS-711: `shortestPath` in expression position has no operator to
/// carry `03N91`; a predicate over the relationship variable or the path
/// length forces exhaustive evaluation and raises `03N92`.
#[test]
fn shortest_path_unbounded_expression_and_exhaustive_predicate_fire() {
    let mut conn = connection();
    let expression = conn
        .query("EXPLAIN MATCH (src:A), (dst:D) RETURN shortestPath((src:A)-[*]->(dst:D)) AS p")
        .unwrap();
    assert_eq!(
        count_of(
            &expression,
            NotificationKind::UnboundedVariableLengthPattern
        ),
        1
    );
    let exhaustive = conn
        .query(
            "EXPLAIN MATCH p = shortestPath((src:A)-[rs*]->(dst:D)) \
             WHERE length(p) % 2 = 1 RETURN p",
        )
        .unwrap();
    assert_eq!(
        count_of(&exhaustive, NotificationKind::ExhaustiveShortestPath),
        1
    );
    let pushable = conn
        .query(
            "EXPLAIN MATCH p = shortestPath((src:A)-[*]->(dst:D)) \
             WHERE NONE(n IN nodes(p) WHERE n:X) RETURN p",
        )
        .unwrap();
    assert_eq!(
        count_of(&pushable, NotificationKind::ExhaustiveShortestPath),
        0
    );
}

/// LABS-711: occurrence-scoped notifications keep one entry per
/// occurrence — two scope-less subqueries and two deprecated calls.
#[test]
fn occurrence_scoped_notifications_are_not_deduplicated() {
    let mut conn = connection();
    let calls = conn
        .query(
            "EXPLAIN WITH 1 AS x CALL { WITH x CALL { WITH x RETURN x AS y } RETURN y } RETURN y",
        )
        .unwrap();
    assert_eq!(
        count_of(&calls, NotificationKind::CallSubqueryWithoutScope),
        2
    );
    let functions = conn
        .query("EXPLAIN MATCH (n) RETURN id(n) AS a, id(n) AS b")
        .unwrap();
    assert_eq!(
        count_of(&functions, NotificationKind::DeprecatedFunction),
        2
    );
}

/// LABS-711: the fulltext query procedures are deprecated in favour of
/// `SEARCH`, exactly like the vector ones.
#[test]
fn fulltext_procedures_are_deprecated() {
    let mut conn = connection();
    let explained = conn
        .query("EXPLAIN CALL db.index.fulltext.queryNodes('idx', 'q') YIELD node RETURN node")
        .unwrap();
    assert_eq!(
        count_of(&explained, NotificationKind::DeprecatedProcedure),
        1
    );
}
