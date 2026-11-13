//! The WHERE-bearing plain LIMIT cap (LABS-853): a MATCH-level WHERE no
//! longer disarms the LIMIT pull-in — the clause's filter policy gates
//! the cap's count, so the walk halts once `SKIP+LIMIT` passing rows have
//! been counted and never builds the rider rows the projection would
//! drop. Every test's answer oracle is the eager differential (the same
//! query without the LIMIT, whose truncation prefix is the LIMIT run's
//! answer by the plain cap's contract), and the payload observable is the
//! neighbor-walk counter: an eager build walks every pattern hop, a
//! capped build stops at the hop whose row filled the cap.

use super::test_support::{column, exec_cypher, hub_out, hub_ts_out, int_cell};
use super::*;
use crate::eval::take_candidate_passes;

/// The LIMIT run's answer equals the eager run's truncation prefix —
/// rows and columns both, the byte-identical contract.
fn assert_prefix(limited: &QueryResult, full: &QueryResult, keep: usize) {
    assert_eq!(limited.columns, full.columns);
    assert_eq!(
        limited.rows.as_slice(),
        &full.rows[..keep],
        "LIMIT run must equal the eager prefix"
    );
}

/// The seam shape (LABS-853): a WHERE between MATCH and RETURN used to
/// disarm the plain limit cap, so the walk enumerated every hop and built
/// a rider row for each; the gated count re-arms it — the walk stops once
/// five WHERE-passing rows are counted.
#[test]
fn plain_limit_with_match_where_stops_the_walk() {
    let g = hub_out(80);
    take_neighbor_walks();
    let limited = exec_cypher(
        &g,
        "MATCH (a:A {id: 0})-[:E]->(b:B) WHERE b.k >= 70 RETURN b.k AS k LIMIT 5",
    );
    let walks = take_neighbor_walks();
    let full = exec_cypher(
        &g,
        "MATCH (a:A {id: 0})-[:E]->(b:B) WHERE b.k >= 70 RETURN b.k AS k",
    );
    assert_eq!(full.rows.len(), 10);
    assert_prefix(&limited, &full, 5);
    // The walk's deterministic order runs k-descending here (the stored
    // typed-chunk order, the same order the ordered cap rides), so the
    // five passing rows are the first five hops — the eager build pays 80.
    assert_eq!(walks, 5, "walks={walks}");
}

/// The seam's own shape (LABS-853): most enumerated rows fail the WHERE,
/// and the cap counts only the passing ones — the walk walks past the
/// failures without counting (or storing) them and halts on the row that
/// fills the cap.
#[test]
fn plain_limit_with_sparse_where_counts_only_passing_rows() {
    let g = hub_out(80);
    take_neighbor_walks();
    let limited = exec_cypher(
        &g,
        "MATCH (a:A {id: 0})-[:E]->(b:B) WHERE b.k % 10 = 0 RETURN b.k AS k LIMIT 3",
    );
    let walks = take_neighbor_walks();
    let full = exec_cypher(
        &g,
        "MATCH (a:A {id: 0})-[:E]->(b:B) WHERE b.k % 10 = 0 RETURN b.k AS k",
    );
    assert_eq!(full.rows.len(), 8);
    assert_prefix(&limited, &full, 3);
    // k-descending enumeration: the three passing rows (k = 70, 60, 50)
    // sit at hops 10, 20, 30 — ten failing hops ride between every
    // counted pair, none of them counted, none of them stored.
    assert_eq!(walks, 30, "walks={walks}");
}

/// SKIP+LIMIT pages the same slice the eager build's truncation keeps,
/// stopping at the (SKIP+LIMIT)-th passing row.
#[test]
fn skip_limit_with_match_where_pages_the_eager_slice() {
    let g = hub_out(80);
    take_neighbor_walks();
    let page = exec_cypher(
        &g,
        "MATCH (a:A {id: 0})-[:E]->(b:B) WHERE b.k >= 70 RETURN b.k AS k SKIP 3 LIMIT 4",
    );
    let walks = take_neighbor_walks();
    let full = exec_cypher(
        &g,
        "MATCH (a:A {id: 0})-[:E]->(b:B) WHERE b.k >= 70 RETURN b.k AS k",
    );
    assert_eq!(page.rows.as_slice(), &full.rows[3..7]);
    // k-descending order: the seven counted rows are the first seven hops.
    assert_eq!(walks, 7, "walks={walks}");
}

/// The gate evaluates the WHERE against each drive row's own bindings: a
/// second MATCH feeding rows with different thresholds gets its own
/// verdicts, and the shared cap counts across the drive rows exactly the
/// rows the eager concatenation would keep.
#[test]
fn where_gate_reads_each_drive_row() {
    let g = hub_out(80);
    g.begin_write().unwrap();
    for k in [68, 74] {
        g.create_node(
            vec!["X".into()],
            BTreeMap::from([("k".into(), Value::Int(k))]),
        )
        .unwrap();
    }
    g.commit_write();
    take_neighbor_walks();
    let limited = exec_cypher(
        &g,
        "MATCH (x:X) MATCH (a:A {id: 0})-[:E]->(b:B) WHERE b.k > x.k \
             RETURN x.k AS xk, b.k AS k LIMIT 3",
    );
    let walks = take_neighbor_walks();
    let full = exec_cypher(
        &g,
        "MATCH (x:X) MATCH (a:A {id: 0})-[:E]->(b:B) WHERE b.k > x.k \
             RETURN x.k AS xk, b.k AS k",
    );
    assert_prefix(&limited, &full, 3);
    // The first drive row (x.k = 68) alone fills the cap: k-descending
    // enumeration offers 79, 78, 77 — three passing rows in three hops,
    // and the second drive row's walk never starts.
    assert_eq!(walks, 3, "walks={walks}");
    assert_eq!(
        column(&limited, "xk", int_cell)
            .into_iter()
            .zip(column(&limited, "k", int_cell))
            .collect::<Vec<_>>(),
        vec![(68, 79), (68, 78), (68, 77)]
    );
}

/// A GPM-shape pattern (an inline element predicate routes the one-path
/// match through the GPM engine) under a clause WHERE rides the same
/// gated cap: the GPM `Cap` arm counts through the same tail step.
#[test]
fn gpm_path_with_match_where_stops_the_walk() {
    let g = hub_out(80);
    take_neighbor_walks();
    let limited = exec_cypher(
        &g,
        "MATCH (a:A {id: 0})-[:E]->(b:B WHERE b.k <> -1) WHERE b.k >= 70 \
             RETURN b.k AS k LIMIT 5",
    );
    let walks = take_neighbor_walks();
    let full = exec_cypher(
        &g,
        "MATCH (a:A {id: 0})-[:E]->(b:B WHERE b.k <> -1) WHERE b.k >= 70 \
             RETURN b.k AS k",
    );
    assert_prefix(&limited, &full, 5);
    // The capped build stops instead of walking all 80 hops (the eager
    // build's count) — the halt is the payload deferral.
    assert!(walks < 80, "the capped walk must stop early, walks={walks}");
}

/// The ordered cap still declines a MATCH-level WHERE (its
/// walk-order-equals-sort-order contract keeps its own arming, ADR-0035):
/// the walk enumerates every hop and the projection truncates.
#[test]
fn ordered_limit_with_match_where_still_walks_all() {
    let g = hub_ts_out(80);
    take_neighbor_walks();
    let limited = exec_cypher(
        &g,
        "MATCH (a:A {id: 0})-[t:E]->(b) WHERE b.k >= 70 \
             RETURN t.ts AS ts ORDER BY t.ts DESC LIMIT 5",
    );
    let walks = take_neighbor_walks();
    let full = exec_cypher(
        &g,
        "MATCH (a:A {id: 0})-[t:E]->(b) WHERE b.k >= 70 \
             RETURN t.ts AS ts ORDER BY t.ts DESC",
    );
    assert_eq!(full.rows.len(), 10);
    assert_prefix(&limited, &full, 5);
    assert_eq!(walks, 80, "the ordered cap declines a MATCH WHERE");
}

/// A `WHERE` on the clause after the MATCH (a `WITH … WHERE`) still
/// declines the plain cap: that filter runs on the projected row, after
/// the count's currency — only the MATCH's own WHERE rides the gate.
#[test]
fn with_where_after_match_still_walks_all() {
    let g = hub_out(80);
    take_neighbor_walks();
    let limited = exec_cypher(
        &g,
        "MATCH (a:A {id: 0})-[:E]->(b:B) WITH b.k AS k WHERE k >= 70 RETURN k LIMIT 5",
    );
    let walks = take_neighbor_walks();
    let full = exec_cypher(
        &g,
        "MATCH (a:A {id: 0})-[:E]->(b:B) WITH b.k AS k WHERE k >= 70 RETURN k",
    );
    assert_prefix(&limited, &full, 5);
    assert_eq!(walks, 80, "a following WITH's WHERE still declines");
}

/// The clause filter runs once per offered row (LABS-853): the plain cap
/// rides a filter-only reduce in the walk's sink, so neither the cap's
/// count nor the clause post-filter re-evaluates a row the sink already
/// judged. The walk's residual pushdown drops the failing hops before
/// the sink, so the three offered rows are the three kept ones — one
/// evaluation each (a gate re-running the filter beside the post-filter
/// would count six).
#[test]
fn match_where_runs_once_per_offered_row() {
    let g = hub_out(80);
    take_neighbor_walks();
    take_candidate_passes();
    let limited = exec_cypher(
        &g,
        "MATCH (a:A {id: 0})-[:E]->(b:B) WHERE b.k % 10 = 0 RETURN b.k AS k LIMIT 3",
    );
    let walks = take_neighbor_walks();
    let evaluations = take_candidate_passes();
    assert_eq!(limited.rows.len(), 3);
    assert_eq!(walks, 30, "walks={walks}");
    assert_eq!(evaluations, 3, "evaluations={evaluations}");
}
