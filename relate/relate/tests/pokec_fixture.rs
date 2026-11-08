//! The synthetic pokec fixture as the integration suites use it: open
//! `SMALL` (the traverse-bench small counts) through `relate-test-support`,
//! then run three representative benchmark queries against it — a point
//! read through the `user_id` index, a multi-hop `DISTINCT` expansion,
//! and the age-index-driven 2-hop filter class. This is the fixture's
//! consumer contract: if the fixture's shape drifts away from what these
//! query classes need, this test says so.

mod common;

use common::scalar_one;
use relate::Value;
use relate_test_support::pokec::{self, PokecShape};

/// One integer scalar, through the shared extractor (`common::scalar_one`).
fn count(conn: &mut relate::Connection, cypher: &str) -> i64 {
    match scalar_one(conn, cypher) {
        Value::Int(n) => n,
        other => panic!("{cypher} did not answer one integer (got {other:?})"),
    }
}

#[test]
fn the_small_fixture_serves_the_benchmark_query_classes() {
    let db = pokec::open(PokecShape::SMALL).unwrap();
    let mut conn = db.connect().unwrap();

    // single_vertex_read: one point through the index, one row.
    assert_eq!(
        count(&mut conn, "MATCH (n:User {id: 5000}) RETURN count(n)"),
        1
    );

    // expansion_2's class: distinct 2-hop neighbours of a mid-degree
    // node — non-empty, and no more than the node count.
    let two_hop = count(
        &mut conn,
        "MATCH (s:User {id: 5000})-->()-->(n:User) RETURN count(DISTINCT n)",
    );
    assert!(two_hop > 0, "the 2-hop class answered nothing");
    assert!(
        two_hop <= PokecShape::SMALL.nodes as i64,
        "the 2-hop class exceeded the node count: {two_hop}"
    );

    // scan_expand_2hop_filter's class: the age index drives the answer,
    // and it is a small one like the real data's.
    let over_50 = count(&mut conn, "MATCH (n:User) WHERE n.age > 50 RETURN count(n)");
    assert!(
        over_50 > 0,
        "the age > 50 class is empty — the skew erased it"
    );
    assert!(over_50 <= 100, "the age > 50 class is not small: {over_50}");

    // Both relationship endpoints are `:User` — the schema invariant the
    // fixture promises, checked with the endpoint types the pattern
    // names rather than an untyped `()`.
    assert_eq!(
        count(
            &mut conn,
            "MATCH (a:User)-[e:Friend]->(b:User) RETURN count(e)"
        ),
        PokecShape::SMALL.rels as i64
    );
}
