//! The seek recognizers' bound-readiness pins: a bound side that is not
//! ready on the entry row — a subquery (`EXISTS`/`COUNT`) or a name the
//! pattern has not bound yet — declines the seek instead of evaluating
//! early and serving a wrong constant. Two regressions of the same class:
//! `WHERE p.flag = EXISTS { … }` served `p.flag = true` (the subquery ran
//! with `p` free and matched anything), keeping only the rows where both
//! the equality and the subquery held; and `WHERE a.prop = COUNT { (b)… }`
//! served `a.prop = <the whole side's count>` once the planner expanded
//! `(a)` first, emptying the pattern. The residual re-applies the
//! predicate per candidate row, so a declined seek is the same answer,
//! correctly reached.

use super::test_support::{column, exec_autocommit, exec_cypher, int_cell, string_cell};
use crate::graph::Graph;

/// Three people — one whose flag agrees with having a dog, one whose
/// disagrees — the fixture the EXISTS-operand regression reads.
fn persons_graph() -> Graph {
    let g = Graph::default();
    exec_autocommit(
        &g,
        "CREATE (:Person {name:'Alice', canAffordDog: false}), \
         (:Person {name:'Bosse', canAffordDog: true})-[:HAS_DOG]->(:Dog), \
         (:Person {name:'Chris', canAffordDog: false})-[:HAS_DOG]->(:Dog)",
    )
    .unwrap();
    g
}

/// `WHERE p.canAffordDog = EXISTS { … }` compares the boolean the
/// subquery yields — Alice (false = false) and Bosse (true = true) —
/// instead of keeping only the rows where the subquery matches (Bosse
/// and Chris) as an early-evaluated `canAffordDog = true` seek did.
#[test]
fn an_exists_subquery_operand_compares_its_boolean() {
    let g = persons_graph();
    let names =
        |query: &str| -> Vec<String> { column(&exec_cypher(&g, query), "p.name", string_cell) };
    let eq = "MATCH (p:Person) \
              WHERE p.canAffordDog = EXISTS { MATCH (p)-[:HAS_DOG]->(:Dog) } \
              RETURN p.name ORDER BY p.name";
    assert_eq!(names(eq), vec!["Alice", "Bosse"]);
    // The operand order cannot change the comparison.
    let eq_reversed = "MATCH (p:Person) \
                       WHERE EXISTS { MATCH (p)-[:HAS_DOG]->(:Dog) } = p.canAffordDog \
                       RETURN p.name ORDER BY p.name";
    assert_eq!(names(eq_reversed), vec!["Alice", "Bosse"]);
    // `<>` never sought (no recognizer arm), so it was always right —
    // the control that must stay green: Chris alone disagrees.
    let neq = "MATCH (p:Person) \
               WHERE p.canAffordDog <> EXISTS { MATCH (p)-[:HAS_DOG]->(:Dog) } \
               RETURN p.name ORDER BY p.name";
    assert_eq!(names(neq), vec!["Chris"]);
    // A ready bound still seeks: the constant comparison answers from
    // the postings, the rows unchanged.
    let constant = "MATCH (p:Person) WHERE p.canAffordDog = false \
                    RETURN p.name ORDER BY p.name";
    assert_eq!(names(constant), vec!["Alice", "Chris"]);
}

/// `WHERE a.prop = COUNT { (b)-->(:X) }` with the COUNT importing `b`,
/// the pattern the planner expands second: the count bound is not ready
/// on the entry row, so the seek declines and the residual compares the
/// per-row count — (1, 'one') and (3, 'three') — in either pattern
/// order, where an early-evaluated `a.prop = 4` seek emptied the
/// pattern.
#[test]
fn a_count_subquery_bound_waits_for_its_pattern() {
    let g = Graph::default();
    exec_autocommit(
        &g,
        "CREATE (:A {prop: 1}) \
         CREATE (:A {prop: 2}) \
         CREATE (:A {prop: 3}) \
         CREATE (b1:B {name: 'one'})-[:REL]->(:X) \
         CREATE (b3:B {name: 'three'})-[:REL]->(:X) \
         CREATE (b3)-[:REL]->(:X) \
         CREATE (b3)-[:REL]->(:X) \
         CREATE (b0:B {name: 'zero'})",
    )
    .unwrap();

    let titles = |query: &str| -> Vec<(i64, String)> {
        let result = exec_cypher(&g, query);
        column(&result, "a.prop", int_cell)
            .into_iter()
            .zip(column(&result, "b.name", string_cell))
            .collect()
    };
    let expected = vec![(1, "one".into()), (3, "three".into())];
    let a_first = "MATCH (a:A), (b:B) \
                   WHERE a.prop = COUNT { (b)-->(:X) } \
                   RETURN a.prop, b.name ORDER BY a.prop";
    assert_eq!(titles(a_first), expected);
    let b_first = "MATCH (b:B), (a:A) \
                   WHERE a.prop = COUNT { (b)-->(:X) } \
                   RETURN a.prop, b.name ORDER BY a.prop";
    assert_eq!(titles(b_first), expected);
}
