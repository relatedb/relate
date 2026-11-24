//! The frontier row rung's per-push retained-row charge (ADR-0463,
//! LABS-781's MATCH-container protocol, LABS-1077 /
//! OPLANE_REQ-00345609 item 6): the rung's two emission points — the
//! capped consumer's served loop and the distinct-endpoint shapes'
//! `emit_endpoint_row` — charge every row through the one `add_row`
//! spelling ahead of the landing, so a kernel-served MATCH refuses at
//! ~the cap while the rows land, never after the container is whole.
//!
//! The pins mirror the dispatch layer's (#754's,
//! `exec/aggregate_charge_tests.rs`): the typed refusal carrying the
//! crossing row's own figure, the walk stopped mid-emission (the
//! frontier counters prove the refusal fired before the rung served
//! every seed — before this charge the same queries refused only after
//! the full walk, the whole container already landed), and the
//! under-cap differential — identical rows to the stood-down DFS.

use crate::exec::test_support::{
    assert_crossing_element, assert_expression_cap, exec_cypher, same_rows, try_exec_cypher,
    Builder,
};
use crate::graph::Graph;
use crate::QueryResult;

use super::super::take_frontier_counters;
use super::frontier_rider_tests::{differential, ring_graph};
use super::with_frontier_disabled;

/// The cap every under-cap rerun in this file fits in.
const FITTING_CAP: u64 = 20_000_000;

/// The capped-fan pins' one statement, hoisted so a fixture change
/// (labels, quantifier, limit) is one edit.
const CAPPED_FAN_QUERY: &str = "MATCH p = (a:A)-[:E*]->(y:B) RETURN p LIMIT 5000";

/// The capped fan's per-landing slot figure — a two-hop trail's own
/// path value (its node and rel list buffers) plus a generous 4-slot
/// row figure — the one spelling both the crossing bound and the
/// payload-inclusive pin's slot-only total derive from.
fn fan_landing() -> u64 {
    crate::workspace::value_buffer_bytes(3)
        + crate::workspace::value_buffer_bytes(2)
        + crate::workspace::retained_rows_bytes(1, 4)
}

/// One `:A` seed per fan over two `E` layers: an acyclic fan whose
/// unbounded quantifier still engages the rung (the unbounded spine is
/// the cost gate's own engagement) while every seed's trail set stays
/// finite — sixteen two-hop trails per seed, no cycle to enumerate — so
/// forty seeds give the capped emission forty `capped_rows` runs whose
/// counters a mid-emission refusal leaves short.
fn layered_fan() -> Graph {
    let b = Builder::new();
    for s in 0..40 {
        let a = b.node(&format!("a{s}"), &["A"]);
        let ms: Vec<_> = (0..4)
            .map(|m| b.node(&format!("m{s}_{m}"), &["M"]))
            .collect();
        for (i, m) in ms.iter().enumerate() {
            b.edge(a, *m);
            for f in 0..4 {
                let y = b.node(&format!("y{s}_{i}_{f}"), &["B"]);
                b.edge(*m, y);
            }
        }
    }
    b.done()
}

/// The two capped pins' shared spine (LABS-1077): run `query` over
/// `build`'s graph under `cap`, expect the typed refusal carrying the
/// crossing row's own figure (`one_landing`), and prove it fired
/// mid-emission — the walked seeds stopped short of the same
/// statement's unrefused run. Returns the unrefused result for the
/// pin's own assertions.
fn refused_mid_emission(
    build: impl Fn() -> Graph,
    query: &str,
    cap: u64,
    one_landing: u64,
) -> QueryResult {
    let mut g = build();
    g.configure_expression_memory_limit(Some(cap));
    let error = try_exec_cypher(&g, query)
        .err()
        .expect("the kernel-served container must be refused at the cap");
    let used = assert_expression_cap(&error, cap);
    assert_crossing_element(used, cap, one_landing);
    let stopped = take_frontier_counters();
    assert!(
        stopped.levels > 0,
        "the frontier rung was serving the clause"
    );
    let mut g_full = build();
    g_full.configure_expression_memory_limit(Some(FITTING_CAP));
    let full =
        try_exec_cypher(&g_full, query).expect("the same statement under a fitting cap completes");
    let full_counters = take_frontier_counters();
    assert!(
        stopped.edge_visits < full_counters.edge_visits,
        "the refusal must fire before the rung served every seed \
         ({} of {} edge visits)",
        stopped.edge_visits,
        full_counters.edge_visits
    );
    full
}

/// A named `DISTINCT` over the frontier's distinct-endpoint shape folds the
/// endpoint rows into the clause's first-seen pass (ADR-0555 §3), so the
/// projection no longer materializes a per-row container: the ~200-byte
/// string each row would construct never exists as a held container, and
/// the 5 KB cap that refused the materialized form answers the six keys.
/// The frontier still serves the clause, and the answer is the stood-down
/// DFS's.
#[test]
fn a_kernel_served_distinct_endpoint_fold_is_bounded_by_its_keys() {
    let cap = 5_000;
    let pad = "p".repeat(200);
    let query = format!("MATCH (x:Geo)-[:LINK*1..3]-(y:Geo) RETURN DISTINCT y.id + '{pad}' AS k");
    let mut g = ring_graph();
    g.configure_expression_memory_limit(Some(cap));
    let _ = take_frontier_counters();
    let folded = exec_cypher(&g, &query);
    assert_eq!(folded.rows.len(), 6, "the six distinct endpoint keys");
    assert!(
        take_frontier_counters().levels > 0,
        "the frontier rung was serving the clause"
    );
    // The under-cap form answers the stood-down DFS's rows exactly.
    let mut g_dfs = ring_graph();
    g_dfs.configure_expression_memory_limit(Some(FITTING_CAP));
    let dfs = with_frontier_disabled(|| exec_cypher(&g_dfs, &query));
    assert!(
        same_rows(&folded, &dfs),
        "rows diverge for {query}:\n  frontier: {:?}\n  dfs:      {:?}",
        folded.rows,
        dfs.rows
    );
}

/// LABS-1077 (the payload-inclusive bound): the capped emission's
/// retained figure must include the landed rows' own payloads, never
/// the slot buffers alone — the bound the drop-before-charge emission
/// spelling exists for. A cap the slot-only figure fits (640 rows of
/// slot buffers) but the rows' path payloads overflow must refuse:
/// with slot-only accounting the same statement completes, so this pin
/// fails exactly when the landed payloads stop counting.
#[test]
fn a_capped_emission_refuses_at_a_cap_the_slot_only_figure_fits() {
    let slot_only_total = 640 * fan_landing();
    let mut g = layered_fan();
    g.configure_expression_memory_limit(Some(slot_only_total + 30_000));
    assert!(
        try_exec_cypher(&g, CAPPED_FAN_QUERY).is_err(),
        "a cap the slot-only figure fits must still refuse: the landed \
         rows' path payloads count"
    );
}

/// LABS-1077: the capped consumer's rows charge as they land inside the
/// served loop. A `RETURN p LIMIT` over forty seeds' trails crosses a
/// 64 KB cap mid-emission — the same typed refusal with the crossing
/// row's own figure, and the walk stopped before the last seed served.
/// One landed row: a two-hop trail's own path value — its node and rel
/// list buffers — plus a generous 4-slot row figure.
#[test]
fn a_kernel_served_capped_emission_container_is_capped_while_rows_land() {
    refused_mid_emission(layered_fan, CAPPED_FAN_QUERY, 64_000, fan_landing());
}

/// The no-false-positive form: the same capped statement under a cap
/// the container fits in completes with every trail — the frontier
/// served it, and the stood-down DFS answers the identical rows (the
/// rider wiring's own differential carries both proofs beside the
/// row count).
#[test]
fn a_kernel_served_capped_match_under_the_cap_completes_identically_to_the_dfs() {
    let mut g = layered_fan();
    g.configure_expression_memory_limit(Some(FITTING_CAP));
    let engaged = differential(&g, CAPPED_FAN_QUERY);
    assert_eq!(engaged.rows.len(), 640, "every trail of the fan");
}
