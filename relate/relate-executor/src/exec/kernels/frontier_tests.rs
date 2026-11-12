//! The frontier core's unit suite (ADR-0478 Decision 1): the
//! level-synchronous BFS over the ExecGraph overlay. Every test drives
//! the production stack whole — `nfa::compile` over the front end's own
//! lowered patterns (the `nfa_tests` helpers), `frontier::run` over an
//! `ExecGraph` view — so the runner's semantics are pinned against the
//! compiler's real output, never a hand-built automaton.
//!
//! The counts the assertions pin:
//!
//! - **edge visits** — each incident rel exactly once per endpoint per
//!   traversed direction: undirected is the degree-sum (each rel twice,
//!   once per endpoint; a self-loop once, rel-deduplicated), directed
//!   once from the traversed side. The prototype's SF0.1 measurement of
//!   the same parity (`knows+` = 28,146 edge visits = the undirected
//!   knows degree-sum) is not
//!   reproduced here: the GBL SF0.1 store is a machine-local, gitignored
//!   124 MB fixture no CI lane can reach, so the parity is pinned on
//!   hand-built graphs where the degree-sum is exact.
//! - **accept on arrival before the dedup filter** — the measured
//!   seed-re-reached-at-hop-2 class (the 855-vs-856 divergence).
//! - **pair-saturation fixpoint** — unbounded runs terminate on cyclic
//!   graphs.
//! - **the work budget** — the authored `WalkBudgetExceeded` error
//!   through the test seam, fast, on a graph the production budget
//!   would happily finish.
//! - **overlay visibility** — a write-scratch rel is traversed, a
//!   scratch-deleted one is not, and a base rel committed after the
//!   view's pin is invisible.

use std::collections::BTreeMap;

use relate_core::NodeId;
use relate_error::ErrorKind;
use relate_ir::{Direction, NodePattern, PatternElement};

// `run` is aliased: several tests bind a local `run` (the run's result)
// beside direct calls of the core.
use super::frontier::{run as run_frontier, FrontierRun, WalkBudget, FRONTIER_WALK_BUDGET};
use super::frontier_tables::{fresh_tables, Arrival, Endpoint, Parent, Step};
use super::nfa::{compile, Pool};
use super::rearival::RuleBuffers;
use crate::ctx::ExecCtx;
use crate::exec::test_support::{
    build, column, endpoint_nodes, exec_write, named_node, node_id, path_pattern, patterns,
    quantified, served_frontier_run, var_length_rel, write_scratch, Built,
};
use crate::overlay::ExecGraph;
use crate::pattern_engine::PatternEngine;

/// The hand-built adversarial graph: two parallel `knows` rels between
/// nodes 0 and 1, a `knows` self-loop at 1, a `knows` rel 1→2, and a
/// `likes` rel 2→0 (a type no `knows` pattern may ever visit).
fn parallel_and_self_loop_graph() -> Built {
    build(
        &[&["N"], &["N"], &["N"]],
        &[
            (0, 1, "knows"),
            (0, 1, "knows"),
            (1, 1, "knows"),
            (1, 2, "knows"),
            (2, 0, "likes"),
        ],
    )
}

/// Undirected traversal: each incident rel exactly once per endpoint —
/// the degree-sum. On this graph: the two parallels cost 2 visits each
/// (once from 0, once from 1), the self-loop 1 (both adjacency sides of
/// node 1, rel-deduplicated), the 1→2 rel 2, and the `likes` rel 0. The
/// prototype's SF0.1 parity measurement (`knows+` = 28,146 = the
/// undirected knows degree-sum) is the same count at store scale; the
/// substitution is stated in the module docs.
///
/// The same run pins the unbounded fixpoint (`*1..`, `max = None`): the
/// level loop ends when a level adds no pair, on a cyclic graph.
#[test]
fn undirected_edge_visits_are_the_degree_sum() {
    let built = parallel_and_self_loop_graph();
    let run = served_frontier_run(
        "MATCH p = (x)-[:knows*1..]-(y) RETURN p",
        &built.graph,
        built.nodes[0],
        Pool::Default,
    );
    assert_eq!(run.edge_visits, 7);
    // One pair per (node, state): three nodes, one state, saturated.
    assert_eq!(run.per_level, vec![1, 1, 1, 0]);

    // Endpoints: 1 and 2 at their first hops, then the SEED — re-reached
    // at hop 2 through a parallel rel traversed backwards, in bounds,
    // accept-on-arrival before the dedup filter. The default pool still
    // admits it: the re-arrival rule's chain proof fails (the
    // representative walk 0-1-0 repeats node 0) but its search finds the
    // 2-trail out over one parallel and back over the other — the
    // parallel-rel half of the 855-vs-856 class (rearival_tests pins
    // both halves).
    assert_eq!(
        endpoint_nodes(&run),
        vec![built.nodes[0], built.nodes[1], built.nodes[2]]
    );
    let seed = run
        .endpoints
        .iter()
        .find(|e| e.node == built.nodes[0])
        .expect("the re-reached seed is an endpoint");
    assert_eq!(seed.hops, 2);
    // Which of the two mutually-connecting rels the arrival traversed is
    // the adjacency iteration's order — not this test's subject — so the
    // rel is pinned as present, the predecessor pair exactly.
    assert!(matches!(
        &seed.arrival,
        Arrival::Rel { pred: (node, 0), .. } if *node == built.nodes[1]
    ));
    // The parent table holds the seed pair's FIRST arrival — none, it is
    // the walk's start — which is why the accepting re-arrival's own
    // predecessor rides on the endpoint record.
    assert_eq!(run.parents().parent_of(built.nodes[0], 0), None);
}

/// Directed traversal: each rel exactly once, from the traversed side
/// only. Outgoing from 0 visits 0's two outgoing parallels and 1's
/// self-loop and 1→2 rel (4); the seed is never re-reached — no `knows`
/// rel ends at it. Incoming from 2 visits 2's incoming 1→2 rel and 1's
/// two incoming parallels and self-loop (4), and reaches 0 through a
/// parallel traversed against its direction.
#[test]
fn directed_transitions_visit_each_rel_once_from_the_traversed_side() {
    let built = parallel_and_self_loop_graph();
    let out = served_frontier_run(
        "MATCH p = (x)-[:knows*1..]->(y) RETURN p",
        &built.graph,
        built.nodes[0],
        Pool::Default,
    );
    assert_eq!(out.edge_visits, 4);
    assert_eq!(endpoint_nodes(&out), vec![built.nodes[1], built.nodes[2]]);

    let inc = served_frontier_run(
        "MATCH p = (x)<-[:knows*1..]-(y) RETURN p",
        &built.graph,
        built.nodes[2],
        Pool::Default,
    );
    assert_eq!(inc.edge_visits, 4);
    assert_eq!(endpoint_nodes(&inc), vec![built.nodes[0], built.nodes[1]]);
}

/// Accept-on-arrival, the dedicated pin: a node first visited at an
/// early level and only ACCEPTED at a later hop count in `[min, max]`
/// is an endpoint. Node 1 is first visited at hop 1 (out of bounds for
/// `*2..2`) and never re-arrived in bounds; the seed is first visited
/// at hop 0 and accepted at hop 2.
#[test]
fn accept_on_arrival_marks_a_later_in_bounds_re_reach() {
    let built = build(&[&["N"], &["N"]], &[(0, 1, "knows"), (1, 0, "knows")]);
    let run = served_frontier_run(
        "MATCH p = (x)-[:knows*2..2]-(y) RETURN p",
        &built.graph,
        built.nodes[0],
        Pool::Default,
    );
    // The accepting arrival: the seed re-reached at hop 2 from node 1.
    // Which of the two mutually-connecting rels the arrival and node
    // 1's first arrival traversed is the adjacency iteration's order —
    // not this test's subject — so the rel column is pinned as present,
    // the pair columns exactly.
    let [endpoint] = &run.endpoints[..] else {
        panic!(
            "the re-reached seed is the one endpoint: {:?}",
            run.endpoints
        )
    };
    assert_eq!(
        (
            endpoint.node,
            endpoint.state,
            endpoint.hops,
            matches!(
                endpoint.arrival,
                Arrival::Rel { pred: (node, 0), .. } if node == built.nodes[1]
            )
        ),
        (built.nodes[0], 0, 2, true)
    );
    assert!(
        matches!(endpoint.arrival, Arrival::Rel { .. }),
        "the arrival traversed a rel"
    );
    // Node 1's first arrival is recorded in the parent table (hop 1,
    // out of bounds — an expansion, never an endpoint).
    let parent = run
        .parents()
        .parent_of(built.nodes[1], 0)
        .expect("node 1's first arrival is recorded");
    assert_eq!((parent.node, parent.state), (built.nodes[0], 0));
    assert!(matches!(parent.step, Step::Rel(_)));
    assert_eq!(run.per_level, vec![1, 1, 0]);
}

/// The work budget (Decision 6): the authored typed error through the
/// test seam, raised at a level boundary. The chain 0→…→6 (undirected
/// `knows+`) costs 1 + 2×5 + 1 = 12 visits across levels whose
/// pre-level cumulative counts are 0, 1, 3, 5, 7, 9, 11 — so a budget
/// of 10 fails at the level whose top count is 11 (one level of
/// overshoot, never a fixpoint run), 11 completes, and the production
/// constant finishes the same walk without notice.
#[test]
fn work_budget_fails_fast_with_the_typed_error() {
    let next: &[&str] = &["N"];
    let labels = vec![next; 7];
    let rels: Vec<(usize, usize, &str)> = (0..6).map(|i| (i, i + 1, "knows")).collect();
    let built = build(&labels, &rels);
    let cypher = "MATCH p = (x)-[:knows*1..]-(y) RETURN p";
    let nfa = compile(&built.graph, &patterns(cypher)[0], PatternEngine::Auto).unwrap();
    let view = ExecGraph::read(&built.graph);

    let over = run_frontier(
        ExecCtx::new(&view, &BTreeMap::new()),
        built.nodes[0],
        &nfa,
        WalkBudget::for_tests(10),
        Pool::Default,
        &mut fresh_tables(&nfa),
        &mut RuleBuffers::new(),
    )
    .unwrap_err();
    // The authored error, byte-exact (single-sourced from the constant),
    // and the `Kind:` prefix parsing back to the ADR-0478 row.
    let message = format!(
        "WalkBudgetExceeded: frontier walk exceeds {FRONTIER_WALK_BUDGET} edge \
         visits per run; a frontier that would blow up fails here instead of \
         running to fixpoint (work budget)."
    );
    assert_eq!(over.to_string(), message);
    let payload = over.to_string();
    let (kind, _) = payload
        .split_once(':')
        .expect("the Execution payload is kind-prefixed");
    assert_eq!(
        ErrorKind::from_name(kind),
        Some(ErrorKind::WalkBudgetExceeded)
    );

    // The boundary: 11 (the largest pre-level count) completes; 10 is
    // the smallest budget that fails.
    let boundary = run_frontier(
        ExecCtx::new(&view, &BTreeMap::new()),
        built.nodes[0],
        &nfa,
        WalkBudget::for_tests(11),
        Pool::Default,
        &mut fresh_tables(&nfa),
        &mut RuleBuffers::new(),
    )
    .unwrap()
    .unwrap_served();
    assert_eq!(boundary.edge_visits, 12);
    assert_eq!(boundary.per_level.len(), 8);
    // The production budget is nowhere near a legitimate chain.
    let standard = run_frontier(
        ExecCtx::new(&view, &BTreeMap::new()),
        built.nodes[0],
        &nfa,
        WalkBudget::standard(),
        Pool::Default,
        &mut fresh_tables(&nfa),
        &mut RuleBuffers::new(),
    )
    .unwrap()
    .unwrap_served();
    assert_eq!(standard.edge_visits, 12);
}

/// Overlay visibility: a rel this statement's earlier write created in
/// the scratch is traversed; a scratch-deleted base rel is not; and a
/// base rel committed after the view's snapshot pin is invisible —
/// the write view reads its own writes at its own pin, nothing newer.
/// Run in the WALK pool: visibility is this test's subject, and the
/// pool semantics (which arrivals the default pool admits) is
/// `rearival_tests`' own.
#[test]
fn overlay_scratch_writes_are_traversed_deletions_and_later_commits_are_not() {
    let built = build(&[&["N"], &["N"], &["N"]], &[(0, 1, "T")]);
    let graph = &built.graph;
    let cypher = "MATCH p = (x)-[:T*1..]-(y) RETURN p";
    let nfa = compile(graph, &patterns(cypher)[0], PatternEngine::Auto).unwrap();

    // The same statement's earlier write: a scratch rel between two
    // scratch-created nodes.
    let scratch = write_scratch(graph);
    let created = exec_write(
        graph,
        &scratch,
        "CREATE (x:N)-[:T]->(y:N) RETURN x AS x, y AS y",
    )
    .unwrap();
    let x = *column(&created, "x", |v| Some(node_id(v))).first().unwrap();
    let y = *column(&created, "y", |v| Some(node_id(v))).first().unwrap();
    let view = ExecGraph::write(graph, &scratch);
    let run = run_frontier(
        ExecCtx::new(&view, &BTreeMap::new()),
        x,
        &nfa,
        WalkBudget::standard(),
        Pool::Walk,
        &mut fresh_tables(&nfa),
        &mut RuleBuffers::new(),
    )
    .unwrap()
    .unwrap_served();
    // The scratch rel is visible: y is reached from x — and x re-reaches
    // itself at hop 2 through it, in bounds, accept-on-arrival.
    assert_eq!(endpoint_nodes(&run), vec![x, y]);
    // The scratch rel's id is the write path's allocation — not this
    // test's subject — so the step is pinned by kind, the pair exactly.
    let parent = run
        .parents()
        .parent_of(y, 0)
        .expect("y's first arrival is recorded");
    assert_eq!((parent.node, parent.state), (x, 0));
    assert!(matches!(parent.step, Step::Rel(_)));

    // A base rel committed after the scratch's pin is invisible: the
    // write view's frontier from 0 reaches 1 (pinned-visible) and never
    // 2 (committed after the pin).
    graph.begin_write().unwrap();
    graph
        .create_rel(built.nodes[1], built.nodes[2], "T".into(), BTreeMap::new())
        .unwrap();
    graph.commit_write();
    let pinned = run_frontier(
        ExecCtx::new(&view, &BTreeMap::new()),
        built.nodes[0],
        &nfa,
        WalkBudget::standard(),
        Pool::Walk,
        &mut fresh_tables(&nfa),
        &mut RuleBuffers::new(),
    )
    .unwrap()
    .unwrap_served();
    assert_eq!(
        endpoint_nodes(&pinned),
        vec![built.nodes[0], built.nodes[1]]
    );
    // The read view, pinned after that commit, does reach 2.
    let fresh = ExecGraph::read(graph);
    let visible = run_frontier(
        ExecCtx::new(&fresh, &BTreeMap::new()),
        built.nodes[1],
        &nfa,
        WalkBudget::standard(),
        Pool::Walk,
        &mut fresh_tables(&nfa),
        &mut RuleBuffers::new(),
    )
    .unwrap()
    .unwrap_served();
    assert_eq!(
        endpoint_nodes(&visible),
        vec![built.nodes[0], built.nodes[1], built.nodes[2]]
    );

    // A scratch deletion hides the base rels entirely: no traversal, no
    // visits.
    let deleting = write_scratch(graph);
    exec_write(graph, &deleting, "MATCH ()-[r:T]->() DELETE r").unwrap();
    let deleted_view = ExecGraph::write(graph, &deleting);
    let empty = run_frontier(
        ExecCtx::new(&deleted_view, &BTreeMap::new()),
        built.nodes[0],
        &nfa,
        WalkBudget::standard(),
        Pool::Walk,
        &mut fresh_tables(&nfa),
        &mut RuleBuffers::new(),
    )
    .unwrap()
    .unwrap_served();
    assert!(empty.endpoints.is_empty());
    assert_eq!(empty.edge_visits, 0);
}

/// Epsilon expansion: the unrolled bounded quantifier's optional-copy
/// skips (the shapes the prototype's two families never produce). Outer
/// `{2,4}` rounds of an inner directed `knows*2..3` over the directed
/// chain 0→…→6 compiles to the 13-state NFA whose shape `nfa_tests`
/// pins; its bounds are `{4, 12}`, and a 4-hop walk (2+2) exits each
/// inner round through an epsilon skip — without the closure, node 4
/// would have no in-bounds arrival at all.
#[test]
fn epsilon_skips_reach_the_in_bounds_endpoints() {
    let next: &[&str] = &["N"];
    let labels = vec![next; 7];
    let rels: Vec<(usize, usize, &str)> = (0..6).map(|i| (i, i + 1, "knows")).collect();
    let built = build(&labels, &rels);
    let mut inner = var_length_rel(&["knows"], 2, Some(3));
    inner.direction = Direction::Outgoing;
    let pattern = path_pattern(vec![
        PatternElement::Node(named_node("x")),
        PatternElement::Quantified(quantified(
            vec![
                PatternElement::Node(NodePattern::anonymous()),
                PatternElement::Relationship(inner),
                PatternElement::Node(NodePattern::anonymous()),
            ],
            2,
            Some(4),
        )),
        PatternElement::Node(named_node("y")),
    ]);
    let nfa = compile(&built.graph, &pattern, PatternEngine::Auto).unwrap();
    assert_eq!((nfa.min_hops, nfa.max_hops), (4, Some(12)));
    assert!(!nfa.epsilons.is_empty());

    let view = ExecGraph::read(&built.graph);
    let run = run_frontier(
        ExecCtx::new(&view, &BTreeMap::new()),
        built.nodes[0],
        &nfa,
        WalkBudget::standard(),
        Pool::Default,
        &mut fresh_tables(&nfa),
        &mut RuleBuffers::new(),
    )
    .unwrap()
    .unwrap_served();
    // The directed chain gives every node one distance; the endpoints
    // are the nodes at distance 4..=6 (the chain ends at 6), each first
    // accepted at its own distance. Nodes 1..=3 sit below `min`.
    assert_eq!(
        endpoint_nodes(&run),
        vec![built.nodes[4], built.nodes[5], built.nodes[6]]
    );
    let by_node = |node: NodeId| {
        run.endpoints
            .iter()
            .find(|e| e.node == node)
            .unwrap_or_else(|| panic!("no endpoint for {node}"))
    };
    assert_eq!(by_node(built.nodes[4]).hops, 4);
    assert_eq!(by_node(built.nodes[5]).hops, 5);
    assert_eq!(by_node(built.nodes[6]).hops, 6);
}

/// The zero-length endpoint and the parent table's first-arrival shape:
/// `knows*0..` marks the seed at hop 0 (no predecessor) before the loop
/// runs, then node 1 at hop 1 over the rel.
#[test]
fn zero_length_endpoint_and_first_arrival_parents() {
    let built = build(&[&["N"], &["N"]], &[(0, 1, "knows")]);
    let run = served_frontier_run(
        "MATCH p = (x)-[:knows*0..]->(y) RETURN p",
        &built.graph,
        built.nodes[0],
        Pool::Default,
    );
    assert_eq!(
        run.endpoints,
        vec![
            Endpoint {
                node: built.nodes[0],
                state: 0,
                hops: 0,
                arrival: Arrival::Zero,
            },
            Endpoint {
                node: built.nodes[1],
                state: 0,
                hops: 1,
                arrival: Arrival::Rel {
                    pred: (built.nodes[0], 0),
                    rel: built.rels[0],
                },
            },
        ]
    );
    assert_eq!(run.parents().states(), 1);
    assert_eq!(run.parents().parent_of(built.nodes[0], 0), None);
    assert_eq!(
        run.parents().parent_of(built.nodes[1], 0),
        Some(Parent {
            node: built.nodes[0],
            state: 0,
            step: Step::Rel(built.rels[0]),
        })
    );
    assert_eq!(run.per_level, vec![1, 1, 0]);
}

/// Direction rides per transition, never globally: the concatenation
/// context's undirected `knows` cycle behind a directed `likes` hop.
/// From node 0 the undirected arm reaches 1 both ways and the directed
/// hop then reaches 2; from node 2 (no `knows` at all) nothing is
/// reached.
#[test]
fn direction_rides_per_transition() {
    let built = build(
        &[&["N"], &["N"], &["N"]],
        &[(0, 1, "knows"), (1, 2, "likes")],
    );
    let cypher = "MATCH p = (x)-[:knows*1..]-()-[:likes]->(y) RETURN p";
    let run = served_frontier_run(cypher, &built.graph, built.nodes[0], Pool::Default);
    // Bounds {2, unbounded}: the context hop plus one round.
    assert_eq!(endpoint_nodes(&run), vec![built.nodes[2]]);

    let none = served_frontier_run(cypher, &built.graph, built.nodes[2], Pool::Default);
    assert!(none.endpoints.is_empty());
}

/// The recycling pin (the allocation bound's reset half): a later seed
/// over the SAME recycled tables and rule buffers answers exactly as
/// over fresh ones — `FrontierTables::begin_seed`'s O(1) generation
/// reset retires every record the previous run wrote (bitmasks, marks,
/// pair chains), and the rule's per-run memo clear keeps one seed's
/// memoized verdicts out of the next's. The parallel-and-self-loop graph's default-pool
/// runs consult the re-arrival rule (the seed's re-arrival is admitted
/// by its search), so the recycled memo is exercised across seeds, not
/// just the BFS tables. One run's comparable facts: the endpoints, the
/// level sizes, the edge visits, and the whole parent table.
#[test]
fn recycled_tables_and_buffers_answer_exactly_as_fresh_ones() {
    let built = parallel_and_self_loop_graph();
    let cypher = "MATCH p = (x)-[:knows*1..]-(y) RETURN p";
    let nfa = compile(&built.graph, &patterns(cypher)[0], PatternEngine::Auto).unwrap();
    let view = ExecGraph::read(&built.graph);

    let facts = |run: &FrontierRun, nodes: &[NodeId]| {
        let states = run.parents().states();
        let parents = nodes
            .iter()
            .flat_map(|&node| (0..states).map(move |s| run.parents().parent_of(node, s as u32)))
            .collect::<Vec<_>>();
        (
            run.endpoints.clone(),
            run.per_level.clone(),
            run.edge_visits,
            parents,
        )
    };

    let mut recycled = Vec::new();
    let mut tables = fresh_tables(&nfa);
    let mut bufs = RuleBuffers::new();
    for &seed in &built.nodes {
        let run = run_frontier(
            ExecCtx::new(&view, &BTreeMap::new()),
            seed,
            &nfa,
            WalkBudget::standard(),
            Pool::Default,
            &mut tables,
            &mut bufs,
        )
        .unwrap()
        .unwrap_served();
        recycled.push(facts(&run, &built.nodes));
        tables = run.reclaim();
    }

    for (i, &seed) in built.nodes.iter().enumerate() {
        let fresh = run_frontier(
            ExecCtx::new(&view, &BTreeMap::new()),
            seed,
            &nfa,
            WalkBudget::standard(),
            Pool::Default,
            &mut fresh_tables(&nfa),
            &mut RuleBuffers::new(),
        )
        .unwrap()
        .unwrap_served();
        assert_eq!(
            recycled[i],
            facts(&fresh, &built.nodes),
            "seed {seed} over the recycled tables answers as over fresh ones"
        );
    }
}

// ---------------------------------------------------------------------------
// The forced rung's guard evaluation (ADR-0519's gap classes): an arrival
// failing its guard is not admitted and its (node, state) cell is never
// marked — the pair the DFS would not bind. Both guard families, one test
// each, on hand-built graphs compiled on the forced rung.
// ---------------------------------------------------------------------------

use super::frontier_rider_tests::seed_id;
use crate::exec::test_support::{served_nfa_run, Builder};

/// A per-state label guard (class K): the round-end node must carry the
/// body's last label. The failing arrival at the unlabelled node is not
/// admitted — no endpoint — and its (node, state) cell is never marked,
/// so the node never expands either.
#[test]
fn label_guard_failing_arrival_is_not_admitted_and_never_marked() {
    let mut b = Builder::new();
    let named = |b: &mut Builder, name: &str, labels: &[&str]| {
        b.prop_node(
            name,
            labels,
            &[("id", relate_core::Value::String(name.into()))],
        )
    };
    let start = named(&mut b, "n0", &[]);
    let keep1 = named(&mut b, "keep1", &["Keep"]);
    let drop = named(&mut b, "drop", &[]);
    let keep2 = named(&mut b, "keep2", &["Keep"]);
    b.rel(start, keep1, "B", &[]);
    b.rel(keep1, drop, "B", &[]);
    b.rel(drop, keep2, "B", &[]);
    let graph = b.done();
    let cypher = "MATCH p = (x)((m)-[:B]->(n:Keep)){1,}(y) RETURN p";
    let nfa = compile(&graph, &patterns(cypher)[0], PatternEngine::Nfa).unwrap();
    let states = nfa.states;
    let run = served_nfa_run(&nfa, &graph, seed_id(&graph, "n0"), Pool::Default);
    // The one endpoint: the labelled round end. The unlabelled node is
    // not an endpoint and never becomes one — its cell was never marked,
    // so the walk beyond it never ran (keep2 stays unreached).
    assert_eq!(endpoint_nodes(&run), vec![seed_id(&graph, "keep1")]);
    let drop = seed_id(&graph, "drop");
    assert!(
        (0..states).all(|state| run.parents().parent_of(drop, state).is_none()),
        "the guard-failing arrival's (node, state) cell is never marked"
    );
    let keep2 = seed_id(&graph, "keep2");
    assert!(
        (0..states).all(|state| run.parents().parent_of(keep2, state).is_none()),
        "a node behind the failed guard never expands"
    );
}

/// A group-var predicate guard (class F): the round must satisfy the
/// group's own WHERE, read at the arrival against the referenced
/// variables' last bindings — the round start off the first-arrival
/// chain, the round end off the arrival itself. The failing arrival is
/// not admitted and its cell never marked; the satisfying one is.
#[test]
fn group_predicate_failing_arrival_is_not_admitted_and_never_marked() {
    use relate_core::Value;
    let mut b = Builder::new();
    let scored = |b: &mut Builder, name: &str, k: i64| {
        b.prop_node(
            name,
            &[],
            &[("id", Value::String(name.into())), ("k", Value::Int(k))],
        )
    };
    let start = scored(&mut b, "n0", 1);
    let mid = scored(&mut b, "mid", 2);
    let ok = scored(&mut b, "ok", 3);
    let bad = scored(&mut b, "bad", 0);
    b.rel(start, mid, "B", &[]);
    b.rel(mid, ok, "B", &[]);
    b.rel(mid, bad, "B", &[]);
    let graph = b.done();
    let cypher = "MATCH p = (x)((m)-[:B]->(w)-[:B]->(n) WHERE m.k < n.k){1,}(y) RETURN p";
    let nfa = compile(&graph, &patterns(cypher)[0], PatternEngine::Nfa).unwrap();
    let states = nfa.states;
    let run = served_nfa_run(&nfa, &graph, seed_id(&graph, "n0"), Pool::Default);
    // The satisfying round end is the one endpoint; the failing one
    // (1 < 0) is not admitted.
    let (ok, bad) = (seed_id(&graph, "ok"), seed_id(&graph, "bad"));
    assert_eq!(endpoint_nodes(&run), vec![ok]);
    assert!(
        (0..states).all(|state| run.parents().parent_of(bad, state).is_none()),
        "the predicate-failing arrival's (node, state) cell is never marked"
    );
    // The satisfying round end's cell is marked, its first arrival the
    // mid position's — the predicate read the round start (the seed, k1)
    // off that chain and the arrival (k3) off the hop.
    let parent = run
        .parents()
        .parent_of(ok, 2)
        .expect("the satisfying round end's cell is marked");
    assert_eq!(
        (parent.node, parent.state),
        (seed_id(&graph, "mid"), 3),
        "the close state's first arrival departs the mid position"
    );
    let endpoint = &run.endpoints[0];
    assert_eq!((endpoint.node, endpoint.hops), (ok, 2));
}
