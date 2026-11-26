//! REL8-2076 / corpus 35: the path aggregate fold ignores a relationship
//! variable bound by an earlier clause. A row-bound `r` pins the later
//! pattern to that one relationship; the fold walked every relationship of
//! the type instead, so the answer depended on the spelling of the
//! projection (`RETURN count(*)` was wrong, `WITH z RETURN count(*)` was
//! right). The fold declines when an entering row binds a pattern
//! relationship variable (ADR-0666); the general path reconciles it through
//! `bind_trail`. Every expectation is live Neo4j 2026.10 (the corpus
//! evidence folders).

use crate::common;

use common::{connection, int, ints, rows, run, scalar_one};
use relate::Value;

/// The corpus `repro.cases` graph: `(a:A)-[:R]->(b:B)-[:R]->(:E)`, and
/// `a` also reaches `:C` and `:D` by `:R`. The only `r` into a `:B` is
/// `a -> b`.
fn repro_graph(c: &mut relate::Connection) {
    run(
        c,
        "CREATE (a:A {id: 1})-[:R]->(b:B {id: 2})-[:R]->(:E {id: 5}), \
         (a)-[:R]->(:C {id: 3}), (a)-[:R]->(:D {id: 4})",
    );
}

/// The fold spelling (`RETURN count(*)`, answered by the path fold) and the
/// general spelling of the same statement must agree: the pinned `r` leaves
/// exactly one row. Before the fix the fold answered every `:R` out of `a`.
#[test]
fn a_row_bound_relationship_pins_the_folded_hop() {
    let mut c = connection();
    repro_graph(&mut c);

    let folded = scalar_one(
        &mut c,
        "MATCH ()-[r:R]->(:B) WITH r MATCH (x:A)-[r:R]->(z) RETURN count(*) AS n",
    );
    assert_eq!(folded, int(1), "the pinned r leaves one hop");

    let general = scalar_one(
        &mut c,
        "MATCH ()-[r:R]->(:B) WITH r MATCH (x:A)-[r:R]->(z) WITH z RETURN count(*) AS n",
    );
    assert_eq!(
        general, folded,
        "the answer must not depend on the spelling"
    );

    // The grouped and distinct seats read the same pin.
    assert_eq!(
        rows(
            &mut c,
            "MATCH ()-[r:R]->(:B) WITH r MATCH (x:A)-[r:R]->(z) \
             RETURN x.id AS g, count(*) AS n",
        ),
        vec![vec![int(1), int(1)]],
    );
    assert_eq!(
        scalar_one(
            &mut c,
            "MATCH ()-[r:R]->(:B) WITH r MATCH (x:A)-[r:R]->(z) RETURN count(DISTINCT z) AS n"
        ),
        int(1),
    );
    // The comma shape: the pinned hop plus the two other `:R` out of `a`.
    assert_eq!(
        scalar_one(
            &mut c,
            "MATCH ()-[r:R]->(:B) WITH r MATCH (x:A)-[r:R]->(z), (x)-[:R]->(w) \
             RETURN count(*) AS n"
        ),
        int(2),
    );
    // A relationship variable bound to null by a failed OPTIONAL MATCH
    // matches nothing.
    assert_eq!(
        scalar_one(
            &mut c,
            "MATCH (x:A) OPTIONAL MATCH (x)-[r:R]->(:Nope) \
             MATCH (x)-[r:R]->(z) RETURN count(*) AS n"
        ),
        int(0),
    );
    // The control that was already right without the fold.
    assert_eq!(
        scalar_one(
            &mut c,
            "MATCH ()-[r:R]->(:B) WITH r MATCH (x:A)-[r:R]->(z) RETURN collect(z.id) AS ids"
        ),
        ints(&[2]),
    );
}

/// The `fold_prebound.cases` graph: `a -R-> b -R-> c -R-> d`,
/// `a -R-> X -R-> c`, `b -R-> Y`, and the `:S` pair `a -> b -> c`.
fn prebound_graph(c: &mut relate::Connection) {
    run(
        c,
        "CREATE (a:A {id: 1})-[:R]->(b:B {id: 2})-[:R]->(c:C {id: 3})-[:R]->(d:D {id: 4}), \
         (a)-[:R]->(:X {id: 5})-[:R]->(c), (b)-[:R]->(:Y {id: 6}), \
         (a)-[:S]->(b), (b)-[:S]->(c)",
    );
}

/// The two-hop grouped count over the pinned hop (one group `2, 2`), the
/// star arm whose pinned hop is null (0), and the boolean probe over the
/// same pin.
#[test]
fn prebound_shapes_agree_with_the_general_path() {
    let mut c = connection();
    prebound_graph(&mut c);

    assert_eq!(
        rows(
            &mut c,
            "MATCH (:A)-[r:R]->(:B) WITH r MATCH (x)-[r]->(y)-[:R]->(z) \
             RETURN y.id AS g, count(*) AS n",
        ),
        vec![vec![int(2), int(2)]],
        "the pinned r is a -> b; b has two :R out",
    );
    assert_eq!(
        scalar_one(
            &mut c,
            "MATCH (x:A) OPTIONAL MATCH (x)-[r:R]->(:Nope) \
             MATCH (h)<-[r]-(l0), (h)-[:R]->(l1) RETURN count(*) AS n"
        ),
        int(0),
    );
    assert_eq!(
        scalar_one(
            &mut c,
            "MATCH (:A)-[r:R]->(:B) WITH r MATCH (x:A)-[r]->(z) RETURN count(*) > 0 AS any"
        ),
        Value::Bool(true),
    );
    assert_eq!(
        scalar_one(
            &mut c,
            "MATCH (x:A) OPTIONAL MATCH (x)-[r:R]->(:Nope) \
             MATCH (x)-[r]->(z) RETURN count(*) > 0 AS any"
        ),
        Value::Bool(false),
    );
}

/// The undirected single-hop shape: the pinned `r` is matched in both
/// directions (2 rows), not every relationship of the graph (6).
#[test]
fn an_undirected_hop_honors_the_pinned_relationship() {
    let mut c = connection();
    run(
        &mut c,
        "CREATE (a:A {id: 1})-[:R {w: 2}]->(b:B {id: 2}), \
         (a)-[:R {w: 1}]->(b), (b)-[:S]->(a)",
    );
    assert_eq!(
        scalar_one(
            &mut c,
            "MATCH ()-[r:R {w: 2}]->() WITH r MATCH (x)-[r]-(y) RETURN count(*) AS n"
        ),
        int(2),
    );
}

/// The map-conflict and bare-second-pattern shapes from the independent
/// confirmation: a pinned relationship whose pattern map disagrees with the
/// binding matches nothing; a patched later pattern accepts the binding.
#[test]
fn a_pinned_relationship_reconciles_with_the_later_pattern_map() {
    let mut c = connection();
    run(
        &mut c,
        "CREATE (a:A {id: 1})-[:T {w: 1}]->(b:B {id: 2}), \
         (a)-[:T {w: 2}]->(b), (b)-[:T {w: 3}]->(a)",
    );
    assert_eq!(
        scalar_one(
            &mut c,
            "MATCH ()-[r {w: 1}]->() MATCH ()-[r {w: 2}]->() RETURN count(*) AS n"
        ),
        int(0),
    );
    assert_eq!(
        scalar_one(
            &mut c,
            "MATCH ()-[r {w: 1}]->() MATCH ()-[r]->() RETURN count(*) AS n"
        ),
        int(1),
    );
}
