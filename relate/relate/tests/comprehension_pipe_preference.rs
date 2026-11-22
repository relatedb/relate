//! The comprehension-WHERE `|` resolves the way live Neo4j 2026.10 does:
//! the map-separator reading is preferred whenever the rest parses as the
//! map expression, and the label disjunction survives only when a *later*
//! pipe in the same comprehension makes the separator unparseable. A
//! single pipe over a label chain — `WHERE x:A|B]` — therefore reads
//! `WHERE x:A` with the map expression `B` and fails on the undefined
//! name, where Relate once returned the disjunction's rows; the two-pipe
//! and operator-then-pipe shapes keep the disjunction and answer.

mod common;

use common::{abc_store, err_text, first_column, run, strings};

/// A single pipe with no later pipe in the comprehension is the map
/// separator: the map expression is the bare would-be disjunct, an
/// undefined name that fails the whole statement, matching live.
#[test]
fn a_single_pipe_yields_the_separator_and_fails_the_undefined_map_name() {
    let mut c = abc_store();
    for q in [
        "MATCH (n) WITH collect(n) AS xs RETURN [x IN xs WHERE x:A|B] AS y",
        // The boolean operator after the would-be continuation changes
        // nothing: the map expression is the whole `B AND ...` chain.
        "MATCH (n) WITH collect(n) AS xs \
         RETURN [x IN xs WHERE x:A|B AND x.p1 IS NOT NULL] AS y",
        // The pattern comprehension's single pipe reads the same way.
        "MATCH (a) RETURN [(a)-[:R]->(b) WHERE a:A|B] AS y",
    ] {
        let error = err_text(c.query(q));
        assert!(
            error.contains("UndefinedVariable") && error.contains("'B'"),
            "{q}: wrong error: {error}"
        );
    }
}

/// A later pipe keeps the disjunction alive: the first pipe commits the
/// label disjunction and the second is the map separator — the answers
/// live agrees with.
#[test]
fn a_later_pipe_keeps_the_disjunction_alive() {
    let mut c = abc_store();
    assert_eq!(
        first_column(&run(
            &mut c,
            "MATCH (n) WITH collect(n) AS xs RETURN [x IN xs WHERE x:A|B | x.p1] AS y",
        )),
        vec![strings(&["a", "b"])],
    );
    // The operator shape with the later pipe: the disjunction is the
    // WHERE's left side and the later pipe is still the separator.
    assert_eq!(
        first_column(&run(
            &mut c,
            "MATCH (n) WITH collect(n) AS xs \
             RETURN [x IN xs WHERE x:A|B AND x.p1 IS NOT NULL | x.p1] AS y",
        )),
        vec![strings(&["a", "b"])],
    );
    // A three-way disjunction before the separator, same rule.
    assert_eq!(
        first_column(&run(
            &mut c,
            "MATCH (n) WITH collect(n) AS xs RETURN [x IN xs WHERE x:A|B|C | x.p1] AS y",
        )),
        vec![strings(&["a", "b", "c"])],
    );
}

/// The map expression a single pipe yields to may be any expression —
/// the parenthesized loop variable answers the node, matching live.
#[test]
fn the_yielded_separator_takes_a_parenthesized_map_expression() {
    let mut c = abc_store();
    let rows = first_column(&run(
        &mut c,
        "MATCH (n:A) WITH collect(n) AS xs RETURN [x IN xs WHERE x:A | (x)] AS y",
    ));
    assert_eq!(rows.len(), 1, "the one :A row carries the node itself");
    let inner = first_column(&run(
        &mut c,
        "MATCH (n:A) WITH collect(n) AS xs RETURN [x IN xs WHERE x:A | (x).p1] AS y",
    ));
    assert_eq!(inner, vec![strings(&["a"])]);
}
