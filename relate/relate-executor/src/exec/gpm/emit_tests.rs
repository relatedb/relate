//! The GPM walk's riders (the LIMIT pull-in and the EXISTS early exit):
//! the LIMIT cap rides the walk — every complete state is offered as it
//! emerges and the walk halts the moment the cap is full, so a bounded
//! LIMIT answers with a strictly smaller hop count while its rows are
//! exactly the truncated full enumeration's; the EXISTS probe rides the
//! same way — the first matching path answers it without evaluating the
//! remaining frontier.
//!
//! The two order-pinning cap tests pin the DFS rider's own order promise
//! ("the rows the materializing walk would emit, in the same order —
//! only sooner", ADR-0475), so they run with the frontier family stood
//! down: those shapes are frontier-eligible (capped / distinct-endpoint
//! over a bounded quantifier), and the engaged production path answers
//! them in BFS-discovery order instead — a different, equally
//! contract-free prefix (ADR-0478 Decision 4). The engaged path's own
//! contract is pinned in `exec/kernels/frontier_rider_tests.rs`. The
//! third cap test (`capped_rows_carry_every_path_name_binding`) runs the
//! frontier engaged: its pin is engine-agnostic — the bindings every
//! name carries, not any engine's enumeration order.

use super::qpp_hops;
use crate::exec::kernels::with_frontier_disabled;
use crate::exec::test_support::{column, exec_cypher, fan_in, path_value, Builder};
use crate::graph::Graph;

/// A small undirected `LINK` ring around a hub: the var-length walks from
/// `s` fan out inside the hop bound, so the full enumeration and any small
/// prefix of it differ by many hops.
fn ring_graph() -> Graph {
    let b = Builder::new();
    let n: Vec<_> = (0..6).map(|i| b.node(&format!("n{i}"), &["Geo"])).collect();
    for i in 0..6 {
        let j = (i + 1) % 6;
        b.rel(n[i], n[j], "LINK", &[]);
        b.rel(n[j], n[i], "LINK", &[]);
    }
    b.done()
}

/// The fixture's liveness floor: the ring enumerates enough paths that
/// a cap's truncation is visible.
fn enumerates_enough(rows: &[Vec<relate_core::Value>]) {
    assert!(
        rows.len() > 10,
        "the fixture enumerates enough paths to make the halt visible: {rows:?}"
    );
}

/// LIMIT rides the quantified walk: the capped run enumerates strictly
/// fewer hops than the full enumeration, and its rows are exactly the
/// full result's truncation — the same deterministic walk order, halted
/// at the cap.
#[test]
fn limit_rides_the_walk_and_answers_the_truncated_enumeration() {
    let graph = ring_graph();
    // The DFS rider's own contract (see the module doc): the frontier
    // stood down, the cap rides the materializing walk.
    with_frontier_disabled(|| {
        let body = |tail: &str| {
            format!(
                "MATCH (x:Geo {{id:'n0'}})-[:LINK*1..3]-(y:Geo) \
                 RETURN x.id AS x, y.id AS y {tail}"
            )
        };
        let full = exec_cypher(&graph, &body("")).rows;
        enumerates_enough(&full);
        let limited = exec_cypher(&graph, &body("LIMIT 3")).rows;
        assert_eq!(
            limited,
            full[..3.min(full.len())].to_vec(),
            "LIMIT keeps exactly the full enumeration's first rows"
        );
        let full_hops = qpp_hops(&graph, &body(""));
        let limited_hops = qpp_hops(&graph, &body("LIMIT 3"));
        eprintln!("HOPS full={full_hops} limit3={limited_hops}");
        assert!(
            limited_hops < full_hops,
            "LIMIT 3 took {limited_hops} hops vs the full enumeration's {full_hops}"
        );

        // SKIP+LIMIT counts both: the cap halts at SKIP+LIMIT produced rows
        // and the projection keeps the skipped suffix.
        let paged = exec_cypher(&graph, &body("SKIP 4 LIMIT 3")).rows;
        assert_eq!(
            paged,
            full[4..7.min(full.len())].to_vec(),
            "SKIP+LIMIT keeps the enumeration's corresponding window"
        );
    });
}

/// A DISTINCT LIMIT rides the walk too (the distinct cap's GPM wiring —
/// pre-unification the QPP path dropped every cap on the floor): the run
/// halts once enough distinct keys exist, and the rows match the
/// uncapped DISTINCT enumeration's truncation. The frontier stood down
/// (see the module doc): the shape is distinct-endpoint eligible, and
/// this test pins the DFS rider's own order and halt.
#[test]
fn distinct_limit_rides_the_walk() {
    let graph = ring_graph();
    with_frontier_disabled(|| {
        let full = exec_cypher(
            &graph,
            "MATCH (x:Geo {id:'n0'})-[:LINK*1..3]-(y:Geo) RETURN DISTINCT y.id AS y",
        )
        .rows;
        let limited = exec_cypher(
            &graph,
            "MATCH (x:Geo {id:'n0'})-[:LINK*1..3]-(y:Geo) RETURN DISTINCT y.id AS y LIMIT 2",
        )
        .rows;
        assert_eq!(
            limited,
            full[..2].to_vec(),
            "DISTINCT LIMIT keeps the distinct enumeration's first rows"
        );
        let full_hops = qpp_hops(
            &graph,
            "MATCH (x:Geo {id:'n0'})-[:LINK*1..3]-(y:Geo) RETURN DISTINCT y.id AS y",
        );
        let limited_hops = qpp_hops(
            &graph,
            "MATCH (x:Geo {id:'n0'})-[:LINK*1..3]-(y:Geo) RETURN DISTINCT y.id AS y LIMIT 2",
        );
        assert!(
            limited_hops < full_hops,
            "the distinct cap halts the walk: {limited_hops} vs {full_hops} hops"
        );
    });
}

/// One run's path-name bindings, the one spelling both runs of the
/// capped-subpath pin read: the `whole`/`sub` columns through the one
/// column reader and the path extractor — a null or non-path cell is
/// loud (the extractor's family contract), and the two columns' values
/// compare (the two names name the same path value).
fn assert_names_bound(result: &super::super::QueryResult) {
    let wholes = column(result, "whole", path_value);
    let subs = column(result, "sub", path_value);
    assert_eq!(wholes, subs, "both names carry the same path value");
}

/// A capped run of a pattern that names a subpath binds **every** name
/// the pattern introduces on its rows: `p = (q = …)` binds both `p` and
/// `q` — the two names name the same path value — whichever engine
/// serves the run (the GPM walk's rider and materializing loop, or the
/// frontier kernel's witness emission). Cypher promises no row order
/// without ORDER BY and the two engines enumerate in different orders
/// (DFS vs BFS levels), so the pin is the bindings and the truncation,
/// not the order: the capped rows are rows of the uncapped enumeration.
#[test]
fn capped_rows_carry_every_path_name_binding() {
    let graph = ring_graph();
    let body = |tail: &str| {
        format!(
            "MATCH p = (q = (x:Geo {{id:'n0'}})-[:LINK*1..3]-(y:Geo)) \
             RETURN x.id AS x, y.id AS y, p AS whole, q AS sub {tail}"
        )
    };
    let full = exec_cypher(&graph, &body(""));
    enumerates_enough(&full.rows);
    assert_names_bound(&full);
    let limited = exec_cypher(&graph, &body("LIMIT 3"));
    assert_eq!(
        limited.rows.len(),
        3,
        "the cap truncates: {:?}",
        limited.rows
    );
    assert_names_bound(&limited);
    for row in &limited.rows {
        assert!(
            full.rows.contains(row),
            "every capped row is a row of the uncapped enumeration: {row:?}"
        );
    }
}

/// The selector-case probe answers from the selector's chosen rows
/// through the residual pass: a `count(DISTINCT …) > 0` fold whose WHERE
/// rejects every row answers **false** even though the pattern itself
/// enumerates paths — the probe never answers on rows the WHERE rejects.
#[test]
fn selector_probe_answers_through_the_residual() {
    let graph = ring_graph();
    let fold = |predicate: &str| {
        format!(
            "MATCH p = ANY (q = (x:Geo {{id:'n0'}})-[:LINK*1..3]-(y:Geo)) \
             WHERE {predicate} \
             RETURN count(DISTINCT x.id) > 0 AS hit"
        )
    };
    // The pattern enumerates paths and an accepting WHERE answers true —
    // the positive control that the false below is the residual's verdict,
    // not a dead route.
    assert_eq!(
        exec_cypher(&graph, &fold("y.id = 'n3'")).rows,
        vec![vec![relate_core::Value::Bool(true)]]
    );
    // Paths exist, the WHERE rejects every one of them: false.
    assert_eq!(
        exec_cypher(&graph, &fold("y.id = 'nope'")).rows,
        vec![vec![relate_core::Value::Bool(false)]],
        "the selector probe must not answer on WHERE-rejected rows"
    );
}

/// The selector-case probe also applies the deferred element predicates:
/// an element predicate the walk could not prove ready (an `EXISTS`
/// whose over-approximated variable set includes subquery-local names)
/// is left to the row-completion sweep, and the probe answering from
/// materialized rows must run that sweep too — not answer on rows the
/// element predicate rejects.
#[test]
fn selector_probe_applies_the_deferred_element_predicates() {
    let graph = ring_graph();
    let fold = |body: &str| {
        format!(
            "MATCH p = ANY (q = (x:Geo {{id:'n0'}} WHERE EXISTS {{ {body} }}) \
             -[:LINK*1..3]-(y:Geo)) \
             RETURN count(DISTINCT x.id) > 0 AS hit"
        )
    };
    // The positive control: an element predicate some row satisfies.
    assert_eq!(
        exec_cypher(&graph, &fold("MATCH (x)-[:LINK]-(z:Geo {id:'n1'})")).rows,
        vec![vec![relate_core::Value::Bool(true)]]
    );
    // Every row fails the element predicate: false, though the pattern
    // itself enumerates paths.
    assert_eq!(
        exec_cypher(&graph, &fold("MATCH (x)-[:LINK]-(z:Geo {id:'nope'})")).rows,
        vec![vec![relate_core::Value::Bool(false)]],
        "the probe must not answer on rows the deferred element predicate rejects"
    );
}

/// The EXISTS probe over a var-length pattern short-circuits: the first
/// matching path answers it, the remaining frontier is never evaluated —
/// strictly fewer hops than the same pattern's full enumeration — and the
/// boolean matches the full-engine spelling.
#[test]
fn exists_probe_short_circuits_the_frontier() {
    let graph = ring_graph();
    let exists = "RETURN EXISTS { MATCH (x:Geo {id:'n0'})-[:LINK*1..3]-(y:Geo {id:'n3'}) } AS hit";
    let rows = exec_cypher(&graph, exists).rows;
    assert_eq!(rows, vec![vec![relate_core::Value::Bool(true)]]);
    // The full enumeration of the very same pattern, for the hop
    // comparison: the probe must not pay it.
    let full_hops = qpp_hops(
        &graph,
        "MATCH (x:Geo {id:'n0'})-[:LINK*1..3]-(y:Geo {id:'n3'}) RETURN count(*) AS n",
    );
    let probe_hops = qpp_hops(&graph, exists);
    eprintln!("HOPS exists={probe_hops} full={full_hops}");
    assert!(full_hops > 0, "the pattern enumerates hops");
    assert!(
        probe_hops < full_hops,
        "the probe took {probe_hops} hops vs the full enumeration's {full_hops}"
    );
    // A miss answers false without enumerating past the frontier either.
    let miss = "RETURN EXISTS { MATCH (x:Geo {id:'n0'})-[:LINK*1..3]-(y:Nope) } AS hit";
    assert_eq!(
        exec_cypher(&graph, miss).rows,
        vec![vec![relate_core::Value::Bool(false)]]
    );
}

/// The probe's verdict is the full engine's: on the fixture's every
/// endpoint pair, the short-circuited EXISTS answers what an uncapped
/// COUNT of the same body answers.
#[test]
fn exists_probe_answers_what_the_engine_answers() {
    let graph = ring_graph();
    for k in 0..6 {
        let exists = format!(
            "RETURN EXISTS {{ MATCH (x:Geo {{id:'n0'}})-[:LINK*1..2]-(y:Geo {{id:'n{k}'}}) }} AS hit"
        );
        let count = format!(
            "MATCH (x:Geo {{id:'n0'}})-[:LINK*1..2]-(y:Geo {{id:'n{k}'}}) \
             RETURN count(*) > 0 AS hit"
        );
        assert_eq!(
            exec_cypher(&graph, &exists).rows,
            exec_cypher(&graph, &count).rows,
            "EXISTS vs the engine's count on n{k}"
        );
    }
}

/// The GPM selector path's half of the cap charge (LABS-1301): a selector
/// the route declines (`SHORTEST 2` — the route takes only the count-1
/// shapes) enumerates through the selector tail, and a `LIMIT` over it
/// used to leave the cap uncharged, so the caller's input-row loop ran
/// every row. The observable is the funnel's node-read count: the capped
/// run resolves the unbound `:B` candidates once, the eager run once per
/// input row.
#[test]
fn selector_limit_stops_the_input_row_loop_on_the_gpm_path() {
    let graph = fan_in(20);

    let q =
        |limit: &str| format!("MATCH (a:A) MATCH p = SHORTEST 2 (a)-[:R*]-(b:B) RETURN p{limit}");
    let _ = crate::ctx::take_node_reads();
    let limited = exec_cypher(&graph, &q(" LIMIT 1"));
    let limited_reads = crate::ctx::take_node_reads();
    let eager = exec_cypher(&graph, &q(""));
    let eager_reads = crate::ctx::take_node_reads();
    assert_eq!(limited.rows.len(), 1);
    assert_eq!(eager.rows.len(), 20);
    // Both runs pay the same one-off `:A` scan; the difference is the
    // unbound `:B` resolution, which the capped run performs for one input
    // row and the eager run for all twenty — the 19 rows the cap never
    // reached.
    assert_eq!(
        eager_reads - limited_reads,
        19,
        "the cap must stop the loop after the first row: \
         {limited_reads} reads vs eager {eager_reads}"
    );
}
