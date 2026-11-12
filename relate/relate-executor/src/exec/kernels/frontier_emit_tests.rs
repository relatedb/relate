//! The witness reconstruction and capped emission's suite (ADR-0478
//! Decision 1's output-sensitive half, Decision 4's row order): the
//! rows the emission binds, pinned against the engine itself — the
//! per-row DFS walker, whose complete states are exactly the row
//! shapes the reconstruction must reproduce (`exec_cypher` runs it;
//! no frontier wiring exists yet, `rider-integration` is the feature's
//! later node).
//!
//! The pins:
//!
//! - **row equality** — on graphs where every endpoint has exactly one
//!   path (a chain), the emission's rows are the DFS's rows, byte for
//!   byte and in order, across the flat spine, the rounds group, and
//!   both context sides; on cyclic graphs (path multiplicity), every
//!   emitted row byte-matches a DFS row — per-row validity, the
//!   strongest order-free claim Decision 4 permits.
//! - **per-witness mode checks** — the parallel-rel graph's closed
//!   2-walk: a TRAIL through the default pool (the deferred search
//!   witness), no row under ACYCLIC, the closed row under SIMPLE, the
//!   rel-reusing row under the WALK pool.
//! - **the early-halt rule under candidates** — the pinned decision:
//!   the BFS halts the level the cap fills, dropping below-min
//!   candidates exactly as the DFS's cap rider drops un-enumerated
//!   paths; a natural end answers them and fills a short cap.
//! - **output sensitivity** — the complete graph's factorial trail
//!   count against the run's degree-sum edge visits and `cap × depth`
//!   reconstruction.
//! - **the zero-length endpoint** — the empty path's row, group lists
//!   and all, byte-equal to the DFS's.

use crate::UsedSet;
use std::collections::BTreeMap;

use relate_core::{NodeId, RelId, Value};

use super::frontier::FrontierRun;
use super::frontier_emit::{capped_rows, CappedRun};
use super::frontier_match::{boundary, BoundaryPair, Consumer, FrontierContract};
use super::frontier_tables::fresh_tables;
use super::nfa::{compile, Pool};
use super::program::{NodeTarget, Prog};
use super::rearival::RuleBuffers;
use super::with_rearival_deferred_budget;
use crate::ctx::ExecCtx;
use crate::exec::test_support::{build, exec_cypher, patterns, Built};
use crate::frontier_decline::FrontierRunDecline;
use crate::graph::Graph;
use crate::overlay::ExecGraph;
use crate::pattern_engine::PatternEngine;
use crate::row::Row;

/// The emission's answer for `cypher`'s first pattern from `seed` under
/// `pool`, capped at `want`: the rows (with their used-rel sets) and the
/// run (the halt pins' observability). The contract is hand-built — the
/// matcher arm's own suite pins its gates; this suite pins the emission
/// on the shapes that pass them. The boundary node targets come from the
/// matcher's own `boundary` pair (the one spelling of that fact).
fn capped(
    graph: &Graph,
    cypher: &str,
    seed: NodeId,
    pool: Pool,
    want: usize,
) -> (Vec<(Row, UsedSet)>, FrontierRun) {
    match capped_answer(graph, cypher, seed, pool, want) {
        CappedRun::Served { matched, run } => (matched, run),
        CappedRun::Declined { facts, reason } => {
            panic!("{cypher} should serve, not decline on {reason:?}: {facts:?}")
        }
    }
}

/// `capped`'s raw answer — the declined arm's observability (the
/// decline's own facts and reason), for the tests that pin the decline
/// rather than the served rows.
fn capped_answer(graph: &Graph, cypher: &str, seed: NodeId, pool: Pool, want: usize) -> CappedRun {
    let pattern = &patterns(cypher)[0];
    let nfa = compile(graph, pattern, PatternEngine::Auto)
        .unwrap_or_else(|| panic!("{cypher} should compile"));
    let (start, target) =
        boundary(pattern).unwrap_or_else(|| panic!("{cypher}: the pattern's boundary is nodes"));
    let bare = |node| NodeTarget {
        pattern: node,
        prog: Prog::Bare,
    };
    let contract = FrontierContract {
        pattern,
        nfa,
        start: bare(start),
        target: bare(target),
        consumer: Consumer::Capped { want },
        pair: BoundaryPair::Unconstrained,
        pool,
    };
    let view = ExecGraph::read(graph);
    let mut tables = fresh_tables(&contract.nfa);
    let params = BTreeMap::new();
    let ctx = ExecCtx::new(&view, &params);
    capped_rows(
        ctx,
        seed,
        &contract,
        &Row::new(),
        want,
        &mut tables,
        &mut RuleBuffers::new(),
    )
    .unwrap_or_else(|error| panic!("{cypher} should emit: {error}"))
}

/// The engine's rows for `cypher`, one `Vec<Value>` per row, the named
/// columns in order — the DFS's own emission, the ground truth.
fn engine(graph: &Graph, cypher: &str, names: &[&str]) -> Vec<Vec<Value>> {
    let result = exec_cypher(graph, cypher);
    let indexes: Vec<usize> = names
        .iter()
        .map(|name| {
            result
                .columns
                .iter()
                .position(|column| column == name)
                .unwrap_or_else(|| panic!("{cypher}: no `{name}` column"))
        })
        .collect();
    result
        .rows
        .iter()
        .map(|row| indexes.iter().map(|&i| row[i].clone()).collect())
        .collect()
}

/// The emission's rows as the same shape: the named bindings, in order.
fn emitted(rows: &[(Row, UsedSet)], names: &[&str]) -> Vec<Vec<Value>> {
    rows.iter()
        .map(|(row, _)| {
            names
                .iter()
                .map(|name| {
                    row.get(name)
                        .cloned()
                        .unwrap_or_else(|| panic!("no `{name}` binding in the emitted row"))
                })
                .collect()
        })
        .collect()
}

/// The chain graph — every endpoint exactly one path from node 0, so
/// the DFS's rows and the frontier's witnesses coincide as sets, and
/// (depth order both ways) as sequences.
fn chain_graph() -> Built {
    let next: &[&str] = &["N"];
    let labels = vec![next; 5];
    let rels: Vec<(usize, usize, &str)> = (0..4).map(|i| (i, i + 1, "KNOWS")).collect();
    build(&labels, &rels)
}

/// Row equality against the DFS, in order, across the spine shapes the
/// parser produces: the flat spine (rel list + path variable + the
/// boundary pair), the rounds group (per-round node and rel lists),
/// and both context sides (single rel bindings, the interior context
/// node). The chain's multiplicity is one, so the emission's
/// per-endpoint witnesses are the DFS's paths exactly.
#[test]
fn rows_byte_match_the_dfs_emission_in_order() {
    let built = chain_graph();
    let seed = built.nodes[0];
    let cases: &[(&str, &[&str])] = &[
        (
            "MATCH p = (x:N {id: 0})-[rs:KNOWS*1..3]-(y:N) RETURN p, rs, x, y",
            &["p", "rs", "x", "y"],
        ),
        (
            "MATCH p = (x:N {id: 0})((a)-[r1:KNOWS]-(b)-[r2:KNOWS]-(c)){1,2}(y:N) \
             RETURN p, a, r1, b, r2, c, x, y",
            &["p", "a", "r1", "b", "r2", "c", "x", "y"],
        ),
        (
            "MATCH p = (x:N {id: 0})-[c1:KNOWS]->(m)-[rs:KNOWS*1..2]-(y:N) \
             RETURN p, c1, m, rs, y",
            &["p", "c1", "m", "rs", "y"],
        ),
        (
            "MATCH p = (x:N {id: 0})-[rs:KNOWS*1..2]-()-[t1:KNOWS]->(y:N) \
             RETURN p, rs, t1, y",
            &["p", "rs", "t1", "y"],
        ),
    ];
    for (cypher, names) in cases {
        let (rows, _) = capped(&built.graph, cypher, seed, Pool::Default, 10);
        assert_eq!(
            emitted(&rows, names),
            engine(&built.graph, cypher, names),
            "{cypher}"
        );
    }
}

/// On cyclic graphs — path multiplicity — the per-endpoint emission
/// cannot match the DFS's row multiset (one witness per endpoint, the
/// documented Decision 4 consequence), so the pin is per-row validity:
/// every emitted row byte-matches a DFS row of the same query, and the
/// row count is the endpoint count.
#[test]
fn cyclic_rows_are_valid_dfs_rows() {
    // The probe graph: a KNOWS triangle plus a LIKES leg.
    let built = build(
        &[&["N"], &["N"], &["N"], &["N"]],
        &[
            (0, 1, "KNOWS"),
            (1, 2, "KNOWS"),
            (2, 0, "KNOWS"),
            (2, 3, "LIKES"),
        ],
    );
    let cypher = "MATCH p = (x:N {id: 0})-[rs:KNOWS*1..3]-(y:N) RETURN p, rs, y";
    let (rows, run) = capped(&built.graph, cypher, built.nodes[0], Pool::Default, 10);
    assert_eq!(rows.len(), run.endpoints.len(), "one witness per endpoint");
    let engine = engine(&built.graph, cypher, &["p", "rs", "y"]);
    for row in emitted(&rows, &["p", "rs", "y"]) {
        assert!(
            engine.contains(&row),
            "the emitted row is one of the DFS's: {row:?}"
        );
    }
}

/// The per-witness mode checks, per pool, on the parallel-rel graph's
/// closed 2-walk `0-(r0)-1-(r1)-0` (nodes [0,1,0], rels distinct): a
/// TRAIL under the default pool — the recorded arrival's representative
/// traverses one rel twice, so the row comes from the deferred trail
/// search's own witness — no row under ACYCLIC (the node repeat), the
/// closed row under SIMPLE (the final close back to the seed), and the
/// rel-reusing 2-walk under the WALK pool.
#[test]
fn per_witness_mode_checks_per_pool() {
    let built = build(&[&["N"], &["N"]], &[(0, 1, "KNOWS"), (0, 1, "KNOWS")]);
    let seed = built.nodes[0];
    let body = "(x:N {id: 0})-[rs:KNOWS*2..2]-(y)";

    // The default pool: one row, the closed 2-trail over both
    // parallels — one of the engine's two (the per-endpoint emission).
    let cypher = format!("MATCH p = {body} RETURN p, rs, y");
    let (rows, _) = capped(&built.graph, &cypher, seed, Pool::Default, 10);
    assert_eq!(rows.len(), 1, "the re-reached seed is the one endpoint");
    let engine_rows = engine(&built.graph, &cypher, &["p", "rs", "y"]);
    assert_eq!(engine_rows.len(), 2, "the DFS emits both rel orders");
    assert!(engine_rows.contains(&emitted(&rows, &["p", "rs", "y"])[0]));

    // ACYCLIC: the node repeat rules the closed walk out — the DFS's
    // own answer is empty, and so is the emission's (the deferred
    // node-simple search finds nothing).
    let acyclic = format!("MATCH p = ACYCLIC {body} RETURN p, rs, y");
    assert!(engine(&built.graph, &acyclic, &["p", "rs", "y"]).is_empty());
    let (none, _) = capped(&built.graph, &acyclic, seed, Pool::Default, 10);
    assert!(none.is_empty(), "no acyclic witness exists to emit");

    // SIMPLE: the closed walk is the mode's one exception — the search
    // allows the final close back to the seed.
    let simple = format!("MATCH p = SIMPLE {body} RETURN p, rs, y");
    let (rows, _) = capped(&built.graph, &simple, seed, Pool::Default, 10);
    assert_eq!(rows.len(), 1, "the closed simple walk is emitted");
    let simple_engine = engine(&built.graph, &simple, &["p", "rs", "y"]);
    assert_eq!(simple_engine.len(), 2, "the DFS emits both rel orders");
    assert!(simple_engine.contains(&emitted(&rows, &["p", "rs", "y"])[0]));

    // The WALK pool: rel reuse is legal — the engine's REPEATABLE
    // spelling answers four 2-walks, all closed; the emission's one is
    // among them (whichever rel the BFS's arrival traversed twice).
    let repeatable = format!("MATCH REPEATABLE ELEMENTS p = {body} RETURN p, rs, y");
    let (rows, _) = capped(&built.graph, &repeatable, seed, Pool::Walk, 10);
    assert_eq!(rows.len(), 1);
    let walk_engine = engine(&built.graph, &repeatable, &["p", "rs", "y"]);
    assert_eq!(walk_engine.len(), 4, "every rel pair, both orders");
    assert!(walk_engine.contains(&emitted(&rows, &["p", "rs", "y"])[0]));
}

/// The WALK pool's native witness: the seed re-reached at hop 2 through
/// its own rel traversed back — a row the default pool could never
/// emit, emitted here without a mode check, byte-matching the engine's
/// REPEATABLE spelling.
#[test]
fn walk_pool_emits_the_rel_reusing_witness() {
    let built = build(
        &[&["N"], &["N"], &["N"]],
        &[(0, 1, "KNOWS"), (1, 2, "KNOWS"), (2, 0, "KNOWS")],
    );
    let cypher =
        "MATCH REPEATABLE ELEMENTS p = (x:N {id: 0})-[rs:KNOWS*1..3]-(y:N) RETURN p, rs, y";
    let (rows, _) = capped(&built.graph, cypher, built.nodes[0], Pool::Walk, 10);
    let values = emitted(&rows, &["p", "rs", "y"]);
    // The seed's own row: y is the seed and rs reuses one rel.
    let seed_row = values
        .iter()
        .find(|row| matches!(&row[2], Value::Node(node) if node.id == built.nodes[0]))
        .expect("the re-reached seed is emitted");
    match &seed_row[1] {
        Value::List(rels) => {
            let ids: Vec<RelId> = rels
                .iter()
                .map(|rel| match rel {
                    Value::Relationship(rel) => rel.id,
                    other => panic!("a rel list of rels, not {other:?}"),
                })
                .collect();
            assert_eq!(ids.len(), 2);
            assert_eq!(ids[0], ids[1], "the 2-walk reuses the seed's own rel");
        }
        other => panic!("the rel list binds a list, not {other:?}"),
    }
    // And the row is one of the engine's.
    let engine_rows = engine(&built.graph, cypher, &["p", "rs", "y"]);
    assert!(engine_rows.contains(seed_row));
}

/// The early-halt rule under candidates, the pinned decision: the BFS
/// halts the level the cap fills from observed in-bounds arrivals, and
/// the below-min candidates are dropped — the runner skips its
/// candidate pass, exactly the MatchCap rider's drop of every
/// un-enumerated path. A natural end instead answers the candidates and
/// fills a still-short cap: the deferred endpoints (a mode-check
/// failure, a below-min candidate) take the rule's own search
/// witnesses.
///
/// The graph: a `KNOWS` self-loop at the seed (node 1) hiding the
/// 2-trails `1-(loop)-1-(rel)-0` and `1-(loop)-1-(rel)-3` from the BFS
/// (their final hop departs the seed pair, expanded only at level 0),
/// a plain rel 1-3 with 3-4 beyond it (the early in-bounds endpoint),
/// and 1-0 (the candidate the rule would admit at a natural end).
#[test]
fn cap_early_halt_stops_at_the_filling_level() {
    let built = build(
        &[&["N"], &["N"], &["N"], &["N"], &["N"]],
        &[
            (1, 1, "KNOWS"),
            (1, 0, "KNOWS"),
            (1, 3, "KNOWS"),
            (3, 4, "KNOWS"),
        ],
    );
    let seed = built.nodes[1];
    let cypher = "MATCH p = (x:N {id: 1})-[rs:KNOWS*2..3]-(y:N) RETURN p, rs, y";
    // The engine's full answer: four trails — the two hidden 2-trails,
    // the plain 2-trail, and the 3-trail through the loop.
    let engine_rows = engine(&built.graph, cypher, &["p", "rs", "y"]);
    assert_eq!(engine_rows.len(), 4, "the engine enumerates every trail");

    // Cap 1: the plain 2-trail to node 4 fills it at level 2 — the BFS
    // never expands node 4's pair, the candidate pass never runs (the
    // run's endpoints are the one observed endpoint), and the row is
    // one of the engine's.
    let (rows, run) = capped(&built.graph, cypher, seed, Pool::Default, 1);
    assert_eq!(emitted(&rows, &["p", "rs", "y"]).len(), 1);
    assert!(engine_rows.contains(&emitted(&rows, &["p", "rs", "y"])[0]));
    assert_eq!(run.per_level, vec![1, 2, 1], "halted after level 2");
    assert_eq!(
        run.endpoints.len(),
        1,
        "the candidates are dropped at the halt, the cap rider's own semantics"
    );

    // No cap pressure (want beyond every endpoint): the BFS runs to its
    // natural end, the candidates are answered, and the deferred
    // endpoints emit the rule's own search witnesses — the hop-3
    // arrival's representative reuses the 3-4 rel, and the candidate
    // node 0's observed arrival is below `min`.
    let (rows, run) = capped(&built.graph, cypher, seed, Pool::Default, 10);
    let values = emitted(&rows, &["p", "rs", "y"]);
    assert_eq!(values.len(), 3, "nodes 4, 3, and 0, one row each");
    assert_eq!(run.per_level, vec![1, 2, 1, 0], "the natural fixpoint");
    assert_eq!(run.endpoints.len(), 3);
    for row in &values {
        assert!(
            engine_rows.contains(row),
            "every row is one of the engine's: {row:?}"
        );
    }
}

/// Output sensitivity (Decision 1's `cap × depth`, never per path): on
/// the complete graph K10 the number of in-bounds trails from the seed
/// is at least the 9! = 362,880 node-simple ones, while the capped run
/// performs the degree sum of edge visits (90: the seed's 9, then the
/// 9 first-arrived pairs' 9 each), stops at the filling level, and
/// reconstructs five 2-hop chains.
#[test]
fn output_sensitive_cap_times_depth_never_path_count() {
    let next: &[&str] = &["N"];
    let labels = vec![next; 10];
    let rels: Vec<(usize, usize, &str)> = (0..10)
        .flat_map(|i| (i + 1..10).map(move |j| (i, j, "KNOWS")))
        .collect();
    let built = build(&labels, &rels);
    let cypher = "MATCH p = (x:N {id: 0})-[rs:KNOWS*2..]-(y:N) RETURN p, rs, y";
    let (rows, run) = capped(&built.graph, cypher, built.nodes[0], Pool::Default, 5);
    assert_eq!(rows.len(), 5, "the cap fills at level 2");
    assert_eq!(run.per_level, vec![1, 9, 0], "halted after level 2");
    assert_eq!(
        run.edge_visits, 90,
        "the undirected degree sum, pair-bounded"
    );
    for row in emitted(&rows, &["p", "rs", "y"]) {
        match &row[1] {
            Value::List(rels) if rels.len() == 2 => {}
            other => panic!("a 2-hop witness, not {other:?}"),
        }
    }
}

/// The zero-length endpoint's row: the empty path — a single-node path
/// variable, an empty rel list, the seed as its own target — byte-equal
/// to the DFS's first row, and in order with the 1-hop row behind it.
#[test]
fn zero_length_endpoint_binds_the_empty_path() {
    let built = build(&[&["N"], &["N"]], &[(0, 1, "KNOWS")]);
    let cypher = "MATCH p = (x:N {id: 0})-[rs:KNOWS*0..1]-(y:N) RETURN p, rs, x, y";
    let (rows, _) = capped(&built.graph, cypher, built.nodes[0], Pool::Default, 10);
    assert_eq!(
        emitted(&rows, &["p", "rs", "x", "y"]),
        engine(&built.graph, cypher, &["p", "rs", "x", "y"]),
        "both rows, in the DFS's order"
    );
}

/// The target's admission rides the emission: an endpoint the target's
/// labels reject emits no row, exactly the rows the DFS's own target
/// check keeps.
#[test]
fn target_admission_filters_endpoints() {
    let b = crate::exec::test_support::Builder::new();
    let nodes: Vec<NodeId> = (0..3)
        .map(|i| b.node(&format!("n{i}"), if i == 2 { &["Other"] } else { &["N"] }))
        .collect();
    b.rel(nodes[0], nodes[1], "KNOWS", &[]);
    b.rel(nodes[1], nodes[2], "KNOWS", &[]);
    let graph = b.done();
    let cypher = "MATCH p = (x:N {id: \"n0\"})-[rs:KNOWS*1..2]-(y:Other) RETURN p, rs, y";
    let (rows, _) = capped(&graph, cypher, nodes[0], Pool::Default, 10);
    let values = emitted(&rows, &["p", "rs", "y"]);
    assert_eq!(values.len(), 1, "only the Other-labeled endpoint emits");
    assert!(matches!(&values[0][2], Value::Node(node) if node.id == nodes[2]));
    // The engine's own target check keeps exactly the same path.
    assert_eq!(values, engine(&graph, cypher, &["p", "rs", "y"]));
}

/// The deferred pass's cut carries the cutting rule's tally, not the
/// runner's (the round-5 rust-review finding): on the walk pool's
/// below-min pin, node 0 defers to the pass's walk-length check, and a
/// pass budget of one unit exhausts on its first expansion — the
/// decline's facts must show the pass's own `declined_exhausted` (the
/// runner's instance never exhausted; before the fix the decline
/// shipped the runner's zero).
#[test]
fn a_deferred_cut_carries_the_cutting_rules_tally() {
    let built = build(&[&["N"], &["N"]], &[(1, 0, "KNOWS"), (1, 1, "KNOWS")]);
    let cypher = "MATCH (x:N {id: 1})-[:KNOWS*2..3]-(y) RETURN y";
    let declined = with_rearival_deferred_budget(1, || {
        capped_answer(&built.graph, cypher, built.nodes[1], Pool::Walk, 10)
    });
    match declined {
        CappedRun::Declined {
            facts,
            reason: FrontierRunDecline::RearrivalBudget,
        } => {
            assert!(
                facts.declined_exhausted >= 1,
                "the cutting rule's tally must ride the decline: {facts:?}"
            );
        }
        other => panic!("the drained deferred pass must decline: {other:?}"),
    }
}

/// A served capped run's counters include the deferred pass's work:
/// on the default pool's below-min pin, the runner's candidate
/// admission proves node 0's in-bounds trail through one search and
/// the deferred pass proves it again through its own instance's
/// search — two `search_proven` asks, where before the fix the run's
/// counters carried only the runner's one.
#[test]
fn a_served_capped_run_counts_its_deferred_pass_works() {
    let built = build(
        &[&["N"], &["N"]],
        &[(0, 0, "LIKES"), (0, 1, "KNOWS"), (1, 1, "KNOWS")],
    );
    let cypher = "MATCH (x:N {id: 1})-[:KNOWS*2..3]-(y) RETURN y";
    let (_, run) = capped(&built.graph, cypher, built.nodes[1], Pool::Default, 10);
    assert_eq!(
        run.rule.search_proven, 2,
        "the runner's admission ask and the deferred pass's ask both count: {:?}",
        run.rule
    );
}
