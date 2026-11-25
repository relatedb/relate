//! LABS-811 item 3: the var-length rel binding is modeled — the
//! rel-list spelling of the walk vocabulary ([`crate::pattern_walk`]'s
//! `RelListBinding`/`rel_list_binding`), the `Binding<Vec<RelId>>` form
//! of the classification every other pattern element already reads — and
//! the two classic engines reconcile a pre-bound trail through it
//! instead of each hand-rolling its own policy over the same value.
//!
//! The pins, one graph (a four-node `R` chain, so one- and two-hop
//! trails coexist):
//!
//! - **The exact-trail rule, both engines** — a pre-bound relationship
//!   list pins the trail (LABS-661, the statement engine's GPM
//!   `Prebound` policy; `overlay::tests::bound_rel_list_is_exact_trail`
//!   pins it at statement level). The eval walk (pattern comprehensions,
//!   pattern predicates, expression shortest paths — the compact `rels`
//!   view) used to overwrite the pre-bound list without reconciling, so
//!   a comprehension enumerated freely past a row-bound trail: these
//!   pins hold both engines to the one rule.
//! - **A malformed list matches nothing** — a list holding a
//!   non-relationship element is a `Mismatch`, the same predicate as
//!   every other spelling of the vocabulary, not an unbound variable
//!   the walk silently rebinds.
//! - **A null pre-binding matches nothing in the eval walk** — the
//!   classic family's own rule (the Neo4j-verified single-hop behavior,
//!   LABS-711; the family's pin
//!   `gpm::gpm_tests::a_null_prebound_rel_variable_is_not_rebound_by_the_classic_walker`
//!   documents it and the GPM engine's deliberate, ticketed divergence
//!   from it — ADR-0335's null-is-fresh arm, which this change does not
//!   touch). The eval walk's var-length arm used to rebind a null
//!   pre-binding freely; it now answers the family's zero rows.

use super::test_support::{build, column, exec_cypher};
use relate_core::Value;

/// The pin graph: `(0:A)-[:R]->(1:B)-[:R]->(2:C)-[:R]->(3:D)` — five
/// `*1..2` trails (`[r0]`, `[r1]`, `[r2]`, `[r0,r1]`, `[r1,r2]`), two
/// `*2..2` trails (`[r0,r1]`, `[r1,r2]`).
fn chain() -> crate::graph::Graph {
    build(
        &[&["A"], &["B"], &["C"], &["D"]],
        &[(0, 1, "R"), (1, 2, "R"), (2, 3, "R")],
    )
    .graph
}

/// The `c` column of the result, one integer per row, in result order.
fn counts(cypher: &str, g: &crate::graph::Graph) -> Vec<i64> {
    let done = exec_cypher(g, cypher);
    column(&done, "c", Value::as_int)
}

#[test]
fn a_prebound_rel_list_pins_the_exact_trail_in_both_engines() {
    let g = &chain();
    // The eval walk (a pattern comprehension is its reach): each `t` row
    // enumerates only its own trail — one match per row, not the free
    // five.
    let sizes = counts(
        "MATCH (a)-[t:R*1..2]->(b) WITH t \
         RETURN size([(x)-[t*1..2]->(y) | y]) AS c",
        g,
    );
    assert_eq!(
        sizes,
        vec![1; 5],
        "the comprehension must honor the bound trail"
    );
    // The statement engine (the GPM walk a top-level var-length rel
    // lowers to): the same rule, already pinned — the positive control
    // that the two engines now agree.
    let done = exec_cypher(
        g,
        "MATCH (a)-[t:R*1..2]->(b) WITH t MATCH (x)-[t*1..2]->(y) RETURN y.id AS c",
    );
    let mut ends = column(&done, "c", Value::as_int);
    ends.sort_unstable();
    assert_eq!(
        ends,
        vec![1, 2, 2, 3, 3],
        "each bound trail must reach exactly its own end node"
    );
}

#[test]
fn a_prebound_var_length_list_with_a_non_relationship_element_matches_nothing() {
    let g = &chain();
    // A list of null: a malformed trail binding, not an unbound variable.
    assert_eq!(
        counts(
            "WITH [null] AS q RETURN size([(x)-[q*1..2]->(y) | y]) AS c",
            g
        ),
        vec![0],
        "the eval walk must not enumerate past a malformed list"
    );
    // One real relationship beside a null element is still malformed.
    assert_eq!(
        counts(
            "MATCH (a)-[t:R]->(b) WITH [t, null] AS q \
             RETURN size([(x)-[q*1..2]->(y) | y]) AS c",
            g,
        ),
        vec![0; 3],
        "a mixed list is a mismatch, never a partial trail"
    );
    // The statement engine's answer to the same shape — the two engines
    // agree.
    assert_eq!(
        counts(
            "WITH [null] AS q MATCH (x)-[q*1..2]->(y) RETURN count(*) AS c",
            g
        ),
        vec![0],
        "the statement walk must not enumerate past a malformed list"
    );
}

#[test]
fn a_null_prebound_var_length_rel_matches_nothing_beside_its_subquery_parity() {
    let g = &chain();
    // A failed OPTIONAL MATCH pre-binds its var-length rel variable to
    // null; the comprehension reconciles against the null — the classic
    // family's zero-row rule — instead of rebinding it freely.
    assert_eq!(
        counts(
            "OPTIONAL MATCH (a)-[q:X*1..2]->(z) WITH q \
             RETURN size([(x)-[q*1..2]->(y) | y]) AS c",
            g,
        ),
        vec![0],
        "a null pre-binding is a mismatch, not a fresh slot"
    );
    // The var-length slot over a single-rel outer binding is the one
    // reconciliation shape the collect-subquery meaning does not carry
    // (ADR-0685): a MATCH var-length rel slot may only rebind an outer
    // list binding, exactly as a user-written `collect { MATCH … }` —
    // so the comprehension answers the same plan-time error, not the
    // old walk's null reconciliation.
    let error = match crate::exec::test_support::try_exec_cypher(
        g,
        "MATCH (x:A) OPTIONAL MATCH (x)-[r:R]->(y:B) WHERE y.id = 99 \
         RETURN size([(x)-[r*1..2]->(z) | z]) AS c",
    ) {
        Err(error) => error.to_string(),
        Ok(_) => panic!("a var-length slot over a single-rel binding errors"),
    };
    assert_eq!(
        error, "syntax error: GroupVariableReuse: `r` is already bound",
        "subquery parity: the rebinding is rejected, not reconciled"
    );
    // The single-hop spelling of the same null, unchanged beside the
    // var-length pins: the family's one rule, both trail shapes.
    assert_eq!(
        counts(
            "MATCH (x:A) OPTIONAL MATCH (x)-[r:R]->(y:B) WHERE y.id = 99 \
             RETURN size([(x)-[r]->(z) | z]) AS c",
            g,
        ),
        vec![0],
        "single-hop null stays matches-nothing"
    );
}

#[test]
fn a_prebound_fixed_length_multi_hop_list_pins_the_exact_trail() {
    let g = &chain();
    // A fixed-length multi-hop trail binds the same list value a
    // var-length one does (`*2..2`, two trails in the chain), so the
    // list reconciliation covers it — the arm that used to overwrite
    // every multi-hop binding, var-length or not.
    let sizes = counts(
        "MATCH (a)-[t:R*2..2]->(c) WITH t \
         RETURN size([(x)-[t*2..2]->(y) | y]) AS c",
        g,
    );
    assert_eq!(sizes, vec![1, 1], "each bound two-hop trail pins itself");
}
