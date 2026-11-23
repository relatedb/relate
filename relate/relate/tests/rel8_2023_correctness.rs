//! REL8-2023: the three reported wrong-result findings, as regression
//! tests — the batched `DELETE` over an empty unnamed factor (the corpus
//! issue 77), the existence probe merging two `MATCH` clauses under one
//! relationship-uniqueness scope (issue 84), and the scan-predicate gate
//! keyed by variable name dropping one occurrence's labels and map for a
//! repeated node variable (issue 21). Every expectation here is what
//! live Neo4j 2026.10 answers (the corpus evidence folders).

mod common;

use common::{
    connection, first_column, first_column_bools, first_column_strings, for_each_engine, run,
    BatchEnv,
};
use relate::Value;

/// The batched `DELETE` path collects the named factors' ids and forgets
/// that the row space is the cross product of ALL factors: with an empty
/// factor the statement has no rows, yet every named target was deleted
/// anyway once above the batch threshold. Batching is forced through the
/// one env seam (`BatchEnv`, threshold 50) instead of mirroring the
/// private default constant with a 10,000-node fixture.
#[test]
fn batched_delete_with_an_empty_unnamed_factor_deletes_nothing() {
    let _env = BatchEnv::forced();
    let mut c = connection();
    run(&mut c, "UNWIND range(1, 60) AS i CREATE (:A {id: i})");
    let count = "MATCH (a:A) RETURN count(a) AS n";
    for shape in [
        // The reported shape: a guard pattern that does not exist.
        "MATCH (a:A), (b:B) DELETE a",
        "MATCH (a:A), (b:B) DETACH DELETE a",
        // The same trap through an empty relationship pattern.
        "MATCH (a:A), ()-[r:NONE]->() DELETE a",
        // And through a second MATCH clause.
        "MATCH (a:A) MATCH (b:B) DELETE a",
    ] {
        run(&mut c, shape);
        assert_eq!(
            first_column(&run(&mut c, count)),
            vec![Value::Int(60)],
            "{shape}: deleted nodes whose MATCH produced no rows"
        );
    }
}

/// A `WITH` between the `MATCH` and the write that drops a factor
/// declined batching entirely (issue 72's multiplicity and 77's WITH
/// shape are the same premise break): the dropped factor still empties
/// or scales the row space, and the batched path cannot see it. The
/// general path answers — nothing while the dropped factor is empty,
/// and with it populated only the carried binding.
#[test]
fn a_with_that_drops_a_factor_does_not_delete_it_all() {
    let _env = BatchEnv::forced();
    let mut c = connection();
    run(&mut c, "UNWIND range(1, 60) AS i CREATE (:A {id: i})");
    // The shadowing rename, the one spelling the decline loop and the
    // control below share: `a` is carried under `b`'s name, and the
    // incoming `b` factor is dropped the same way.
    const SHADOWING_WITH_DELETE: &str = "MATCH (a:A), (b:B) WITH a AS b DELETE b";
    let count = "MATCH (a:A) RETURN count(a) AS n";
    for shape in [
        // The reported spelling: only `a` is carried.
        "MATCH (a:A), (b:B) WITH a DELETE a",
        SHADOWING_WITH_DELETE,
    ] {
        run(&mut c, shape);
        assert_eq!(
            first_column(&run(&mut c, count)),
            vec![Value::Int(60)],
            "{shape}: the WITH shape deleted nodes whose MATCH produced no rows"
        );
    }
    // Control: the dropped factor populated, the general path deletes
    // only the carried binding — the `:B` the dropped name bound
    // survives.
    run(&mut c, "CREATE (:B {id: 1})");
    run(&mut c, SHADOWING_WITH_DELETE);
    assert_eq!(
        first_column(&run(&mut c, "MATCH (x:B) RETURN x.id AS id")),
        vec![Value::Int(1)]
    );
    assert_eq!(first_column(&run(&mut c, count)), vec![Value::Int(0)]);
}

/// Relationship uniqueness is scoped to one `MATCH` clause: two clauses
/// may bind the same relationship. The existence probe merged both
/// clauses' patterns into one `DIFFERENT RELATIONSHIPS` list, so a
/// two-clause body only the same relationship satisfies answered false —
/// and a `NOT EXISTS` feeding a `DETACH DELETE` deleted the node live
/// keeps.
#[test]
fn existence_probe_across_two_match_clauses_allows_the_same_relationship() {
    let mut c = connection();
    run(&mut c, "CREATE (:A {id: 1})-[:R]->(:B {id: 2})");
    assert_eq!(
        first_column_bools(&run(
            &mut c,
            "RETURN EXISTS { MATCH ()-[r]->() MATCH ()-[s]->() } AS e"
        )),
        vec![true]
    );
    assert_eq!(
        first_column(&run(
            &mut c,
            "RETURN COUNT { MATCH ()-[r]->() MATCH ()-[s]->() } AS n"
        )),
        vec![Value::Int(1)]
    );
    // The `count(DISTINCT x) > 0` fold of the same two-clause shape.
    assert_eq!(
        first_column_bools(&run(
            &mut c,
            "MATCH (a:A)-[:R]->(b) MATCH (b)<-[:R]-(c) RETURN count(DISTINCT c) > 0 AS e"
        )),
        vec![true]
    );
    // A `NOT EXISTS` over the two-clause shape must not delete.
    run(
        &mut c,
        "MATCH (a:A) WHERE NOT EXISTS { MATCH (a)-->(b) MATCH (b)<--(c) } DETACH DELETE a",
    );
    assert_eq!(
        first_column(&run(&mut c, "MATCH (a:A) RETURN count(a) AS n")),
        vec![Value::Int(1)]
    );
    // Control: the single-clause body keeps the probe's answer.
    assert_eq!(
        first_column_bools(&run(&mut c, "RETURN EXISTS { MATCH ()-[r]->() } AS e")),
        vec![true]
    );
}

/// A node variable repeated in one path pattern (a cycle closing onto
/// it) is constrained by every occurrence's labels and property map. The
/// scan-predicate gate is keyed by variable name, so the bare
/// occurrence's gate replaced the decorated occurrence's checks under a
/// compilable WHERE conjunct. Every pattern engine must agree. The
/// fixture seeds every label and hop a listed shape constrains — the
/// cycle closes on the unlabeled `n`, the `:Z` population sits outside
/// it, the `:C` hop sits inside — so each shape is satisfiable through
/// the bare occurrence alone and can produce the buggy row it describes.
#[test]
fn a_repeated_node_variable_keeps_every_occurrences_checks() {
    for_each_engine(|_engine, c| {
        run(
            c,
            "CREATE (n {p: 2}), (z:Z {p: 2}), (b:B), (c:C), \
             (b)-[:R]->(n), (b)-[:R]->(n), (b)-[:S]->(c), (c)-[:S]->(n)",
        );
        for q in [
            // The reported shape: the map is on the first occurrence.
            "MATCH (a {p: 1})<--(b:B)-->(a) WHERE a.p > 0 RETURN a.p",
            // The map folded from the WHERE.
            "MATCH (a)<--(b:B)-->(a) WHERE a.p = 1 AND a.p > 0 RETURN a.p",
            // A label on the repeated variable, first or closing.
            "MATCH (a:Z)<--(b:B)-->(a) WHERE a.p > 0 RETURN a.p",
            "MATCH (a)<--(b:B)-->(a:Z) WHERE a.p > 0 RETURN a.p",
            // The map on the closing occurrence.
            "MATCH (a)<--(b:B)-->(a {p: 1}) WHERE a.p > 0 RETURN a.p",
            // Three hops closing the cycle.
            "MATCH (a {p: 1})<--(b:B)-->(c:C)-->(a) WHERE a.p > 0 RETURN a.p",
        ] {
            assert!(
                run(c, q).rows.is_empty(),
                "{q}: rows from a pattern no node satisfies"
            );
        }
        // The write side: no SET may reach the node.
        run(
            c,
            "MATCH (a {p: 1})<--(b:B)-->(a) WHERE a.p > 0 SET a.hit = true",
        );
        assert_eq!(
            first_column(&run(c, "MATCH (a) RETURN count(a.hit) AS n")),
            vec![Value::Int(0)]
        );
        // Control without the gating WHERE conjunct: still no rows, and
        // the comma spelling splits the occurrences across patterns.
        assert!(run(c, "MATCH (a {p: 1})<--(b:B)-->(a) RETURN a.p")
            .rows
            .is_empty());
        assert!(run(c, "MATCH (a {p: 1})<--(b:B), (b)-->(a) RETURN a.p")
            .rows
            .is_empty());
    });
}

/// The same repeated-variable shape on the default engine, pinning the
/// exact reported statement: no `Bob` rows when no `Alice` exists.
#[test]
fn where_predicate_does_not_drop_the_repeated_variables_map() {
    let mut c = connection();
    run(
        &mut c,
        "CREATE (t:Team)-[:HAS]->({name: 'Bob'})<-[:HAS]-(t)",
    );
    assert_eq!(
        first_column_strings(&run(
            &mut c,
            "MATCH (p {name: 'Alice'})<--(t:Team)-->(p) WHERE p.name <> 'Carl' RETURN p.name"
        )),
        Vec::<String>::new()
    );
}
