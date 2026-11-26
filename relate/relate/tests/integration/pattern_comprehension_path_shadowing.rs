//! A pattern comprehension's path variable is an assignment, not a join
//! (ADR-0685): `[p = (pat) | …]` binds `p` fresh for the body's whole
//! scope, shadowing any outer name of the same spelling — verified against
//! live Neo4j (`MATCH p = (n:X)-->(b) RETURN [x IN nodes(p) |
//! size([p=(x)-->(:Y) | p])]` answers `[1, 2]`, the inner path's own
//! matches). Pattern *element* variables are the opposite: an element an
//! outer variable names joins on that value. Both rules are pinned here
//! because the collect-subquery lowering must carry the shadow (the nested
//! seed does not import it) while the joins ride the ordinary import.

use crate::common;

use common::{connection, first_column, int, ints, run};

/// The store the live-verified shadowing case runs on.
fn shadow_store() -> relate::Connection {
    let mut c = connection();
    run(
        &mut c,
        "CREATE (n1:X {n: 1}), (m1:Y {v: 1}), (i1:Y {v: 2}), (i2:Y {v: 3}) \
         CREATE (n1)-[:T]->(m1), (m1)-[:T]->(i1), (m1)-[:T]->(i2)",
    );
    c
}

/// The corpus shape: an inner path variable named like the outer one
/// shadows it, and the count follows the inner pattern's matches.
#[test]
fn a_path_variable_shadows_an_outer_name_of_the_same_spelling() {
    let mut c = shadow_store();
    let row = common::row(
        &mut c,
        "MATCH p = (n:X)-->(b) \
         RETURN n.n AS n, [x IN nodes(p) | size([p=(x)-->(:Y) | p])] AS list",
    );
    assert_eq!(row, vec![int(1), ints(&[1, 2])], "n1's inner matches");
}

/// The outer path variable stays visible under a different inner name —
/// only the same spelling shadows.
#[test]
fn an_outer_path_variable_stays_visible_under_another_name() {
    let mut c = shadow_store();
    let l = first_column(&run(
        &mut c,
        "MATCH p = (n:X)-->(b) RETURN [q=(n)-->(:Y) | length(p)] AS l",
    ));
    assert_eq!(
        l,
        vec![ints(&[1])],
        "the outer path's length, read through the outer name"
    );
}

/// A pattern element an outer variable names still joins on its value:
/// the shadow rule is the path variable's alone.
#[test]
fn a_pattern_element_variable_joins_instead_of_shadowing() {
    let mut c = shadow_store();
    // `m` is bound outside, so the comprehension's element joins on it:
    // the outer `m` (v = 1) leaves exactly one match. A shadowing read
    // would answer every `:Y` (1, 2 and 3).
    let l = first_column(&run(
        &mut c,
        "MATCH (n:X)-[:T]->(m:Y) RETURN [(n)-[:T]->(m) | m.v] AS l",
    ));
    assert_eq!(l, vec![ints(&[1])], "the outer m joined");
}
