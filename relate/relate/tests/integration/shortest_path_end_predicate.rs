//! 51N23 and the end pattern's inline predicate: the same-node pair a
//! `shortestPath()` spelling would reject forms only if the origin
//! satisfies the end pattern — an origin failing its inline `WHERE` is not
//! a reached end, so the pair never exists and the route answers the real
//! ends (the TCK's `ShortestPathAcceptance` "Using all with shortestPath
//! and property predicate" pair, live-planned this way). An origin that
//! *does* satisfy it keeps the 51N23 rejection.

use crate::common;

use common::{connection, scalar};
use relate::Value;

/// The corpus setup: a chain of levels, one A-coded child per level —
/// seeded once through the suite's canonical runner.
const CHAIN: &[&str] = &[
    "CREATE (:N {level: 0})",
    "UNWIND range(0, 4) AS level CALL (level) { \
     MATCH (n {level: level}) UNWIND ['A', 'B', 'C'] AS code \
     CREATE (n)-[:R]->(:N {level: level + 1, code: code}) }",
];

fn seeded() -> relate::Connection {
    let mut c = connection();
    for statement in CHAIN {
        common::run(&mut c, statement);
    }
    c
}

#[test]
fn an_origin_failing_the_end_predicate_is_not_a_same_node_pair() {
    let mut c = seeded();
    // The origin (level 0) fails `e.level > 0`, so the (s, s) partition
    // never forms — no 51N23 rejection — and the real ends answer.
    assert_eq!(
        scalar(
            &mut c,
            "MATCH p = shortestPath((:N {level: 0})-[r*]-(e:N WHERE e.level > 0)) \
             WHERE all(n IN nodes(p) WHERE coalesce(n.code = 'A', true)) \
             RETURN count(p) AS c"
        ),
        Value::Int(5)
    );
}

#[test]
fn an_origin_satisfying_the_end_predicate_keeps_the_rejection() {
    let mut c = seeded();
    // The origin satisfies `e.level >= 0`, so the same-node pair forms and
    // 51N23 rejects it exactly as before.
    let err = common::err_text(c.query(
        "MATCH p = shortestPath((:N {level: 0})-[r*]-(e:N WHERE e.level >= 0)) \
         RETURN count(p) AS c",
    ));
    assert!(
        err.contains("start and end nodes are the same"),
        "the satisfying-origin pair still rejects, got {err}"
    );
}
