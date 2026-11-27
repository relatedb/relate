//! The per-pattern walk-plan memo (REL8-841): a mid-statement `MATCH`
//! walked once per driving row sets its walk up once per entry schema, so
//! the per-row cost carries no schema interning and the answers are the
//! generic path's.

use relate_core::Value;

use crate::exec::kernels::with_kernels_disabled;
use crate::exec::test_support::{build, exec_cypher};
use crate::row::interned_schema_calls;

/// `people` `:P` nodes, each with one `R` edge to its own `:Q` node.
fn fan(people: usize) -> crate::exec::test_support::Built {
    let labels: Vec<&[&str]> = (0..people)
        .map(|_| &["P"][..])
        .chain((0..people).map(|_| &["Q"][..]))
        .collect();
    let rels: Vec<(usize, usize, &str)> = (0..people).map(|i| (i, people + i, "R")).collect();
    build(&labels, &rels)
}

/// The statement's second `MATCH` runs once per `:P` row; the kernels are
/// stood down so the per-row walker is what runs it.
const STATEMENT: &str = "MATCH (p:P) WITH p MATCH (p)-[:R]->(q:Q) RETURN p, q";

/// The driving clause alone: its own per-row interning (the projection
/// of each `:P` row), the baseline the second `MATCH` is measured against.
const DRIVER_ONLY: &str = "MATCH (p:P) WITH p RETURN p";

/// Schemas interned while answering `cypher` over `people` driving rows,
/// and the number of answer rows.
fn interned_for(cypher: &str, people: usize) -> (usize, usize) {
    let built = fan(people);
    with_kernels_disabled(|| {
        let before = interned_schema_calls();
        let rows = exec_cypher(&built.graph, cypher).rows.len();
        (interned_schema_calls() - before, rows)
    })
}

#[test]
fn the_second_match_adds_no_per_row_interning_over_the_driving_clause() {
    let growth = |cypher: &str| {
        let (few, few_rows) = interned_for(cypher, 8);
        let (many, many_rows) = interned_for(cypher, 80);
        assert_eq!((few_rows, many_rows), (8, 80), "{cypher}");
        many - few
    };
    // The walk's ladder is built once per (entry schema, arm); without the
    // memo it interned four schemas per driving row on this shape.
    assert_eq!(growth(STATEMENT), growth(DRIVER_ONLY));
}

#[test]
fn the_memoized_walk_pairs_each_p_with_its_own_q() {
    const PEOPLE: usize = 20;
    let built = fan(PEOPLE);
    let rows = with_kernels_disabled(|| exec_cypher(&built.graph, STATEMENT).rows);
    assert_eq!(rows.len(), PEOPLE);
    // Every p pairs with its own q (node i with node PEOPLE + i).
    for row in &rows {
        let (Value::Node(p), Value::Node(q)) = (&row[0], &row[1]) else {
            panic!("p and q are nodes: {row:?}");
        };
        assert_eq!(q.id, p.id + PEOPLE as i64, "{row:?}");
    }
}
