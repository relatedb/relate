//! REL8-2028: existence-subquery short bodies — the hand-written walks
//! that decide which names a query reads, and which stages an `EXISTS { }`
//! body runs, missing a construct. Corpus issues 37 (a QPP element's
//! property map / label expression, and a procedure's `YIELD … WHERE`),
//! 59 (a comprehension/quantifier/`reduce` binder read by a subquery
//! body), 67's T5 (`CALL (n) { }` as a body's only mention of an outer
//! name) and 80 (`EXISTS { … NEXT … }` answered from its first stage).
//! Every expectation is what live Neo4j 2026.10 answers (the corpus
//! evidence folders). Issue 81 does not reproduce on this tree (its three
//! walks are exhaustive now) and has no test here.
//!
//! Each test fails on the unpatched tree: the trim drops a name only the
//! missed construct reads (`UndefinedVariable`), or `EXISTS` answers
//! `true` from a stage whose `NEXT` successor yields nothing.

mod common;

use common::{
    assert_ce_rejection, connection, first_column, for_each_engine, list, rows, run, scalar_one,
};
use relate::Value;

/// Issue 37: a name an outer row binds, read only by a quantified group's
/// element property map or dynamic label, is trimmed away by the MATCH
/// stage's row trim and the scan then evaluates the map against a row
/// without it. Same answer under both forced pattern engines.
#[test]
fn qpp_element_maps_keep_an_outer_name_live() {
    for_each_engine(|engine, c| {
        run(
            c,
            "CREATE (:A {id: 1, p: 1, lab: 'A'})-[:R {w: 1}]->(:A {id: 2, p: 1})-\
             [:R {w: 1}]->(:A {id: 3, p: 2})",
        );
        for (query, expected) in [
            // The map on the leading node, the trailing node and inside the
            // quantified group — the three positions the corpus reports.
            (
                "MATCH (s:A {id: 1}) WITH s MATCH (n0 {p: s.p})((x)-[:R]->(y)){1,2}(n1) \
                 RETURN count(*) AS c",
                3,
            ),
            (
                "MATCH (s:A {id: 1}) WITH s MATCH (n0)((x)-[:R]->(y)){1,2}(n1 {p: s.p}) \
                 RETURN count(*) AS c",
                1,
            ),
            (
                "MATCH (s:A {id: 1}) WITH s MATCH (n0)((x {p: s.p})-[:R]->(y)){1,2}(n1) \
                 RETURN count(*) AS c",
                3,
            ),
            // A relationship map inside the group.
            (
                "MATCH (s:A {id: 1}) WITH s MATCH (n0)((x)-[r:R {w: s.p}]->(y)){1,2}(n1) \
                 RETURN count(*) AS c",
                3,
            ),
            // A dynamic label on an element inside the group (Cypher 25).
            (
                "CYPHER 25 MATCH (s:A {id: 1}) WITH s MATCH (n0:$(s.lab))((x)-[:R]->(y)){1,2}(n1) \
                 RETURN count(*) AS c",
                3,
            ),
        ] {
            assert_eq!(
                scalar_one(c, query),
                Value::Int(expected),
                "[{engine}] {query}"
            );
        }
        // The write behind the shape runs, and only over the matching rows.
        assert_eq!(
            scalar_one(
                c,
                "MATCH (s:A {id: 1}) WITH s MATCH (n0 {p: s.p})((x)-[:R]->(y)){1,2}(n1) \
                 SET n1.hit = true RETURN count(*) AS c",
            ),
            Value::Int(3),
            "[{engine}] the write behind a QPP element map"
        );
        // Three rows, two distinct ending nodes (ids 2 and 3).
        assert_eq!(
            scalar_one(c, "MATCH (n:A) WHERE n.hit RETURN count(*) AS c"),
            Value::Int(2),
            "[{engine}] the write behind a QPP element map did not land"
        );
    });
}

/// Issue 37, second omission: a procedure's `YIELD … WHERE` reads the
/// enclosing row, and the trim ignored the call's `where_clause`.
#[test]
fn yield_where_keeps_an_outer_name_live() {
    let mut c = connection();
    run(
        &mut c,
        "CREATE (:A {id: 1, p: 1, lab: 'A'})-[:R]->(:A {id: 2, p: 1})-[:R]->(:A {id: 3, p: 2})",
    );
    for (query, expected) in [
        (
            "MATCH (s:A {id: 1}) CALL db.labels() YIELD label WHERE label = s.lab \
             RETURN count(*) AS c",
            1,
        ),
        // With a second MATCH stage in between: the trim happens there.
        (
            "MATCH (s:A {id: 1}) WITH s MATCH (z:A {id: 3}) CALL db.labels() YIELD label \
             WHERE label = s.lab RETURN count(*) AS c",
            1,
        ),
        (
            "MATCH (s:A {id: 1}) WITH s MATCH (z:A {id: 3}) CALL db.propertyKeys() \
             YIELD propertyKey WHERE propertyKey STARTS WITH s.lab RETURN count(*) AS c",
            0,
        ),
    ] {
        assert_eq!(scalar_one(&mut c, query), Value::Int(expected), "{query}");
    }
}

/// Issue 59: a list comprehension's, quantifier's or `reduce`'s binder is
/// in scope in its `WHERE`/projection, a subquery body there included.
/// The front's subquery-body walk validated every body against the clause
/// scope, which does not carry the binder — a `MATCH … WHERE` in the body
/// then rejected a name visibly in scope.
#[test]
fn comprehension_binders_are_in_scope_inside_a_subquery_body() {
    let mut c = connection();
    run(
        &mut c,
        "CREATE (a:A {id: 1})-[:R]->(b:B {id: 2})-[:R]->(c:C {id: 3})",
    );
    assert_eq!(
        first_column(&run(
            &mut c,
            "RETURN [x IN [1, 2] | COUNT { MATCH (m) WHERE m.id = x }] AS l",
        )),
        vec![list(vec![Value::Int(1), Value::Int(1)])],
        "the comprehension's element in a COUNT body"
    );
    assert_eq!(
        first_column(&run(
            &mut c,
            "RETURN [x IN [1, 2] | EXISTS { MATCH (m) WHERE m.id = x }] AS l",
        )),
        vec![list(vec![Value::Bool(true), Value::Bool(true)])],
        "the comprehension's element in an EXISTS body"
    );
    assert_eq!(
        scalar_one(
            &mut c,
            "RETURN any(x IN [1, 2] WHERE EXISTS { MATCH (m) WHERE m.id = x + 2 }) AS a",
        ),
        Value::Bool(true),
        "a quantifier's variable"
    );
    // `reduce`'s element and its accumulator are both in scope.
    assert_eq!(
        scalar_one(
            &mut c,
            "RETURN reduce(s = 0, y IN [1, 2] | s + COUNT { MATCH (m) WHERE m.id > y }) AS r",
        ),
        Value::Int(3),
        "reduce's element"
    );
    assert_eq!(
        scalar_one(
            &mut c,
            "RETURN reduce(s = 0, y IN [1, 2] | s + COUNT { MATCH (m) WHERE m.id > s }) AS r",
        ),
        Value::Int(3),
        "reduce's accumulator"
    );
    // Correlated with an outer node as well.
    assert_eq!(
        first_column(&run(
            &mut c,
            "MATCH (n:A) RETURN [x IN [1, 2] | COUNT { MATCH (n)-->(m) WHERE m.id > x }] AS l",
        )),
        vec![list(vec![Value::Int(1), Value::Int(0)])],
        "a body reading both a binder and an outer node"
    );
    // The write form runs.
    assert_eq!(
        first_column(&run(
            &mut c,
            "MATCH (n:A) SET n.l = [x IN [1, 2] | COUNT { MATCH (m) WHERE m.id = x }] RETURN n.l AS l",
        )),
        vec![list(vec![Value::Int(1), Value::Int(1)])],
        "a comprehension inside a SET value"
    );
    // A binder shadows a same-named outer variable: the inner `x` is used.
    assert_eq!(
        first_column(&run(
            &mut c,
            "WITH 9 AS x RETURN [x IN [1, 2] | COUNT { MATCH (m) WHERE m.id = x }] AS l",
        )),
        vec![list(vec![Value::Int(1), Value::Int(1)])],
        "a same-named outer variable does not leak into the body"
    );
}

/// Issue 80: `EXISTS { … NEXT … }` is decided on the whole stage chain —
/// a later stage with no row makes the test false. Before the fix the
/// body's first stage decided, so `NOT EXISTS` deleted nothing.
#[test]
fn exists_runs_every_next_stage() {
    let mut c = connection();
    run(&mut c, "CREATE (:B {id: 2}), (:B {id: 3})");
    assert_eq!(
        scalar_one(
            &mut c,
            "CYPHER 25 RETURN EXISTS { RETURN 1 AS k NEXT MATCH (y:B) WHERE y.id = 99 \
             RETURN y } AS e",
        ),
        Value::Bool(false),
        "the NEXT stage yields no row, so the body does not exist"
    );
    // The COUNT spelling of the same body is the control.
    assert_eq!(
        scalar_one(
            &mut c,
            "CYPHER 25 RETURN COUNT { RETURN 1 AS k NEXT MATCH (y:B) WHERE y.id = 99 \
             RETURN y } AS c",
        ),
        Value::Int(0)
    );
    // An omitted-RETURN body counts the FINAL stage's rows.
    assert_eq!(
        scalar_one(
            &mut c,
            "CYPHER 25 RETURN COUNT { MATCH (y:B) NEXT MATCH (z:B) WHERE z.id = 99 } AS c",
        ),
        Value::Int(0),
        "an omitted-RETURN COUNT counts the last NEXT stage, not the first"
    );
    assert_eq!(
        scalar_one(
            &mut c,
            "CYPHER 25 RETURN COUNT { MATCH (y:B) NEXT MATCH (z:B) WHERE z.id = 2 } AS c",
        ),
        Value::Int(2),
        "the positive control: every first-stage row survives the second stage"
    );
    assert!(
        rows(
            &mut c,
            "CYPHER 25 MATCH (x:B) WHERE EXISTS { MATCH (y:B) WHERE y.id = 2 RETURN y \
             NEXT MATCH (y) WHERE y.id = 99 RETURN y } RETURN x.id AS xi",
        )
        .is_empty(),
        "the second stage rejects y, so no x exists"
    );
    // A later stage reads the outer `o`: for o = 2 the stage's `MATCH (y)`
    // re-uses the bound y (id 2); for o = 3 it rejects it.
    assert_eq!(
        first_column(&run(
            &mut c,
            "CYPHER 25 MATCH (o:B) WHERE EXISTS { MATCH (y:B) WHERE y.id = 2 RETURN y \
             NEXT MATCH (y) WHERE y.id = o.id RETURN y.id AS v } RETURN o.id AS oi ORDER BY oi",
        )),
        vec![Value::Int(2)],
        "EXISTS over the NEXT chain"
    );
    assert_eq!(
        first_column(&run(
            &mut c,
            "CYPHER 25 MATCH (o:B) WHERE NOT EXISTS { MATCH (y:B) WHERE y.id = 2 RETURN y \
             NEXT MATCH (y) WHERE y.id = o.id RETURN y.id AS v } RETURN o.id AS oi ORDER BY oi",
        )),
        vec![Value::Int(3)],
        "NOT EXISTS over the NEXT chain"
    );
    // The guarded write acts on the right rows.
    run(
        &mut c,
        "CYPHER 25 MATCH (o:B) WHERE NOT EXISTS { MATCH (y:B) WHERE y.id = 2 RETURN y \
         NEXT MATCH (y) WHERE y.id = o.id RETURN y.id AS v } DETACH DELETE o",
    );
    assert_eq!(
        first_column(&run(&mut c, "MATCH (b:B) RETURN b.id AS id ORDER BY id")),
        vec![Value::Int(2)],
        "the write behind NOT EXISTS deleted the wrong rows"
    );
}

/// Issue 67 (T5): a subquery body that reaches an outer name only through
/// its `CALL (n) { }` import list — nothing inside the body mentions it —
/// must keep that name live. The body here is a `COUNT` inside a
/// procedure's `YIELD … WHERE`.
#[test]
fn a_subquery_body_importing_via_call_keeps_the_name_live() {
    let mut c = connection();
    run(&mut c, "CREATE (:A {name: 'a'})-[:R]->(:B {name: 'b'})");
    assert_eq!(
        scalar_one(
            &mut c,
            "MATCH (n:A) CALL db.labels() YIELD label \
             WHERE COUNT { CALL (n) { RETURN 1 AS q } RETURN q } > 0 RETURN count(*) AS c",
        ),
        Value::Int(2),
        "the CALL import list is the body's only mention of `n`"
    );
}

/// Issue 59 controls: a binder read by a body `RETURN` item, a body
/// `WITH … WHERE`, or a property map was accepted before the fix and must
/// stay accepted.
#[test]
fn comprehension_binder_controls_stay_accepted() {
    let mut c = connection();
    run(
        &mut c,
        "CREATE (a:A {id: 1})-[:R]->(b:B {id: 2})-[:R]->(c:C {id: 3})",
    );
    assert_eq!(
        first_column(&run(
            &mut c,
            "RETURN [x IN [1, 2] | COUNT { MATCH (m {id: x}) }] AS l",
        )),
        vec![list(vec![Value::Int(1), Value::Int(1)])],
        "the binder in a property map"
    );
    assert_eq!(
        first_column(&run(
            &mut c,
            "RETURN [x IN [1, 2] | COLLECT { MATCH (m) WITH m WHERE m.id = x + 1 \
             RETURN m.id }] AS l",
        )),
        vec![list(vec![
            list(vec![Value::Int(2)]),
            list(vec![Value::Int(3)]),
        ])],
        "the binder in a body `WITH … WHERE`"
    );
}

/// A write in a `NEXT` stage of an expression-subquery body is the same
/// composition violation as one in the body's first stage: the body is the
/// whole statement chain. The planner's gate now walks the chain.
#[test]
fn a_write_in_a_next_stage_is_rejected() {
    let mut c = connection();
    for query in [
        "CYPHER 25 RETURN COUNT { MATCH (n) NEXT CREATE (:X) } AS c",
        "CYPHER 25 RETURN COLLECT { MATCH (n) NEXT CREATE (:X) RETURN 1 } AS l",
        "CYPHER 25 RETURN EXISTS { MATCH (n) NEXT CREATE (:X) } AS e",
    ] {
        assert_ce_rejection(
            &mut c,
            query,
            "InvalidClauseComposition: writes are not allowed in subquery expressions",
            "SyntaxError",
        );
    }
    assert_eq!(
        scalar_one(&mut c, "MATCH (x:X) RETURN count(*) AS n"),
        Value::Int(0),
        "a refused body must write nothing"
    );
}

/// An import read in a `NEXT` stage of an `EXISTS` body survives the
/// first stage's conditional projection: the branch body's terminal
/// `RETURN` exports the import into the next stage's seed. Before the
/// fix the import was dropped and the second stage either errored
/// (`UndefinedVariable`) or — under the pre-REL8-2028 first-stage-only
/// answer — never ran at all (main answers `true` for the filtering
/// shape).
#[test]
fn an_import_read_in_a_conditional_next_stage_is_seeded() {
    let mut c = connection();
    assert_eq!(
        scalar_one(
            &mut c,
            "CYPHER 25 LET a = 1 RETURN EXISTS { WHEN true THEN RETURN 1 AS k \
             NEXT WITH a WHERE a = 2 RETURN 1 AS x } AS x",
        ),
        Value::Bool(false),
        "the NEXT stage filters the import out, so the body has no row"
    );
    assert_eq!(
        scalar_one(
            &mut c,
            "CYPHER 25 LET a = 2 RETURN EXISTS { WHEN true THEN RETURN 1 AS k \
             NEXT WITH a WHERE a = 2 RETURN 1 AS x } AS x",
        ),
        Value::Bool(true),
        "the import is readable in the NEXT stage"
    );
    // The issue's own shape: the import is read only by the later stage's
    // projection.
    assert_eq!(
        scalar_one(
            &mut c,
            "LET a = 1 RETURN EXISTS { WHEN true THEN LET b = 2 RETURN b \
             NEXT RETURN a + b AS x } AS x",
        ),
        Value::Bool(true)
    );
}
