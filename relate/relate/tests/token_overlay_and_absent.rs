//! Planner-resolved token ids meet names the plan could not resolve:
//! names created earlier in the same statement (write overlay) and names
//! never interned at all. Both must read correctly under the walker and
//! the forced frontier-NFA engines (ADR-0519).

mod common;

use common::{for_each_engine, rows, run, scalar};

#[test]
fn names_created_in_the_same_statement_are_read_back() {
    for_each_engine(|engine, conn| {
        let count = scalar(
            conn,
            "CREATE (:Fresh {k: 1}) WITH 1 AS one \
             MATCH (n:Fresh {k: 1}) RETURN count(n)",
        );
        assert_eq!(count, relate::Value::Int(1), "engine {engine}");
    });
}

#[test]
fn never_interned_names_match_nothing() {
    for_each_engine(|engine, conn| {
        run(conn, "CREATE (:Known {k: 1})-[:KNOWS]->(:Known {k: 2})");
        for query in [
            "MATCH (n:NeverLabel) RETURN n",
            "MATCH (:Known)-[r:NEVER_TYPE]->() RETURN r",
            "MATCH (n:Known {never_key: 1}) RETURN n",
            "MATCH (n:Known) WHERE n.never_key = 1 RETURN n",
            "MATCH (:Known)-[r:KNOWS]->() WHERE r.never_key = 1 RETURN r",
            "MATCH (:Known)-[r:KNOWS {never_key: 1}]->() RETURN r",
            "MATCH (n:Known) WHERE n.never_key IS NOT NULL RETURN n",
            "MATCH (a:NeverLabel)-[:NEVER_TYPE]->(b {never_key: 1}) RETURN a, b",
        ] {
            assert!(rows(conn, query).is_empty(), "engine {engine}: {query}");
        }
    });
}

#[test]
fn label_created_by_a_batch_is_matched_later_in_the_statement() {
    for_each_engine(|engine, conn| {
        run(
            conn,
            "UNWIND range(1, 4) AS i CALL { WITH i CREATE (:Batched {i: i}) } \
             IN TRANSACTIONS OF 2 ROWS \
             WITH i MATCH (n:Batched {i: i}) RETURN n",
        );
        assert_eq!(
            scalar(conn, "MATCH (n:Batched) RETURN count(n)"),
            relate::Value::Int(4),
            "engine {engine}"
        );
    });
}

#[test]
fn rel_key_created_in_the_same_statement_is_read_back() {
    for_each_engine(|engine, conn| {
        let count = scalar(
            conn,
            "CREATE (:A)-[:FRESH_REL {fresh_key: 1}]->(:B) WITH 1 AS one \
             MATCH ()-[r:FRESH_REL {fresh_key: 1}]->() RETURN count(r)",
        );
        assert_eq!(count, relate::Value::Int(1), "engine {engine}");
    });
}
