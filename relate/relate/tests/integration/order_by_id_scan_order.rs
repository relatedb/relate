//! `ORDER BY id(n)` over a plain `MATCH (n)` is the scan's own order, so the
//! planner drops the sort. The rewrite must answer exactly what the sort
//! answers, and must leave every shape whose scan order is not id order
//! (descending, `elementId`, relationships, filtered or labeled patterns,
//! aliases that rebind the variable) on the sort.

use crate::common;

fn ids(c: &mut relate::Connection, q: &str) -> Vec<i64> {
    common::first_column_ints(&common::run(c, q))
}

fn seeded() -> relate::Connection {
    let mut c = common::connection();
    common::run(&mut c, "UNWIND range(1, 25) AS i CREATE (:N {k: i})");
    common::run(&mut c, "MATCH (n:N) WHERE n.k % 5 = 0 DETACH DELETE n");
    common::run(&mut c, "UNWIND range(1, 3) AS i CREATE (:M {k: i})");
    c
}

#[test]
fn the_dropped_sort_answers_what_the_sort_answers() {
    let mut c = seeded();
    for page in [
        "",
        "LIMIT 7",
        "SKIP 4 LIMIT 7",
        "SKIP 100 LIMIT 3",
        "LIMIT 0",
    ] {
        let fast = ids(
            &mut c,
            &format!("MATCH (n) RETURN id(n) AS i ORDER BY id(n) {page}"),
        );
        // The oracle sorts a projected column, a shape the rewrite never touches.
        let oracle = ids(
            &mut c,
            &format!("MATCH (n) WITH id(n) AS i ORDER BY i {page} RETURN i"),
        );
        assert_eq!(fast, oracle, "page {page:?}");
    }
}

#[test]
fn shapes_outside_the_rewrite_keep_their_order() {
    let mut c = seeded();
    let desc = ids(
        &mut c,
        "MATCH (n) RETURN id(n) AS i ORDER BY id(n) DESC LIMIT 3",
    );
    assert!(desc.windows(2).all(|w| w[0] > w[1]), "{desc:?}");
    let labeled = ids(
        &mut c,
        "MATCH (n:M) RETURN id(n) AS i ORDER BY id(n) LIMIT 3",
    );
    assert!(labeled.windows(2).all(|w| w[0] < w[1]), "{labeled:?}");
    // An alias named like the variable rebinds it: id(n) reads the alias.
    let rebound = common::err_text(c.query("MATCH (n) RETURN n.k AS n ORDER BY id(n) LIMIT 3"));
    assert!(
        rebound.contains("id() needs a node or relationship"),
        "{rebound}"
    );
}

/// A `NEXT` stage can import a bound `n`, which turns its `MATCH (n)` from a
/// scan into a lookup of that one node: the sort over it is not a no-op, and
/// the rewrite (first stage only) must leave it.
#[test]
fn a_later_stage_matching_an_imported_variable_keeps_its_sort() {
    let mut c = seeded();
    let q = "MATCH (n:N) WITH n ORDER BY id(n) DESC LIMIT 3 RETURN n NEXT \
             MATCH (n) RETURN id(n) AS i ORDER BY id(n) LIMIT 5";
    let got = ids(&mut c, q);
    assert!(got.windows(2).all(|w| w[0] < w[1]), "{got:?}");
}
