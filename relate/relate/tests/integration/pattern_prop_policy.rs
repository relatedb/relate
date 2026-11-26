//! One pattern-property evaluator, one error policy (LABS-668): the same
//! malformed property map must answer identically on every engine that
//! evaluates a pattern — the plain MATCH (the candidates funnel / the GPM
//! walk), the eval walker (pattern predicates, comprehensions, the
//! expression-form shortest paths), and the one-hop folds. A non-storable
//! property-map value is a loud `InvalidPropertyType` on every one of
//! them, never a silent zero-row answer.

use crate::common;

use common::{connection, rows, scalar_one};
use relate::Value;

/// The one error-expectation helper every differential assertion in this
/// file spells its expectation through (F-1-4): `query` on a seeded
/// two-node, one-rel store must fail with an error containing `marker`
/// — never a zero-row success.
fn assert_query_error(query: &str, marker: &str) {
    let mut connection = connection();
    connection
        .query("CREATE (:N {p: 1})-[:R {q: 2}]->(:M {p: 3})")
        .unwrap();
    let error = connection
        .query(query)
        .err()
        .unwrap_or_else(|| panic!("{query}: expected an error containing {marker:?}, got rows"));
    assert!(
        error.to_string().contains(marker),
        "{query}: expected an error containing {marker:?}, got {error}"
    );
}

/// The rel-pattern property map: the plain MATCH's one-hop admission
/// (`rel_edge_ok` behind the expand path), the EXISTS pattern predicate,
/// the pattern comprehension, and the GPM walk's var-length admission all
/// raise the same typed error for a non-storable value (a nested list;
/// the `sanitize_props` verdict walk, ADR-0327). The first three
/// silently answered zero rows before the unification; the GPM walk was
/// already loud and pins the policy.
#[test]
fn rel_props_error_identically_on_every_engine() {
    // The plain MATCH — `rel_edge_ok` behind the expand path.
    assert_query_error(
        "MATCH (a)-[r {q: [[1]]}]->(b) RETURN count(*) AS c",
        "InvalidPropertyType",
    );
    // The eval walker — the EXISTS pattern predicate.
    assert_query_error(
        "RETURN EXISTS { (a)-[r {q: [[1]]}]->(b) }",
        "InvalidPropertyType",
    );
    // The pattern comprehension — the eval walker's `rel_edge_ok`.
    assert_query_error(
        "MATCH (a) RETURN [(a)-[r {q: [[1]]}]->(b) | b.p] AS xs",
        "InvalidPropertyType",
    );
    // The GPM walk's var-length rel admission — the loud spelling the
    // unification generalizes.
    assert_query_error(
        "MATCH p = (a)-[r*1..2 {q: [[1]]}]->(b) RETURN count(*) AS c",
        "InvalidPropertyType",
    );
}

/// The node-pattern property map: the candidates funnel (already loud),
/// the far-seat EXISTS admission, and the expression-form shortest path's
/// start admission all raise the same typed error. The shortest path
/// silently answered null before the unification — the walker's
/// `pattern_node_ok` swallowed the evaluator's error.
#[test]
fn node_props_error_identically_on_every_engine() {
    // The plain MATCH — the candidates funnel's eager property-map seek.
    assert_query_error("MATCH (n {p: [[1]]}) RETURN n", "InvalidPropertyType");
    // The eval walker's far-seat admission through the funnel.
    assert_query_error(
        "RETURN EXISTS { (a)-[r:R]->(b {p: [[1]]}) }",
        "InvalidPropertyType",
    );
    // The expression-form shortest path — `eval/shortest.rs`'s start
    // admission through `pattern_node_ok`.
    assert_query_error(
        "MATCH (a), (b) RETURN shortestPath((a {p: [[1]]})-[*]->(b)) AS p",
        "InvalidPropertyType",
    );
}

/// One error policy for the label half of the same admission: a
/// mis-evaluating dynamic label expression raises the same typed error
/// on the plain MATCH, the EXISTS predicate, and the pattern
/// comprehension's far-seat admission (pre-fix: the comprehension
/// silently answered empty lists — the walker's `pattern_node_ok`
/// swallowed the label error its rel-side twin propagates).
#[test]
fn dynamic_labels_error_identically_on_every_engine() {
    for query in [
        "WITH 5 AS x MATCH (n:$(x)) RETURN count(*) AS c",
        "WITH 5 AS x RETURN EXISTS { (n:$(x)) }",
        "WITH 5 AS x MATCH (a) RETURN [(a)-[:R]->(b:$(x)) | b.p] AS xs",
    ] {
        assert_query_error(query, "dynamic label");
    }
}

/// The loud policy is not a blanket change: a well-formed property map
/// still matches (and still fails to match) exactly as before on both
/// engines — the unification changed only the malformed-map answer.
#[test]
fn well_formed_props_still_match_and_miss_on_both_engines() {
    let mut connection = connection();
    connection
        .query("CREATE (:N {p: 1})-[:R {q: 2}]->(:N {p: 3})")
        .unwrap();

    // Matching and non-matching property maps on the plain MATCH.
    assert_eq!(
        connection
            .query("MATCH (n:N {p: 1}) RETURN count(n) AS c")
            .unwrap()
            .rows,
        vec![vec![relate::Value::Int(1)]]
    );
    assert_eq!(
        connection
            .query("MATCH (n:N {p: 9}) RETURN count(n) AS c")
            .unwrap()
            .rows,
        vec![vec![relate::Value::Int(0)]]
    );
    assert_eq!(
        connection
            .query("MATCH (a)-[r:R {q: 2}]->(b) RETURN count(*) AS c")
            .unwrap()
            .rows,
        vec![vec![relate::Value::Int(1)]]
    );
    assert_eq!(
        connection
            .query("MATCH (a)-[r:R {q: 9}]->(b) RETURN count(*) AS c")
            .unwrap()
            .rows,
        vec![vec![relate::Value::Int(0)]]
    );
    // The same shapes through the eval walker: the pattern predicate and
    // the comprehension answer true/one-row and false/empty respectively.
    assert_eq!(
        connection
            .query("RETURN EXISTS { (m:N {p: 1}) } AS e")
            .unwrap()
            .rows,
        vec![vec![relate::Value::Bool(true)]]
    );
    assert_eq!(
        connection
            .query("RETURN EXISTS { (m:N {p: 9}) } AS e")
            .unwrap()
            .rows,
        vec![vec![relate::Value::Bool(false)]]
    );
    assert_eq!(
        connection
            .query("MATCH (a:N {p: 1}) RETURN [(a)-[r:R {q: 2}]->(b) | b.p] AS xs")
            .unwrap()
            .rows,
        vec![vec![relate::Value::List(
            vec![relate::Value::Int(3)].into()
        )]]
    );
    assert_eq!(
        connection
            .query("MATCH (a:N {p: 1}) RETURN [(a)-[r:R {q: 9}]->(b) | b.p] AS xs")
            .unwrap()
            .rows,
        vec![vec![relate::Value::List(vec![].into())]]
    );
}

// --- LABS-1086: a null entry in a *match* map ------------------------------
//
// A null value in a pattern's match property map makes the pattern
// unsatisfiable — `{k: null}` is the equality `n.k = null`, never true in
// Cypher's three-valued logic, so it matches no element (Neo4j returns zero
// rows). The engine used to drop the null entry the way CREATE
// materialization must (a null is not stored), silently widening
// `{k: null}` into "any value of k": an agent's keyed
// `MATCH (x:Model {id: <missing field>})` matched every Model and created
// 330 edges where 11 were correct. Every pattern reader goes through the one
// evaluator, so each test pins one reader; the non-null controls pin that
// keyed matching is untouched, and CREATE keeps dropping the null entry
// (materialization is the other role).

fn models(c: &mut relate::Connection) {
    c.query("CREATE (:Model {id: 'a'})-[:R {w: 1}]->(:Model {id: 'b'})-[:R]->(:Model)")
        .unwrap();
}

#[test]
fn a_literal_null_in_a_node_map_matches_nothing() {
    let mut c = connection();
    models(&mut c);
    assert_eq!(
        scalar_one(&mut c, "MATCH (x:Model {id: null}) RETURN count(x)"),
        Value::Int(0)
    );
    // Keyed control: a non-null map still matches its node.
    assert_eq!(
        scalar_one(&mut c, "MATCH (x:Model {id: 'a'}) RETURN count(x)"),
        Value::Int(1)
    );
}

#[test]
fn a_where_equality_with_null_matches_nothing() {
    // The planner pushes `x.id = null` into the pattern's map; the
    // pushed form must keep the three-valued meaning of `=`.
    let mut c = connection();
    models(&mut c);
    assert_eq!(
        scalar_one(&mut c, "MATCH (x:Model) WHERE x.id = null RETURN count(x)"),
        Value::Int(0)
    );
    assert_eq!(
        scalar_one(&mut c, "MATCH (x) WHERE x.id = null RETURN count(x)"),
        Value::Int(0)
    );
}

#[test]
fn a_where_equality_on_a_key_the_map_already_holds_is_kept() {
    // The pushdown folds `x.k = v` into the pattern's map; when the map
    // already carries `k`, the conjunct must stay in the WHERE — both a
    // null and a conflicting value — not be silently dropped.
    let mut c = connection();
    models(&mut c);
    assert_eq!(
        scalar_one(
            &mut c,
            "MATCH (x:Model {id: 'a'}) WHERE x.id = null RETURN count(x)"
        ),
        Value::Int(0)
    );
    assert_eq!(
        scalar_one(
            &mut c,
            "MATCH (x:Model {id: 'a'}) WHERE x.id = 'b' RETURN count(x)"
        ),
        Value::Int(0)
    );
    // Agreeing values still match.
    assert_eq!(
        scalar_one(
            &mut c,
            "MATCH (x:Model {id: 'a'}) WHERE x.id = 'a' RETURN count(x)"
        ),
        Value::Int(1)
    );
}

#[test]
fn a_bound_null_and_a_mixed_map_match_nothing() {
    let mut c = connection();
    models(&mut c);
    assert_eq!(
        scalar_one(
            &mut c,
            "WITH null AS v MATCH (x:Model {id: v}) RETURN count(x)"
        ),
        Value::Int(0)
    );
    // One null entry beside a matching one still empties the pattern.
    assert_eq!(
        scalar_one(
            &mut c,
            "MATCH (x:Model {id: 'a', name: null}) RETURN count(x)"
        ),
        Value::Int(0)
    );
}

#[test]
fn optional_match_with_a_null_map_binds_null() {
    let mut c = connection();
    models(&mut c);
    // OPTIONAL MATCH keeps its one row with `x` bound to null — not an
    // empty result, and not every Model.
    assert_eq!(
        rows(
            &mut c,
            "OPTIONAL MATCH (x:Model {id: null}) RETURN x IS NULL AS absent"
        ),
        vec![vec![Value::Bool(true)]]
    );
}

#[test]
fn pattern_predicates_and_comprehensions_honour_the_null_entry() {
    let mut c = connection();
    models(&mut c);
    assert_eq!(
        scalar_one(
            &mut c,
            "MATCH (x:Model) WHERE EXISTS { (x)-[:R {w: null}]->() } RETURN count(x)"
        ),
        Value::Int(0)
    );
    assert_eq!(
        scalar_one(
            &mut c,
            "MATCH (x:Model) RETURN sum(size([(x)-->(y:Model {id: null}) | y]))"
        ),
        Value::Int(0)
    );
}

#[test]
fn relationship_maps_and_shortest_paths_honour_the_null_entry() {
    let mut c = connection();
    models(&mut c);
    assert_eq!(
        scalar_one(&mut c, "MATCH (:Model)-[r:R {w: null}]->() RETURN count(r)"),
        Value::Int(0)
    );
    assert_eq!(
        scalar_one(
            &mut c,
            "MATCH (:Model)-[:R*1..2 {w: null}]->() RETURN count(*)"
        ),
        Value::Int(0)
    );
    assert_eq!(
        scalar_one(
            &mut c,
            "MATCH p = shortestPath((:Model {id: 'a'})-[*]-(:Model {id: null})) \
             RETURN count(p)"
        ),
        Value::Int(0)
    );
    // Relationship control: a non-null rel map still matches.
    assert_eq!(
        scalar_one(&mut c, "MATCH (:Model)-[r:R {w: 1}]->() RETURN count(r)"),
        Value::Int(1)
    );
}

#[test]
fn create_still_drops_a_null_entry() {
    // Materialization is the other role: a null is not stored, so the
    // created node simply has no `name` key — and is then found by its
    // non-null key alone.
    let mut c = connection();
    c.query("CREATE (:Model {id: 'n', name: null})").unwrap();
    assert_eq!(
        scalar_one(
            &mut c,
            "MATCH (x:Model {id: 'n'}) WHERE x.name IS NULL RETURN count(x)"
        ),
        Value::Int(1)
    );
}
