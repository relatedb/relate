//! The frontier family's rider wiring (ADR-0478 §2/§6): the executor
//! plug-in's integration contract — the EXISTS verdict and its
//! nested-evaluation budget tick's symmetry, the capped emission's rider
//! semantics, the distinct-endpoint answer's identity with the DFS, the
//! decline fall-through (identical rows, no engagement), and the
//! PROFILE counters that prove engagement.
//!
//! The statement-level A/B differential (the differential-suite node,
//! a-2) lives in its own module beside this one
//! (`frontier_differential_tests`): the same whole statements run
//! engaged and stood down over the rearival suite's generated
//! multigraphs, a fixture matrix, and hand-built cyclic edge cases.
//!
//! Rows compare as multisets wherever the frontier serves the shape:
//! its emit order is BFS-discovery order, the DFS's is its walk's — a
//! capped prefix and a distinct-endpoint answer are contract-free
//! orderings (ADR-0478 Decision 4), and every consumer above this layer
//! is order-insensitive by contract. The DFS order promise itself stays
//! pinned where it always was, on the stood-down path (`gpm/emit_tests`).

use std::time::Instant;

use relate_core::Value;

use crate::exec::test_support::{
    column, exec_cypher, frontier_decline_of, int_cell, patterns, profiled, same_rows, string_cell,
    try_exec_cypher_on, Builder,
};
use crate::graph::Graph;
use crate::metrics::ClauseMetricsRecord;
use crate::overlay::ExecGraph;
use crate::QueryResult;

use super::super::take_frontier_counters;
use super::frontier_run::single_quantified;
use super::{
    take_kernel_engagements, with_frontier_disabled, with_frontier_table_budget,
    with_kernels_disabled,
};
use crate::frontier_decline::FrontierRunDecline;

/// A small undirected `LINK` ring (the `gpm/emit_tests` fixture's shape):
/// cyclic, so an unbounded quantifier's trail enumeration fans out while
/// the pair set stays tiny — the frontier's whole reason — and small
/// enough (12 rels) that the stood-down DFS's full trail enumeration
/// still finishes, so the differential can run the unbounded spelling
/// both ways. `pub(super)`: the statement differential's ring pins.
pub(super) fn ring_graph() -> Graph {
    let b = Builder::new();
    let n: Vec<_> = (0..6).map(|i| b.node(&format!("n{i}"), &["Geo"])).collect();
    for i in 0..6 {
        let j = (i + 1) % 6;
        b.rel(n[i], n[j], "LINK", &[]);
        b.rel(n[j], n[i], "LINK", &[]);
    }
    b.done()
}

/// A denser cyclic web: every node linked to its two ring successors in
/// both directions (degree 4, 32 rels) — the exponential trail class at
/// a fixture's size. Its unbounded enumeration is far too wide for a
/// stood-down differential (the ADR's 42-timeout class in miniature),
/// so the web's differentials ride the DFS's own early exits (the
/// probe's first hit, the cap rider's halt), never its full walk.
fn web_graph() -> Graph {
    let b = Builder::new();
    let n: Vec<_> = (0..8).map(|i| b.node(&format!("w{i}"), &["Web"])).collect();
    for i in 0..8 {
        for step in 1..=2 {
            let j = (i + step) % 8;
            b.rel(n[i], n[j], "LINK", &[]);
            b.rel(n[j], n[i], "LINK", &[]);
        }
    }
    b.done()
}

/// The engaged run and the stood-down DFS run of one statement, with
/// the row-identity assertion every A/B shares.
fn ab(g: &Graph, cypher: &str) -> (QueryResult, QueryResult) {
    let engaged = exec_cypher(g, cypher);
    let dfs = with_frontier_disabled(|| exec_cypher(g, cypher));
    assert!(
        same_rows(&engaged, &dfs),
        "rows diverge for {cypher}:\n  frontier: {:?}\n  dfs:      {:?}",
        engaged.rows,
        dfs.rows
    );
    (engaged, dfs)
}

/// The A/B differential for a frontier-eligible shape: the frontier
/// must have taken the clause exactly once (the engagement counter and
/// the run counters both say so), and the stood-down DFS answers the
/// same rows. Returns the engaged result. `pub(super)`: the statement
/// differential (`frontier_differential_tests`) pins its hand-built
/// shapes through it.
pub(super) fn differential(g: &Graph, cypher: &str) -> QueryResult {
    let (engaged, _dfs) = ab(g, cypher);
    assert_eq!(
        take_kernel_engagements(),
        1,
        "the frontier engages for {cypher}"
    );
    assert!(
        take_frontier_counters().levels > 0,
        "the frontier ran for {cypher}"
    );
    engaged
}

/// The A/B differential for a declined shape: no kernel took the clause
/// either way (the batch chain and the chain/hub-star families decline
/// these shapes too, so zero engagements pins the walker), the frontier
/// never ran, and the rows are the stood-down DFS's by construction —
/// the fall-through's own proof. Returns the enabled run's result.
/// `pub(super)`: the statement differential (`frontier_differential_tests`)
/// pins its decline shapes through it.
pub(super) fn differential_declined(g: &Graph, cypher: &str) -> QueryResult {
    let (enabled, _dfs) = ab(g, cypher);
    assert_eq!(
        take_kernel_engagements(),
        0,
        "no kernel takes the declined shape {cypher}"
    );
    assert_eq!(
        take_frontier_counters().levels,
        0,
        "the frontier never runs for {cypher}"
    );
    enabled
}

/// (a) An EXISTS over an unbounded cyclic pattern: the frontier answers
/// in milliseconds (the pair-saturation fixpoint, never the trail
/// enumeration), the run counters prove it ran, and the stood-down
/// probe — whose DFS halts at the first matching trail, its own early
/// exit — answers the same boolean.
#[test]
fn exists_over_an_unbounded_cyclic_pattern_answers_fast_and_identically() {
    let g = web_graph();
    let query = "RETURN EXISTS { MATCH (x:Web {id:'w0'})-[:LINK*]-(y:Web {id:'w5'}) } AS hit";
    let t0 = Instant::now();
    let engaged = exec_cypher(&g, query);
    let elapsed = t0.elapsed();
    assert_eq!(engaged.rows, vec![vec![Value::Bool(true)]]);
    assert_eq!(take_kernel_engagements(), 1, "the frontier arm served it");
    assert!(
        take_frontier_counters().levels > 0,
        "the frontier ran to fixpoint"
    );
    // Generous for CI noise; the measured shape is microseconds (the
    // pair set is 8 nodes).
    assert!(
        elapsed.as_millis() < 500,
        "the frontier answered in {elapsed:?}"
    );
    let dfs = with_frontier_disabled(|| exec_cypher(&g, query));
    assert_eq!(
        dfs.rows, engaged.rows,
        "the DFS probe answers the same boolean"
    );
}

/// (a, PROFILE half) The PROFILE record carries the frontier's counters
/// — the operator marker's own trio, on the clause whose arm ran the
/// statement's walk.
#[test]
fn profile_counters_prove_the_frontier_engagement() {
    let g = web_graph();
    let query = "MATCH (x:Web {id:'w0'})-[:LINK*]-(y:Web) RETURN count(DISTINCT y.id) AS c";
    let records = profiled(&g, query);
    let ran: Vec<&ClauseMetricsRecord> = records
        .iter()
        .filter(|record| record.frontier.levels > 0)
        .collect();
    assert_eq!(
        ran.len(),
        1,
        "exactly the MATCH clause's record carries the frontier (of {} records)",
        records.len()
    );
    let record = ran[0];
    assert!(record.frontier.edge_visits > 0, "the BFS visited edges");
    assert!(record.frontier.endpoints > 0, "the BFS accepted endpoints");
    assert!(
        record.reads.neighbor_walks >= record.frontier.edge_visits,
        "the edge visits ride the standard neighbor-walk read kind"
    );
    assert_eq!(record.rows, 8, "the distinct-endpoint answer: every node");
}

/// (b) A capped `RETURN p LIMIT n` over an unbounded cyclic pattern: n
/// valid rows with the frontier engaged — each a complete binding the
/// DFS's pipeline would keep (a trail under the default pool, inside
/// the hop bounds, anchored at the seed), the BFS halting at the
/// filling level.
#[test]
fn capped_emission_over_an_unbounded_cyclic_pattern_returns_valid_rows() {
    let g = web_graph();
    let query = "MATCH p = (x:Web {id:'w0'})-[:LINK*]-(y:Web) RETURN p LIMIT 5";
    let engaged = exec_cypher(&g, query);
    assert_eq!(take_kernel_engagements(), 1, "the frontier engages");
    assert!(
        take_frontier_counters().levels > 0,
        "the frontier ran for the capped emission"
    );
    assert_eq!(engaged.rows.len(), 5, "the cap fills with exactly n rows");
    assert_eq!(engaged.columns, vec!["p".to_string()]);
    for row in &engaged.rows {
        let Value::Path(path) = &row[0] else {
            panic!("the capped consumer binds the path variable: {row:?}");
        };
        assert!(
            !path.rels.is_empty(),
            "the quantifier's min is 1 hop: {row:?}"
        );
        let mut seen = std::collections::HashSet::new();
        assert!(
            path.rels.iter().all(|rel| seen.insert(rel.id)),
            "the default pool answers TRAIL rows: {row:?}"
        );
        assert!(
            path.nodes
                .first()
                .is_some_and(|node| node.id == seed_id(&g, "w0")),
            "the witness chain anchors at the seed: {row:?}"
        );
    }
    // The stood-down DFS cap rider answers the same count — its rows are
    // its own contract-free prefix (ADR-0478 Decision 4), so the count,
    // not the ordering, is the comparable contract.
    let dfs = with_frontier_disabled(|| exec_cypher(&g, query));
    assert_eq!(dfs.rows.len(), 5, "the DFS cap rider also halts at n");
}

/// (c) The distinct-endpoint consumers answer what the DFS answers: the
/// distinct-endpoint aggregate, the plain DISTINCT projection, and the
/// arrival-time fold (`min`) — the bounded spelling and the unbounded
/// one (the ring is small enough for the DFS's full unbounded trail
/// enumeration, so the exponential class runs both ways).
#[test]
fn distinct_endpoint_answers_match_the_dfs() {
    let g = ring_graph();
    let aggregate = differential(
        &g,
        "MATCH (x:Geo {id:'n0'})-[:LINK*1..3]-(y:Geo) RETURN count(DISTINCT y.id) AS c",
    );
    assert_eq!(aggregate.rows, vec![vec![Value::Int(6)]]);
    differential(
        &g,
        "MATCH (x:Geo {id:'n0'})-[:LINK*1..3]-(y:Geo) RETURN DISTINCT y.id AS y",
    );
    differential(
        &g,
        "MATCH (x:Geo {id:'n0'})-[:LINK*1..3]-(y:Geo) RETURN min(y.id) AS m",
    );
    differential(
        &g,
        "MATCH (x:Geo {id:'n0'})-[:LINK*]-(y:Geo) RETURN count(DISTINCT y.id) AS c",
    );
}

/// (d) The decline fall-through: each representative `FrontierDecline`
/// variant keeps the unchanged DFS — identical rows, and no kernel took
/// the clause.
#[test]
fn declined_shapes_fall_through_to_the_unchanged_dfs() {
    let g = ring_graph();
    // `FullEnumeration`: no cap and no distinct/aggregate fold — every
    // row is the answer.
    let full = differential_declined(
        &g,
        "MATCH (x:Geo {id:'n0'})-[:LINK*1..3]-(y:Geo) RETURN x.id AS x, y.id AS y",
    );
    assert!(full.rows.len() > 10, "the fixture enumerates many trails");
    // `OrderedCap`: ORDER BY + LIMIT needs the full candidate set.
    differential_declined(
        &g,
        "MATCH (x:Geo {id:'n0'})-[:LINK*1..3]-(y:Geo) \
         RETURN y.id AS y ORDER BY y.id LIMIT 3",
    );
    // `PathCount`: `count(*)` counts paths, not endpoints.
    differential_declined(
        &g,
        "MATCH (x:Geo {id:'n0'})-[:LINK*1..3]-(y:Geo) RETURN count(*) AS n",
    );
    // `MultiplePatterns`: two patterns — the single-pattern family's
    // decline.
    differential_declined(
        &g,
        "MATCH (x:Geo {id:'n0'})-[:LINK*1..3]-(y:Geo), (z:Geo {id:'n3'}) \
         RETURN count(DISTINCT y.id) AS c",
    );
    // `CarryingRow`: the entering row binds more than the seed-side
    // variable — per-row semantics the single-seed frontier does not
    // reproduce.
    differential_declined(
        &g,
        "UNWIND [0] AS u MATCH (x:Geo {id:'n0'})-[:LINK*1..3]-(y:Geo) \
         RETURN count(DISTINCT y.id) AS c",
    );
    // `Conjunct`: a WHERE outside the closed predicate vocabulary on the
    // boundary nodes.
    differential_declined(
        &g,
        "MATCH (x:Geo {id:'n0'})-[:LINK*1..3]-(y:Geo) \
         WHERE size(y.id) > 1 RETURN count(DISTINCT y.id) AS c",
    );
    // `Mode`: an explicit TRAIL under a non-capped consumer stays
    // per-path.
    differential_declined(
        &g,
        "MATCH p = TRAIL (x:Geo {id:'n0'})-[:LINK*1..3]-(y:Geo) \
         RETURN count(DISTINCT y.id) AS c",
    );
}

/// (e) The nested-evaluation budget tick's symmetry (the
/// `nested_eval_tests` pattern over ADR-0478 §6): a frontier-served
/// EXISTS evaluation is the same nested evaluation the seek arm serves,
/// so it advances the budget exactly once — one tick on the engaged
/// answer, one tick on the stood-down DFS probe, never a tick on the
/// decline.
#[test]
fn frontier_served_exists_walk_ticks_the_budget_once() {
    let g = ring_graph();
    let view = ExecGraph::read(&g);
    let query = "RETURN EXISTS { MATCH (x:Geo {id:'n0'})-[:LINK*]-(y:Geo {id:'n3'}) } AS hit";
    let kernaled = try_exec_cypher_on(&view, query).unwrap();
    assert_eq!(take_kernel_engagements(), 1, "the frontier arm served it");
    assert_eq!(kernaled.rows, vec![vec![Value::Bool(true)]]);
    assert_eq!(view.nested_evals().count(), 1, "one evaluation, one tick");
    // The same shape on the DFS probe arm (the frontier stood down): the
    // counter restarts per statement, so the same single tick.
    let probed = with_frontier_disabled(|| try_exec_cypher_on(&view, query).unwrap());
    assert_eq!(take_kernel_engagements(), 0);
    assert_eq!(probed.rows, vec![vec![Value::Bool(true)]]);
    assert_eq!(view.nested_evals().count(), 1);
}

/// The shared cheap gate (both entries' first structural proof,
/// `frontier_run::single_quantified`): a pattern carrying two top-level
/// quantified groups declines there — before the answer-shape decode or
/// the matcher's compile runs — on the EXISTS arm exactly as on the row
/// rung, and both answer with the DFS.
#[test]
fn multi_quantifier_exists_probe_declines_at_the_cheap_gate() {
    let g = ring_graph();
    let pattern = "(x:Geo {id:'n0'})((a)-[:LINK]-(b))+((c)-[:LINK]-(d))+(y:Geo {id:'n3'})";
    assert!(
        single_quantified(&g, &patterns(&format!("MATCH {pattern} RETURN y"))).is_none(),
        "two top-level quantified groups fail the cheap gate"
    );
    differential_declined(&g, &format!("RETURN EXISTS {{ MATCH {pattern} }} AS hit"));
    differential_declined(
        &g,
        &format!("MATCH {pattern} RETURN count(DISTINCT y) AS c"),
    );
}

/// The seed node's id (the fixtures' `id` property is the lookup key;
/// `id(n)` reads the node id). `pub(super)`: the statement
/// differential's capped pins anchor their witnesses through it.
pub(super) fn seed_id(g: &Graph, name: &str) -> relate_core::NodeId {
    let rows = exec_cypher(g, &format!("MATCH (n {{id:'{name}'}}) RETURN id(n) AS i")).rows;
    match rows.first().and_then(|row| row.first()).and_then(int_cell) {
        Some(id) => id as relate_core::NodeId,
        None => panic!("the fixture's seed lookup: id(n) returned no integer"),
    }
}

/// (a, allocation-ceiling half) The table-allocation ceiling declines
/// fail-through at run time: with the ceiling held at zero — the test
/// seam standing in for a run whose exact table bytes cross the
/// production constant — an otherwise-engaging shape starts its run,
/// crosses the ceiling at the first level boundary, and declines with
/// `FrontierRunDecline::TableBudget`: zero engagements, the rows the
/// stood-down DFS's own.
#[test]
fn a_tables_over_ceiling_clause_declines_to_the_dfs() {
    // The ring, not the web: the decline sends the statement to the DFS,
    // and the ring is the cyclic fixture whose stood-down enumeration
    // still finishes (the web's is the exponential class).
    let g = ring_graph();
    let query = "MATCH (x:Geo {id:'n0'})-[:LINK*]-(y:Geo) RETURN count(DISTINCT y.id) AS c";
    // Unheld: the frontier takes the clause (a fixture's tables are
    // orders under the production constant).
    differential(&g, query);
    assert_eq!(frontier_decline_of(&g, query), None, "no run declined");
    // Held at zero: the run starts, then the same statement answers
    // through the DFS, the run-time decline's own proof.
    with_frontier_table_budget(0, || {
        let engaged = exec_cypher(&g, query);
        assert!(take_frontier_counters().levels > 0, "the frontier ran");
        assert_eq!(take_kernel_engagements(), 0, "no kernel served it");
        assert_eq!(
            frontier_decline_of(&g, query),
            Some(FrontierRunDecline::TableBudget.into()),
            "the typed run-time decline"
        );
        let dfs = with_frontier_disabled(|| exec_cypher(&g, query));
        assert_eq!(engaged.rows, dfs.rows, "the DFS's own rows");
    });
}

/// The LABS-952 repro fixture: a seed `s` with an N-clique (`s→ci` for
/// every i, `ci→cj` for every i≠j — created first), then the tail
/// `s→a`, `a→s`, `s→b`, `b→u` — every rel `:R`. Under `[:R*4..4]` the
/// quantifier compiles to a one-state cycle NFA, so the dedup filter
/// marks every reachable pair at the walk's first levels and EVERY
/// accepting arrival is observed below `min`: every endpoint is a
/// below-min candidate, and the re-arrival rule's shared work budget is
/// the clause's whole exactness. The true `*4..4` endpoint set is the
/// N clique nodes plus `u` (via the 4-trail s→a→s→b→u); `a`, `b`, and
/// `s` hold no 4-trail — the hopeless candidates whose searches drain
/// the budget once N crosses ~28.
fn clique_graph(n: usize) -> Graph {
    let b = Builder::new();
    let s = b.node("s", &["User"]);
    let clique: Vec<_> = (0..n)
        .map(|i| b.node(&format!("c{i}"), &["User"]))
        .collect();
    for &ci in &clique {
        b.rel(s, ci, "R", &[]);
    }
    for (i, &ci) in clique.iter().enumerate() {
        for (j, &cj) in clique.iter().enumerate() {
            if i != j {
                b.rel(ci, cj, "R", &[]);
            }
        }
    }
    let a = b.node("a", &["User"]);
    let tail_b = b.node("b", &["User"]);
    let u = b.node("u", &["User"]);
    b.rel(s, a, "R", &[]);
    b.rel(a, s, "R", &[]);
    b.rel(s, tail_b, "R", &[]);
    b.rel(tail_b, u, "R", &[]);
    b.done()
}

/// (f, ADR-0516) The LABS-952 silent wrong answer, pinned fixed: at
/// N = 28/32/36 the first hopeless candidate's search drains the
/// re-arrival rule's shared budget, so the rung declines mid-clause —
/// the frontier ran (its counters), no kernel served the clause (the
/// rung's engagement counter), the typed decline is
/// `FrontierRunDecline::RearrivalBudget`, and the DFS's exact answer
/// (N+1 endpoints) is what every path returns: the default run, the
/// stood-down frontier, and the all-kernels-off walker.
#[test]
fn clique_exact_quantifier_falls_through_on_the_rearrival_budget() {
    for n in [28usize, 32, 36] {
        // LABS-781: the walker's per-row container charge refuses this
        // multi-hundred-MB scan at the default 256 MiB cap; this test's
        // subject is the frontier's decline mechanics, not the budget,
        // so it runs with the cap explicitly off.
        let mut g = clique_graph(n);
        g.configure_expression_memory_limit(None);
        let count = "MATCH (s:User {id:'s'})-[:R*4..4]->(n:User) RETURN count(DISTINCT n) AS c";
        let engaged = exec_cypher(&g, count);
        assert!(
            take_frontier_counters().levels > 0,
            "the frontier ran for N={n}"
        );
        assert_eq!(
            take_kernel_engagements(),
            0,
            "the rung declined mid-clause for N={n}: no kernel served it"
        );
        assert_eq!(
            frontier_decline_of(&g, count),
            Some(FrontierRunDecline::RearrivalBudget.into()),
            "the typed run-time decline for N={n}"
        );
        assert_eq!(
            engaged.rows,
            vec![vec![Value::Int(n as i64 + 1)]],
            "the exact endpoint count for N={n}"
        );
        let dfs = with_frontier_disabled(|| exec_cypher(&g, count));
        assert_eq!(dfs.rows, engaged.rows, "the stood-down frontier for N={n}");
        let walker = with_kernels_disabled(|| exec_cypher(&g, count));
        assert_eq!(
            walker.rows, engaged.rows,
            "the all-kernels-off walker for N={n}"
        );

        // The collect shape: N+1 distinct endpoint ids, no silent drop.
        let collect =
            "MATCH (s:User {id:'s'})-[:R*4..4]->(n:User) RETURN collect(DISTINCT n.id) AS ids";
        let ids = exec_cypher(&g, collect);
        take_frontier_counters();
        take_kernel_engagements();
        assert_eq!(
            frontier_decline_of(&g, collect),
            Some(FrontierRunDecline::RearrivalBudget.into()),
            "the collect shape declines too for N={n}"
        );
        let Value::List(list) = &ids.rows[0][0] else {
            panic!("collect(DISTINCT n.id) binds a list: {:?}", ids.rows)
        };
        assert_eq!(list.len(), n + 1, "the distinct endpoint ids for N={n}");
        let dfs_ids = with_frontier_disabled(|| exec_cypher(&g, collect));
        assert_eq!(dfs_ids.rows, ids.rows, "the stood-down collect for N={n}");

        // The riding-WHERE shape: the one endpoint past the clique.
        let filtered =
            "MATCH (s:User {id:'s'})-[:R*4..4]->(n:User) WHERE n.id = 'u' RETURN DISTINCT n.id AS id";
        let hit = exec_cypher(&g, filtered);
        take_frontier_counters();
        take_kernel_engagements();
        assert_eq!(
            frontier_decline_of(&g, filtered),
            Some(FrontierRunDecline::RearrivalBudget.into()),
            "the WHERE shape declines too for N={n}"
        );
        assert_eq!(
            column(&hit, "id", string_cell),
            vec!["u".to_string()],
            "u is the one filtered endpoint for N={n}"
        );
        let dfs_hit = with_frontier_disabled(|| exec_cypher(&g, filtered));
        assert_eq!(dfs_hit.rows, hit.rows, "the stood-down WHERE for N={n}");
    }
}

/// (f, the no-regression half, ADR-0516) The same repro shape INSIDE
/// the budget: at N = 8/16/24 every candidate's search completes, so
/// the frontier serves the clause — engaged, no run-time decline, the
/// exact N+1 answer, identical to the stood-down DFS. The fall-through
/// is the budget corner only, never a needless stand-down.
#[test]
fn clique_inside_the_budget_still_frontier_serves() {
    for n in [8usize, 16, 24] {
        let g = clique_graph(n);
        let query = "MATCH (s:User {id:'s'})-[:R*4..4]->(n:User) RETURN count(DISTINCT n) AS c";
        let engaged = differential(&g, query);
        assert_eq!(
            engaged.rows,
            vec![vec![Value::Int(n as i64 + 1)]],
            "the frontier-served exact count for N={n}"
        );
        assert_eq!(
            frontier_decline_of(&g, query),
            None,
            "no budget fall-through for N={n}"
        );
    }
}
