//! REL8-1188: deleting, inside one explicit transaction, an entity an
//! earlier statement of that transaction created. The delete's statement
//! counters must stay non-negative (the create was counted by the earlier
//! statement, so there is nothing to net against here) — the wire-stats
//! boundary `relate::query_stats_to_wire` states that invariant with an
//! `expect`, and a negative counter there poisons every language binding's
//! connection. Live Neo4j 2026.10.1 (probed 2026-11-22) reports the delete
//! statement's counters as `nodes_deleted: 1` — the engine's
//! same-statement netting model (`create_then_delete_cancels_create_stats`
//! in the executor's overlay tests) is untouched; only the cross-statement
//! shape counts the delete.

use relate::{Connection, Value};

use crate::common;

/// Run `query` and hand the counters through the canonical wire boundary,
/// the one home of the entity-counter non-negativity invariant (its `expect`
/// is the one a negative counter trips). The two property counters the
/// netting writes are not on the wire — `properties_removed` is engine-local
/// and the wire's `properties_set` renders `properties_written` — so they are
/// asserted here beside it.
fn delete_stats(conn: &mut Connection, query: &str) -> relate::QueryStats {
    let result = conn.query(query).expect("the delete statement runs");
    assert!(
        result.stats.properties_set >= 0 && result.stats.properties_removed >= 0,
        "property counters are non-negative: {:?}",
        result.stats
    );
    relate::query_stats_to_wire(&result.stats);
    result.stats
}

/// A node created by one statement and deleted by the next statement of
/// the same transaction: the delete counts, nothing nets, the transaction
/// commits, and the connection stays usable.
#[test]
fn tx_delete_of_node_created_by_earlier_statement_counts_the_delete() {
    let mut conn = common::connection();
    conn.begin().unwrap();
    let created = conn.query("CREATE (:T {k: 2})").unwrap().stats;
    assert_eq!(created.nodes_created, 1);
    let deleted = delete_stats(&mut conn, "MATCH (n:T {k: 2}) DELETE n");
    assert_eq!(deleted.nodes_created, 0);
    assert_eq!(deleted.nodes_deleted, 1);
    assert_eq!(deleted.properties_removed, 1);
    conn.commit().unwrap();
    assert_eq!(
        conn.query("MATCH (n:T) RETURN count(n) AS c").unwrap().rows,
        vec![vec![Value::Int(0)]]
    );
}

/// The relationship twin: a rel created by one statement and deleted by
/// the next statement of the same transaction counts as this statement's
/// rel delete. `DELETE r` deletes only the relationship — the created
/// endpoints survive — so this statement's counters are exactly the rel
/// delete (no node counters move).
#[test]
fn tx_delete_of_rel_created_by_earlier_statement_counts_the_delete() {
    let mut conn = common::connection();
    conn.begin().unwrap();
    let created = conn.query("CREATE (:A)-[:R {x: 1}]->(:B)").unwrap().stats;
    assert_eq!(created.relationships_created, 1);
    let deleted = delete_stats(&mut conn, "MATCH ()-[r:R]->() DELETE r");
    assert_eq!(deleted.relationships_created, 0);
    assert_eq!(deleted.relationships_deleted, 1);
    assert_eq!(deleted.properties_removed, 1);
    conn.commit().unwrap();
    assert_eq!(
        conn.query("MATCH ()-[r:R]->() RETURN count(r) AS c")
            .unwrap()
            .rows,
        vec![vec![Value::Int(0)]]
    );
}

/// The one-statement pair keeps the pinned netting model: a create and its
/// delete inside one statement cancel, reporting no counters — inside a
/// transaction just as in auto-commit.
#[test]
fn tx_create_then_delete_in_one_statement_still_nets() {
    let mut conn = common::connection();
    conn.begin().unwrap();
    let stats = delete_stats(&mut conn, "CREATE (n:T {k: 3}) WITH n DELETE n");
    assert_eq!(stats.nodes_created, 0);
    assert_eq!(stats.nodes_deleted, 0);
    conn.commit().unwrap();
    assert_eq!(
        conn.query("MATCH (n:T) RETURN count(n) AS c").unwrap().rows,
        vec![vec![Value::Int(0)]]
    );
}
