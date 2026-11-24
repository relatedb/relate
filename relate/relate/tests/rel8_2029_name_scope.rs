//! REL8-2029 (the REL8-2023 campaign's name-scope family): four findings on
//! what a row a clause evaluates actually exposes. Every expectation is
//! what live Neo4j 2026.10 answers — the corpus evidence folders
//! (`relate-private/issues/{34,50,51,64}`) carry the reference outputs.
//!
//! - 34: a variable an earlier clause bound to null matches nothing in a
//!   later pattern, on every engine and every pattern spelling;
//! - 50: a naked `CALL { … }` body imports only what its leading importing
//!   `WITH` names, so a name it binds is fresh;
//! - 51: a scope clause's imports stay visible for the whole body, past a
//!   body `WITH`;
//! - 64: a `DISTINCT` / aggregating projection ends the scope of the names
//!   it does not project, so a subquery body in the following `WHERE` /
//!   `ORDER BY` binds a re-used name fresh.

mod common;

use common::{connection, first_column, for_each_engine, rows, run, scalar_one};
use relate::Value;

fn int(value: i64) -> Value {
    Value::Int(value)
}

/// Issue 34's graph: `(:A {id: 1})-[:R]->(:M {id: 2})-[:R]->(:B {id: 3})`.
fn chain(conn: &mut relate::Connection) {
    run(
        conn,
        "CREATE (:A {id: 1})-[:R]->(:M {id: 2})-[:R]->(:B {id: 3})",
    );
}

/// Issue 34: a name an earlier clause bound to null (`WITH null AS x`, a
/// failed `OPTIONAL MATCH`) is a binding that matches nothing. The GPM
/// engine used to read it as a fresh slot and rebind it to whatever node a
/// variable-length or quantified walk reached, so counts, `EXISTS`, `COUNT`
/// subqueries and writes all covered values Neo4j never matches.
#[test]
fn a_null_bound_variable_matches_nothing_in_a_later_pattern() {
    for_each_engine(|engine, conn| {
        chain(conn);
        for (query, expected) in [
            // Fixed-length (always right) and the var-length / quantified
            // spellings that rebound (wrong before the fix).
            (
                "WITH null AS x MATCH (a:A)-[:R]->()-[:R]->(x) RETURN count(*) AS n",
                0,
            ),
            (
                "WITH null AS x MATCH (a:A)-[:R*2]->(x) RETURN count(*) AS n",
                0,
            ),
            (
                "WITH null AS x MATCH (a:A)-[:R]->{2}(x) RETURN count(*) AS n",
                0,
            ),
            (
                "MATCH (a:A) OPTIONAL MATCH (a)-[:NOPE]->(x) MATCH (a)-[:R*2]->(x) \
                 RETURN count(*) AS n",
                0,
            ),
            (
                "MATCH (a:A) OPTIONAL MATCH (a)-[:NOPE]->(x) \
                 MATCH (a)-[:R*1..2]->(x)-[:R*0..1]->(y) RETURN count(*) AS n",
                0,
            ),
            // A null list element is the same binding.
            (
                "MATCH (b:B) UNWIND [null, b] AS x MATCH (a:A)-[:R*2]->(x) RETURN count(*) AS n",
                1,
            ),
            // The subquery-expression consumers.
            (
                "MATCH (n:A) OPTIONAL MATCH (n)-[:NOPE]->(m) \
                 RETURN COUNT { MATCH (n)-[*1..2]->(m) } AS n",
                0,
            ),
            // A quantified target on the ladder's specialized rungs: the
            // frontier rung runs ahead of the per-row walker, so the row
            // rule is the ladder's, not the walker's.
            (
                "CYPHER 25 MATCH (n:A) OPTIONAL MATCH (n)-[:NOPE]->(t) WITH n, t \
                 MATCH (n)-[:R]->{1,3}(t) RETURN count(t) AS n",
                0,
            ),
            (
                "CYPHER 25 MATCH (n:A) OPTIONAL MATCH (n)-[:NOPE]->(t) WITH n, t \
                 MATCH (n)((a)-[:R]->(b)){1,3}(t) RETURN count(DISTINCT t) AS n",
                0,
            ),
            (
                "CYPHER 25 MATCH (n:A) OPTIONAL MATCH (n)-[:NOPE]->(t) WITH n, t \
                 MATCH WALK (n)((a)-[:R]->(b)){1,3}(t) RETURN count(*) AS n",
                0,
            ),
        ] {
            assert_eq!(scalar_one(conn, query), int(expected), "{engine}: {query}");
        }
        // A `COUNT { }` body is the ladder's and answers zero. The
        // `EXISTS { }` body is the one residual: its existence probe walks
        // the body per pattern directly, and an enclosing quantification's
        // predicate reaches the same probe with its own element names
        // stamped null (ADR-0335 §5), so the row rule cannot tell a clause
        // binding from that placeholder there. Fixing it needs the probe's
        // row to carry the distinction; main answers the same.
        assert_eq!(
            scalar_one(
                conn,
                "MATCH (a:A) OPTIONAL MATCH (a)-[:NOPE]->(x) \
                 RETURN COUNT { MATCH (a)-[:R*2]->(x) } AS c"
            ),
            int(0),
            "{engine}: a null-bound end is no count"
        );
        // The positive controls: without the failed OPTIONAL MATCH the same
        // spellings match, and an unbound start still walks.
        assert_eq!(
            scalar_one(conn, "MATCH (a:A)-[:R*2]->(x) RETURN x.id AS id"),
            int(3),
            "{engine}: the control pattern still matches"
        );
        assert_eq!(
            scalar_one(
                conn,
                "WITH null AS x MATCH (x)<-[:R*2]-(a) RETURN count(*) AS n"
            ),
            int(0),
            "{engine}: a null start matches nothing"
        );
    });
}

/// Issue 34 under `OPTIONAL MATCH`: a null-bound name matches nothing, but
/// the optional clause keeps its row and null-extends it — the row rule is a
/// plain-MATCH rule, never a row killer for the optional form. The
/// quantified optional spellings keep their own (pre-existing) GPM
/// behaviour, which this rule does not touch.
#[test]
fn an_optional_match_over_a_null_bound_name_keeps_its_row() {
    let mut conn = connection();
    chain(&mut conn);
    for optional in ["(n)-[:R]->(t)", "(n)-[:R*1..3]->(t)", "(n)-[:R]->{1,3}(t)"] {
        let result = rows(
            &mut conn,
            &format!(
                "MATCH (n:A) OPTIONAL MATCH (n)-[:NOPE]->(t) WITH n, t \
                 OPTIONAL MATCH {optional} RETURN n.id AS id, t IS NULL AS t_null"
            ),
        );
        assert_eq!(
            result,
            vec![vec![int(1), Value::Bool(true)]],
            "{optional}: the optional row survives, null-extended"
        );
    }
}

/// Issue 34's write arm: the row a null-bound pattern would have rebound is
/// never matched, so the `SET` behind it writes nothing — and the same under
/// a `SHORTEST` spelling whose null end used to enumerate every trail
/// (the K6 case took 3.9 GB and 24 s; the row now dies before any walk).
#[test]
fn a_null_bound_pattern_writes_nothing_and_enumerates_nothing() {
    for_each_engine(|engine, conn| {
        chain(conn);
        run(
            conn,
            "MATCH (a:A) OPTIONAL MATCH (a)-[:NOPE]->(x) MATCH (a)-[:R*2]->(x) SET x.hit = true",
        );
        assert_eq!(
            scalar_one(conn, "MATCH (n) WHERE n.hit RETURN count(*) AS n"),
            int(0),
            "{engine}: the write behind the null-bound pattern"
        );
        // A complete graph K5: the null end must answer 0 without walking a
        // trail, and the unbound control must still count them.
        run(conn, "UNWIND range(1, 5) AS i CREATE (:K {id: i})");
        run(
            conn,
            "MATCH (a:K), (b:K) WHERE a.id < b.id CREATE (a)-[:S]->(b)",
        );
        assert_eq!(
            scalar_one(
                conn,
                "WITH null AS u MATCH SHORTEST 1 (a)-[:S]-+(u) RETURN count(*) AS n"
            ),
            int(0),
            "{engine}: a null shortest end matches nothing"
        );
        assert_eq!(
            scalar_one(
                conn,
                "MATCH SHORTEST 1 (a:K)-[:S]-+(u) RETURN count(*) AS n"
            ),
            int(25),
            "{engine}: the unbound shortest end still counts K5's endpoints"
        );
    });
}

/// Issue 50: a `CALL { … }` body with no scope clause imports only what its
/// leading importing `WITH` names — so a name the body itself binds is a
/// new variable even when the outer row binds one of the same name. The body
/// used to be seeded with the whole outer row, which pinned the body's `MATCH
/// (n)` to the outer `n` (counts, filters and writes covered that one node).
#[test]
fn a_naked_call_body_binds_its_own_names_fresh() {
    let mut conn = connection();
    run(
        &mut conn,
        "CREATE (a:A {id: 1})-[:R]->(b:B {id: 2})-[:R]->(c:C {id: 3})",
    );
    for (query, expected) in [
        (
            "MATCH (n:A) CALL { MATCH (n)-->(x) RETURN x.id AS xi } RETURN xi ORDER BY xi",
            vec![vec![int(2)], vec![int(3)]],
        ),
        (
            "MATCH (n:A) CALL { MATCH (n) RETURN count(n) AS total } RETURN total",
            vec![vec![int(3)]],
        ),
        (
            "MATCH (n:A) CALL { MATCH (n) WHERE n.id > 1 RETURN n.id AS xi } RETURN xi ORDER BY xi",
            vec![vec![int(2)], vec![int(3)]],
        ),
        // The body's own binding exported under another name.
        (
            "MATCH (n:A) CALL { MATCH (n)-->(x) RETURN n AS q, x.id AS xi } \
             RETURN q.id AS qi, xi ORDER BY qi, xi",
            vec![vec![int(1), int(2)], vec![int(2), int(3)]],
        ),
        // One UNION arm imports, the other does not: the second arm's `n`
        // is fresh, so its `n-->(x)` is every outgoing hop.
        (
            "MATCH (n:A) CALL { WITH n MATCH (n)-->(x) RETURN x.id AS xi \
             UNION MATCH (n)-->(x) RETURN x.id AS xi } RETURN xi ORDER BY xi",
            vec![vec![int(2)], vec![int(3)]],
        ),
        // A naked call nested in a scope-clause body.
        (
            "MATCH (n:A) CALL (n) { CALL { MATCH (n) RETURN count(n) AS c } RETURN c } RETURN c",
            vec![vec![int(3)]],
        ),
        // The legacy importing `WITH` still imports: `n` is the outer node.
        (
            "MATCH (n:A) CALL { WITH n MATCH (n)-->(x) RETURN x.id AS xi } RETURN xi",
            vec![vec![int(2)]],
        ),
        // The explicit empty scope clause imports nothing, so the body's
        // `n` is fresh…
        (
            "MATCH (n:A) CALL () { MATCH (n) RETURN count(n) AS total } RETURN total",
            vec![vec![int(3)]],
        ),
        // …while `(*)` and `(n)` import the outer `n`, so the body's
        // `MATCH (n)` is a join on it — unchanged by this fix.
        (
            "MATCH (n:A) CALL (*) { MATCH (n) RETURN count(n) AS total } RETURN total",
            vec![vec![int(1)]],
        ),
        (
            "MATCH (n:A) CALL (n) { MATCH (n) RETURN count(n) AS total } RETURN total",
            vec![vec![int(1)]],
        ),
    ] {
        assert_eq!(rows(&mut conn, query), expected, "{query}");
    }
    // The write arm: the body updates every node, not just the outer one.
    run(
        &mut conn,
        "MATCH (n:A) CALL { MATCH (n) SET n.seen = true } RETURN count(*) AS c",
    );
    assert_eq!(
        scalar_one(&mut conn, "MATCH (x) WHERE x.seen RETURN count(*) AS n"),
        int(3),
        "a naked body's write covers every node it binds"
    );
}

/// Issue 50 under `CALL { … } IN TRANSACTIONS`: the same naked-import rule
/// per batch.
#[test]
fn a_naked_call_in_transactions_body_binds_its_own_names_fresh() {
    let mut conn = connection();
    run(&mut conn, "CREATE (:A {id: 1}), (:B {id: 2}), (:C {id: 3})");
    run(
        &mut conn,
        "MATCH (n:A) CALL { MATCH (n) SET n.s = 1 } IN TRANSACTIONS OF 1 ROWS",
    );
    assert_eq!(
        scalar_one(&mut conn, "MATCH (x) WHERE x.s = 1 RETURN count(*) AS n"),
        int(3),
        "every batch's body binds its own names"
    );
}

/// Issue 51: in a scope-clause `CALL (k) { … }` body — and in an expression
/// subquery, which sees the enclosing row — an imported / outer name stays
/// visible for the *whole* body, so a `MATCH … WHERE`, `SET`, `CREATE`,
/// `REMOVE` or `DETACH DELETE` after a body `WITH` still reads it. These
/// shapes were rejected with `UndefinedVariable` while the same name was
/// accepted by `RETURN`, `WITH … WHERE`, `UNWIND` and a MATCH property map.
#[test]
fn a_scope_clause_body_keeps_its_imports_past_a_with() {
    let mut conn = connection();
    run(&mut conn, "CREATE (:N {v: 5})");
    for (query, expected) in [
        (
            "WITH 5 AS k CALL (k) { WITH 1 AS one MATCH (x) WHERE x.v = k RETURN x.v AS xv } RETURN xv",
            vec![int(5)],
        ),
        (
            "WITH 5 AS k RETURN COUNT { WITH 1 AS one MATCH (x) WHERE x.v = k RETURN x } AS c",
            vec![int(1)],
        ),
        (
            "MATCH (n) RETURN COUNT { MATCH (n)-->(x) WITH x MATCH (x)-->(y) WHERE y <> n RETURN y } AS c",
            vec![int(0)],
        ),
        (
            "MATCH (n:N) CALL (n) { WITH 1 AS one SET n.s = one } RETURN n.s AS s",
            vec![int(1)],
        ),
        (
            "WITH 5 AS k CALL (k) { WITH 1 AS one CREATE (z:Z {v: k}) RETURN z.v AS zv } RETURN zv",
            vec![int(5)],
        ),
    ] {
        assert_eq!(first_column(&run(&mut conn, query)), expected, "{query}");
    }
    // `REMOVE` and `DETACH DELETE` on the import, behind a body `WITH`.
    run(
        &mut conn,
        "MATCH (n:N) CALL (n) { WITH 1 AS k REMOVE n:N } RETURN count(*) AS c",
    );
    assert_eq!(
        scalar_one(&mut conn, "MATCH (x:N) RETURN count(*) AS n"),
        int(0)
    );
    run(&mut conn, "CREATE (a:A {id: 1})-[:R]->(b:B {id: 2})");
    run(
        &mut conn,
        "MATCH (n:A) CALL (n) { WITH 1 AS k DETACH DELETE n } RETURN count(*) AS c",
    );
    assert_eq!(
        scalar_one(&mut conn, "MATCH (x:A) RETURN count(*) AS n"),
        int(0),
        "the DETACH DELETE on the import ran"
    );
}

/// Issue 51's boundary: the legacy (naked) importing `WITH` is an ordinary
/// projection — a later `WITH` ends its imports' scope, and a read past that
/// is still the front end's undefined variable.
#[test]
fn a_naked_bodys_importing_with_ends_its_scope_at_the_next_with() {
    let mut conn = connection();
    run(&mut conn, "CREATE (:N {v: 5})");
    let error = conn
        .query("MATCH (n:N) CALL { WITH n WITH 1 AS one RETURN n AS out } RETURN out")
        .unwrap_err();
    assert!(
        error.to_string().contains("UndefinedVariable"),
        "expected the import's scope to end at the body WITH: {error}"
    );
}

/// Issue 64: a `DISTINCT` or aggregating projection ends the scope of the
/// names it does not project, so a subquery body in the following `WHERE` /
/// `ORDER BY` that re-uses a dropped name binds it *fresh*. Relate pinned it
/// to the dropped value (dropping groups, and the writes behind them) or
/// rejected the aggregating spelling.
#[test]
fn a_subquery_after_distinct_or_aggregating_binds_a_dropped_name_fresh() {
    let store = "CREATE (:P {g: 1, v: 5}), (:P {g: 1, v: 1}), (:P {g: 2, v: 2})";
    let mut conn = connection();
    run(&mut conn, store);
    for (query, expected) in [
        // C1: a plain WITH keeps `n` in scope, so the body's `n` is the
        // outer one — the filter sees the v = 5 row only.
        (
            "MATCH (n:P) WITH n.g AS g WHERE EXISTS { MATCH (n) WHERE n.v > 4 } RETURN g",
            vec![int(1)],
        ),
        // C2: the DISTINCT form with a fresh body name.
        (
            "MATCH (n:P) WITH DISTINCT n.g AS g WHERE EXISTS { MATCH (q) WHERE q.v > 4 } \
             RETURN g ORDER BY g",
            vec![int(1), int(2)],
        ),
        // Q1 / Q2: the DISTINCT form with the dropped name re-used.
        (
            "MATCH (n:P) WITH DISTINCT n.g AS g WHERE EXISTS { MATCH (n) WHERE n.v > 4 } \
             RETURN g ORDER BY g",
            vec![int(1), int(2)],
        ),
        (
            "MATCH (n:P) WITH DISTINCT n.g AS g WHERE COUNT { MATCH (n) WHERE n.v > 4 } > 0 \
             RETURN g ORDER BY g",
            vec![int(1), int(2)],
        ),
    ] {
        assert_eq!(first_column(&run(&mut conn, query)), expected, "{query}");
    }
    // Q4 / Q5: the aggregating spellings, both columns.
    for query in [
        "MATCH (n:P) WITH n.g AS g, count(*) AS c WHERE EXISTS { MATCH (n) WHERE n.v > 4 } \
         RETURN g, c ORDER BY g",
        "MATCH (n:P) RETURN n.g AS g, count(*) AS c \
         ORDER BY COUNT { MATCH (n) WHERE n.v > 1 } DESC, g",
    ] {
        assert_eq!(
            rows(&mut conn, query),
            vec![vec![int(1), int(2)], vec![int(2), int(1)]],
            "{query}"
        );
    }
    // Q3: the write behind the DISTINCT form covers every group.
    run(
        &mut conn,
        "MATCH (n:P) WITH DISTINCT n.g AS g WHERE EXISTS { MATCH (n) WHERE n.v > 4 } \
         CREATE (:G {g: g})",
    );
    assert_eq!(
        first_column(&run(&mut conn, "MATCH (x:G) RETURN x.g AS g ORDER BY g")),
        vec![int(1), int(2)],
        "one :G per passing group"
    );
}
