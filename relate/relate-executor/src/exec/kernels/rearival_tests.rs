//! The re-arrival rule's suite (ADR-0478 §3): the pool-exactness pins —
//! the default pool's trail proofs and the walk pool's below-min
//! candidate rule. The ground truth is the engine itself —
//! no frontier wiring exists yet (`rider-integration` is this feature's
//! later node), so `exec_cypher` runs the per-row DFS walker, whose
//! default pool enumerates exactly the trails the rule must reproduce
//! and whose REPEATABLE ELEMENTS pool enumerates exactly the walks the
//! walk-length check must reproduce.
//! The differential-suite node later re-runs the same shapes through
//! the wired seam; these pins hold the rule to the engine's answers
//! now, at the seam the runner actually calls.
//!
//! The pins:
//!
//! - **the differential** — generated multigraphs (parallel rels,
//!   self-loops, mixed directions, both types), every bounded pattern
//!   vocabulary shape, every seed: default-pool endpoint sets equal the
//!   DFS engine's `RETURN DISTINCT y` exactly, and walk-pool endpoint
//!   sets equal the engine's REPEATABLE ELEMENTS `RETURN DISTINCT y`
//!   exactly. Unbounded shapes are
//!   pinned on hand-built graphs below, not generated ones: the DFS's
//!   own unbounded trail enumeration is the PathDB timeout class, and a
//!   generated dense multigraph reproduces that blowup — while below
//!   the engine's rel-count cap the rule's depth ceiling cannot bite,
//!   so the bounded vocabulary covers the same rule semantics (and the
//!   parser rejects unbounded REPEATABLE ELEMENTS outright, so the walk
//!   pool's unbounded runs have no engine oracle at all).
//! - **the measured case** — the `knows{1,2}` SF0.1 divergence shape
//!   (engine-trail 855 vs frontier-walk 856, the delta being the seed
//!   re-reached by a rel-reusing 2-walk;
//!   the frontier-NFA design's §1 rule) on the
//!   hand-built graph that reproduces it: the default pool answers
//!   855-shape (the seed excluded), the walk pool 856-shape.
//! - **remedy (b)'s counterexample** — why the cheaper parallel-rel
//!   formulation was declined (type-blind: a same-pair rel of a type
//!   the transition cannot traverse repairs nothing).
//! - **the first-arrival divergence** — the design note's "first
//!   arrivals never diverge" measured claim fails for context+round
//!   automata sharing a type: a rel-reusing walk can FIRST-arrive a
//!   fresh (node, state) pair. The chain proof closes it (a
//!   node-repeating representative chain is not a trail proof), pinned
//!   here against the engine.
//! - **the two proofs and the budget corner** — the chain proof admits
//!   with zero search budget; the search proves the parallel-rel
//!   trails; an exhausted budget surfaces `Exhausted` — a tri-state
//!   verdict, never a disproof (ADR-0516) — and flags the run so the
//!   rider wiring declines the clause (the LABS-952 silent wrong answer
//!   was the cut cached as a rejection).

use proptest::{collection, prelude::*};

use relate_core::NodeId;

use super::frontier::arms;
use super::nfa::{compile, Pool};
use super::rearival::{RearivalRule, RuleBuffers, Verdict};
use crate::exec::test_support::{
    build, column, endpoint_nodes, exec_cypher, node_id, patterns, served_frontier_run, Built,
};
use crate::graph::Graph;
use crate::overlay::ExecGraph;
use crate::pattern_engine::PatternEngine;
use crate::test_support::config_with_floor;

/// The engine's distinct-endpoint answer for `body` (the pattern text
/// with its seed anchor): the DFS walker's default pool, deduplicated —
/// the ground truth every default-pool run must reproduce.
fn engine_endpoints(graph: &Graph, body: &str) -> Vec<NodeId> {
    let mut ids = engine_pool_endpoints(graph, "MATCH", body);
    ids.sort_unstable();
    ids
}

/// The engine's distinct-endpoint answer under the REPEATABLE ELEMENTS
/// pool — the ground truth every walk-pool run must reproduce (bounded
/// spellings only: the parser rejects an unbounded quantifier under
/// REPEATABLE ELEMENTS, so no oracle exists for those).
fn engine_walk_endpoints(graph: &Graph, body: &str) -> Vec<NodeId> {
    let mut ids = engine_pool_endpoints(graph, "MATCH REPEATABLE ELEMENTS", body);
    ids.sort_unstable();
    ids
}

/// The engine's distinct-`y` answer for `body` under the match-mode
/// prefix `prefix` — the two pools' shared reader.
fn engine_pool_endpoints(graph: &Graph, prefix: &str, body: &str) -> Vec<NodeId> {
    let result = exec_cypher(graph, &format!("{prefix} {body} RETURN DISTINCT y"));
    column(&result, "y", |v| Some(node_id(v)))
}

/// The frontier's endpoint set for `body` from `seed` in `pool` — the
/// served-run harness plus the endpoint-nodes read, one wrapper over
/// the two.
fn frontier_endpoints(graph: &Graph, body: &str, seed: NodeId, pool: Pool) -> Vec<NodeId> {
    endpoint_nodes(&served_frontier_run(
        &format!("MATCH {body} RETURN y"),
        graph,
        seed,
        pool,
    ))
}

/// The measured divergence shape's hand-built stand-in: a `KNOWS`
/// triangle, no parallel rels — every 2-walk back to the seed reuses
/// its own outgoing rel, so the seed's shortest trail is the empty walk
/// (below `min`), exactly the SF0.1 `knows{1,2}` seed-re-reach class.
fn triangle_graph() -> Built {
    build(
        &[&["N"], &["N"], &["N"]],
        &[(0, 1, "KNOWS"), (1, 2, "KNOWS"), (0, 2, "KNOWS")],
    )
}

/// The measured case (ADR-0478 §3): on GBL SF0.1 `knows{1,2}` the
/// engine's trail answers count 855 distinct endpoints, the frontier's
/// walk answers 856 — the delta is the seed, re-reached at hop 2 by a
/// rel-reusing walk whose shortest trail is the empty walk, below
/// `min`. The GBL store is a machine-local, gitignored 124 MB fixture
/// no CI lane can reach (the frontier suite's substitution precedent),
/// so this pin reproduces the shape on the hand-built triangle: the
/// default pool must answer the engine's 855-shape (seed excluded),
/// the walk pool the 856-shape (seed included) — the delta exactly the
/// seed, at both the bounded and the unbounded bound. The one-off
/// store-scale re-run (the main checkout's checksum-verified SF0.1
/// cache, degree-median Person seed, the spike's own policy) measured
/// the same three numbers on the real store: engine trail 855,
/// default pool with the rule 855, walk pool 856.
#[test]
fn measured_divergence_shape_answers_like_the_dfs_default_pool() {
    let built = triangle_graph();
    let seed = built.nodes[0];

    let bounded = "(x:N {id: 0})-[:KNOWS*1..2]-(y)";
    let engine = engine_endpoints(&built.graph, bounded);
    assert_eq!(engine, vec![built.nodes[1], built.nodes[2]]);
    // 855-shape: the seed is NOT a default-pool endpoint.
    assert_eq!(
        frontier_endpoints(&built.graph, bounded, seed, Pool::Default),
        engine
    );
    // 856-shape: the walk pool's native answer — one endpoint more,
    // and the delta is exactly the seed.
    let walk = frontier_endpoints(&built.graph, bounded, seed, Pool::Walk);
    assert_eq!(walk, vec![seed, built.nodes[1], built.nodes[2]]);

    // The unbounded spelling on the same graph: the seed IS reachable
    // by a trail (0-1-2-0, all three rels distinct), so every pool
    // agrees — the divergence class is bounds-relative, not absolute.
    let unbounded = "(x:N {id: 0})-[:KNOWS*1..]-(y)";
    let engine_unbounded = engine_endpoints(&built.graph, unbounded);
    assert_eq!(engine_unbounded, vec![seed, built.nodes[1], built.nodes[2]]);
    assert_eq!(
        frontier_endpoints(&built.graph, unbounded, seed, Pool::Default),
        engine_unbounded
    );
}

/// Parallel rels close the trail the re-arrival's own walk cannot: two
/// `KNOWS` rels between the seed and node 1 make the 2-walk
/// seed-1-seed a TRAIL (out over one, back over the other), so the
/// seed is a default-pool endpoint of `*2..2` — admitted by the rule's
/// search after the chain proof refuses the reusing representative.
#[test]
fn parallel_rels_close_the_seed_trail_and_admit() {
    let built = build(&[&["N"], &["N"]], &[(0, 1, "KNOWS"), (0, 1, "KNOWS")]);
    let seed = built.nodes[0];
    let body = "(x:N {id: 0})-[:KNOWS*2..2]-(y)";
    let engine = engine_endpoints(&built.graph, body);
    assert_eq!(engine, vec![seed]);
    assert_eq!(
        frontier_endpoints(&built.graph, body, seed, Pool::Default),
        vec![seed]
    );
}

/// A self-loop is a rel like any other: `0-1` plus a self-loop at 1
/// gives the 2-trail 0-1-1 (two distinct rels) but no 2-trail back to
/// the seed — the engine's answer is {1}, and the default pool's too,
/// the re-arrived seed rejected and the re-arrived 1 admitted.
#[test]
fn self_loop_walks_are_trails_only_with_distinct_rels() {
    let built = build(&[&["N"], &["N"]], &[(0, 1, "KNOWS"), (1, 1, "KNOWS")]);
    let seed = built.nodes[0];
    let body = "(x:N {id: 0})-[:KNOWS*2..2]-(y)";
    let engine = engine_endpoints(&built.graph, body);
    assert_eq!(engine, vec![built.nodes[1]]);
    assert_eq!(
        frontier_endpoints(&built.graph, body, seed, Pool::Default),
        vec![built.nodes[1]]
    );
}

/// Remedy (b)'s counterexample — why the design note §7.1's cheaper
/// "≥ 2 distinct rels connect the duplicated rel's pair" check was
/// declined: it is type-blind. Here the re-arrival walk 0-1-2-1 reuses
/// the `KNOWS` rel of the pair (1,2), and a second rel DOES connect
/// that pair — but it is a `LIKES` rel no `KNOWS` transition can
/// traverse, so it repairs nothing: no in-bounds `KNOWS` trail to 1
/// exists (only two `KNOWS` rels are reachable at all), the engine's
/// answer is empty, and (b) would have admitted 1.
#[test]
fn type_blind_parallelism_is_not_a_trail_proof() {
    let built = build(
        &[&["N"], &["N"], &["N"]],
        &[(0, 1, "KNOWS"), (1, 2, "KNOWS"), (1, 2, "LIKES")],
    );
    let seed = built.nodes[0];
    let body = "(x:N {id: 0})-[:KNOWS*3..3]-(y)";
    // No 3-hop KNOWS trail exists at all: both 3-walks (0-1-2-1,
    // 0-1-0-1) reuse a rel.
    assert_eq!(engine_endpoints(&built.graph, body), Vec::<NodeId>::new());
    assert_eq!(
        frontier_endpoints(&built.graph, body, seed, Pool::Default),
        Vec::<NodeId>::new()
    );
    // The walk pool's native answer diverges by exactly the re-arrived
    // node — the shape (b) would have wrongly admitted to the default
    // pool.
    assert_eq!(
        frontier_endpoints(&built.graph, body, seed, Pool::Walk),
        vec![built.nodes[1]]
    );
}

/// The first-arrival divergence the design note's "first arrivals
/// never diverge (a shortest walk is always a trail)" claim misses: a
/// context rel sharing a type with the round it precedes lets a
/// rel-reusing walk FIRST-arrive a fresh (node, state) pair — the seed
/// at hop 2 over its own context rel traversed backwards, and node 1
/// at hop 3 over the round rel twice. Measured against the engine
/// before implementation (the engine answers {2}; the walk pool
/// answers {0, 1, 2}): the chain proof closes the gap — both
/// representatives repeat a node, so neither is a trail proof, and the
/// search finds no trail. The rule runs for every accepting arrival
/// whose representative chain is not node-simple, first or re-.
#[test]
fn first_arrival_through_a_rel_reusing_context_needs_the_rule() {
    let built = build(
        &[&["N"], &["N"], &["N"]],
        &[(0, 1, "KNOWS"), (1, 2, "KNOWS")],
    );
    let seed = built.nodes[0];
    let body = "(x:N {id: 0})-[:KNOWS]->(m)-[:KNOWS*1..]-(y)";
    let engine = engine_endpoints(&built.graph, body);
    assert_eq!(engine, vec![built.nodes[2]]);
    assert_eq!(
        frontier_endpoints(&built.graph, body, seed, Pool::Default),
        vec![built.nodes[2]]
    );
    // The walk pool's native answer carries both divergent endpoints.
    assert_eq!(
        frontier_endpoints(&built.graph, body, seed, Pool::Walk),
        vec![seed, built.nodes[1], built.nodes[2]]
    );
}

/// The chain proof admits without spending any search budget, and the
/// seed's re-arrival is disproven by the trail semantics itself (not
/// merely by a spent budget): the diamond's second arm re-arrives
/// node 3 with a node-simple representative (0-2-3) — a trail, no
/// search needed — while the seed's re-arrival repeats a node and has
/// no 2-trail to find. Under a spent budget the seed's ask cuts the
/// budget (no verdict — the rule's exhaustion flag, ADR-0516); the
/// disproof is the production budget's answer.
#[test]
fn chain_proof_admits_rearrivals_without_a_search() {
    let built = build(
        &[&["N"], &["N"], &["N"], &["N"]],
        &[
            (0, 1, "KNOWS"),
            (0, 2, "KNOWS"),
            (1, 3, "KNOWS"),
            (2, 3, "KNOWS"),
        ],
    );
    let seed = built.nodes[0];
    let body = "(x:N {id: 0})-[:KNOWS*2..2]-(y)";
    let engine = engine_endpoints(&built.graph, body);
    assert_eq!(engine, vec![built.nodes[3]]);
    assert_eq!(
        frontier_endpoints(&built.graph, body, seed, Pool::Default),
        vec![built.nodes[3]]
    );

    // The rule directly, against the run's own tables, with no search
    // budget at all: the diamond re-arrival is admitted by the chain
    // proof; the seed's re-arrival (no 2-trail exists) is denied.
    let cypher = format!("MATCH {body} RETURN y");
    let nfa = compile(&built.graph, &patterns(&cypher)[0], PatternEngine::Auto).unwrap();
    let view = ExecGraph::read(&built.graph);
    let run = served_frontier_run(&cypher, &built.graph, seed, Pool::Default);
    let arms = arms(&view, &nfa);
    let mut broke_bufs = RuleBuffers::new();
    let mut broke =
        RearivalRule::with_budget(&view, &nfa, &arms, seed, Pool::Default, 0, &mut broke_bufs);
    assert!(broke
        .admits(&run.parents(), (built.nodes[1], 0), built.nodes[3])
        .unwrap());
    // The seed's re-arrival under a spent budget: the chain proof
    // refuses, the search cannot run — the cut (the exhaustion flag),
    // never an admission and never a disproof.
    assert!(!broke
        .admits(&run.parents(), (built.nodes[1], 0), seed)
        .unwrap());
    assert!(broke.budget_exhausted());
    // And under the production budget the seed's rejection is the
    // disproof — the trail semantics, not the budget.
    let mut rule_bufs = RuleBuffers::new();
    let mut rule = RearivalRule::new(&view, &nfa, &arms, seed, Pool::Default, &mut rule_bufs);
    assert!(!rule
        .admits(&run.parents(), (built.nodes[1], 0), seed)
        .unwrap());
    assert!(!rule.budget_exhausted());
}

/// The budget corner's tri-state (ADR-0516, LABS-952): an exhausted
/// search budget is no verdict at all — `Exhausted`, never a disproof —
/// and the rule flags the run so the rider wiring declines the clause
/// to the DFS. The silent under-admission was the live wrong answer: a
/// `*k..k` one-state cycle makes every endpoint a below-min candidate,
/// the first hopeless candidate's search drains the shared budget, and
/// the cut cached as a rejection emptied every remaining endpoint's
/// answer. On the parallel-rel graph the production rule admits the
/// seed through its search; a rule denied every expansion surfaces the
/// cut.
#[test]
fn exhausted_budget_surfaces_exhaustion_never_a_disproof() {
    let built = build(&[&["N"], &["N"]], &[(0, 1, "KNOWS"), (0, 1, "KNOWS")]);
    let seed = built.nodes[0];
    let body = "(x:N {id: 0})-[:KNOWS*2..2]-(y)";

    // The production rule: the chain proof refuses (the representative
    // 0-1-0 repeats the seed), the search proves the parallel 2-trail.
    assert_eq!(
        frontier_endpoints(&built.graph, body, seed, Pool::Default),
        vec![seed]
    );
    let cypher = format!("MATCH {body} RETURN y");
    let nfa = compile(&built.graph, &patterns(&cypher)[0], PatternEngine::Auto).unwrap();
    let view = ExecGraph::read(&built.graph);
    let run = served_frontier_run(&cypher, &built.graph, seed, Pool::Default);
    let arms = arms(&view, &nfa);
    let mut rule_bufs = RuleBuffers::new();
    let mut rule = RearivalRule::new(&view, &nfa, &arms, seed, Pool::Default, &mut rule_bufs);
    assert!(rule
        .admits(&run.parents(), (built.nodes[1], 0), seed)
        .unwrap());
    assert!(!rule.budget_exhausted());

    // The same arrival under a spent budget: the cut, never a
    // rejection — and the run is flagged for the decline.
    let mut broke_bufs = RuleBuffers::new();
    let mut broke =
        RearivalRule::with_budget(&view, &nfa, &arms, seed, Pool::Default, 0, &mut broke_bufs);
    assert!(!broke
        .admits(&run.parents(), (built.nodes[1], 0), seed)
        .unwrap());
    assert!(broke.budget_exhausted());
}

/// The memo's half of the tri-state (ADR-0516): a cut search caches as
/// [`Verdict::Exhausted`], so a later ask for the same target under the
/// same run reads the same honest answer — the memo can never turn the
/// cut into a rejection. The spent-budget rule's first ask runs the
/// search and cuts; the admission ask for the same target then reads
/// the memo, surfacing `Exhausted` where the old `Option` memo answered
/// "rejected".
#[test]
fn a_cut_search_memoizes_as_exhausted_never_disproven() {
    let built = build(&[&["N"], &["N"]], &[(0, 1, "KNOWS"), (0, 1, "KNOWS")]);
    let seed = built.nodes[0];
    let body = "(x:N {id: 0})-[:KNOWS*2..2]-(y)";
    let cypher = format!("MATCH {body} RETURN y");
    let nfa = compile(&built.graph, &patterns(&cypher)[0], PatternEngine::Auto).unwrap();
    let view = ExecGraph::read(&built.graph);
    let run = served_frontier_run(&cypher, &built.graph, seed, Pool::Default);
    let arms = arms(&view, &nfa);
    let mut bufs = RuleBuffers::new();
    let mut rule = RearivalRule::with_budget(&view, &nfa, &arms, seed, Pool::Default, 0, &mut bufs);
    // The first ask runs the search; the spent budget cuts it, and the
    // memo records the cut as itself.
    assert_eq!(rule.pool_witness(seed).unwrap(), Verdict::Exhausted);
    // The memo read: the same cut — never a disproof the admission
    // question could reject on.
    assert_eq!(rule.pool_witness(seed).unwrap(), Verdict::Exhausted);
    // The admission ask reads the memo: neither admitted nor rejected —
    // the cut, carried by the rule's exhaustion flag.
    assert!(!rule
        .admits(&run.parents(), (built.nodes[1], 0), seed)
        .unwrap());
    assert!(rule.budget_exhausted());
}

/// The below-min candidate class, exactly as the differential found
/// it (the minimal failing case, kept as the named pin): a `KNOWS`
/// self-loop at the seed (node 1) plus the single `KNOWS` rel 1-0, and
/// a `LIKES` self-loop at 0. The only in-bounds trail to node 0 is
/// `1-(loop)-1-(rel)-0` — two hops, in `[2,3]` — but its final hop
/// departs the SEED pair, which the dedup filter expanded only at
/// level 0: the BFS observes node 0's arrival solely at hop 1, below
/// `min`, and without the candidate admission the default pool
/// answered empty where the engine answers {0}.
///
/// (The same shape is the WALK pool's own version of the gap —
/// `walk_pool_below_min_candidates_answer_through_the_walk_length_check`
/// below.)
#[test]
fn below_min_candidates_cover_trails_the_bfs_cannot_observe() {
    let built = build(
        &[&["N"], &["N"]],
        &[(0, 0, "LIKES"), (0, 1, "KNOWS"), (1, 1, "KNOWS")],
    );
    let seed = built.nodes[1];
    let body = "(x:N {id: 1})-[:KNOWS*2..3]-(y)";
    let engine = engine_endpoints(&built.graph, body);
    assert_eq!(engine, vec![built.nodes[0]]);
    assert_eq!(
        frontier_endpoints(&built.graph, body, seed, Pool::Default),
        vec![built.nodes[0]]
    );
}

/// The WALK pool's below-min candidate rule's named pin (ADR-0478 §3's
/// walk-pool amendment, the minimal failing case): the seed (node 1)
/// carries a `KNOWS` self-loop plus the one outward `KNOWS` rel to 0.
/// The in-bounds walks of `*2..3` to node 0 are `1-(loop)-1-(rel)-0`
/// and longer — every one departing the SEED pair, which the dedup
/// filter expanded only at level 0 — so the BFS observes node 0's
/// arrival solely at hop 1, below `min`. Before the rule the walk pool
/// answered `{1}` where the engine's REPEATABLE ELEMENTS pool answers
/// `{0, 1}` (node 1 via the rel-reusing 2-walk `1-(rel)-0-(rel)-1`,
/// observed in bounds and admitted by the pool's native semantics);
/// the walk-length check admits node 0 and the walk pool answers the
/// engine exactly. The default pool on the same graph answers the
/// engine's default-pool `{0}` — no trail back to the seed exists
/// (every 2-walk to it reuses the one connecting rel or the one
/// self-loop) — the two pools genuinely diverge here, each matching
/// its own engine spelling.
#[test]
fn walk_pool_below_min_candidates_answer_through_the_walk_length_check() {
    let built = build(&[&["N"], &["N"]], &[(1, 0, "KNOWS"), (1, 1, "KNOWS")]);
    let seed = built.nodes[1];
    let body = "(x:N {id: 1})-[:KNOWS*2..3]-(y)";
    assert_eq!(
        engine_walk_endpoints(&built.graph, body),
        vec![built.nodes[0], built.nodes[1]]
    );
    assert_eq!(
        frontier_endpoints(&built.graph, body, seed, Pool::Walk),
        vec![built.nodes[0], built.nodes[1]]
    );
    // The pools' contrast on the same graph, each matching its own
    // engine spelling.
    assert_eq!(engine_endpoints(&built.graph, body), vec![built.nodes[0]]);
    assert_eq!(
        frontier_endpoints(&built.graph, body, seed, Pool::Default),
        vec![built.nodes[0]]
    );
}

/// The walk-length check's budget corner, the trail search's twin
/// tri-state (ADR-0516): an exhausted budget is no verdict —
/// `Exhausted`, never a disproof — and the rule flags the run for the
/// rider wiring's decline. On the pin's graph the production rule
/// admits the candidate node 0 through its search; a rule denied every
/// expansion surfaces the cut for the same candidate.
#[test]
fn exhausted_budget_surfaces_walk_candidate_exhaustion_never_a_disproof() {
    let built = build(&[&["N"], &["N"]], &[(1, 0, "KNOWS"), (1, 1, "KNOWS")]);
    let seed = built.nodes[1];
    let body = "(x:N {id: 1})-[:KNOWS*2..3]-(y)";
    let cypher = format!("MATCH {body} RETURN y");
    let nfa = compile(&built.graph, &patterns(&cypher)[0], PatternEngine::Auto).unwrap();
    let view = ExecGraph::read(&built.graph);
    let arms = arms(&view, &nfa);

    let mut rule_bufs = RuleBuffers::new();
    let mut rule = RearivalRule::new(&view, &nfa, &arms, seed, Pool::Walk, &mut rule_bufs);
    assert!(matches!(
        rule.pool_witness(built.nodes[0]).unwrap(),
        Verdict::Proven(_)
    ));
    assert!(!rule.budget_exhausted());
    let mut broke_bufs = RuleBuffers::new();
    let mut broke =
        RearivalRule::with_budget(&view, &nfa, &arms, seed, Pool::Walk, 0, &mut broke_bufs);
    assert_eq!(
        broke.pool_witness(built.nodes[0]).unwrap(),
        Verdict::Exhausted
    );
    assert!(broke.budget_exhausted());
}

/// The differential corpus's pattern vocabulary: every bounded shape
/// the NFA compiles — flat (min 1 and min 2), exact, alternation,
/// zero-min, the rounds group, directed traversal, and the two
/// context+round shapes whose context rel shares a type with the round
/// (the first-arrival divergence class above). `{seed}` is the anchor
/// substitution; the strategy draws indices below
/// [`VOCAB`]'s length, which is the one fact this table is.
///
/// `pub(super)`: the statement-level differential
/// (`frontier_differential_tests`) runs the same bodies as whole
/// statements — one pattern table, both suites, never a second copy of
/// the corpus vocabulary. The target is labeled `N` — every generated node carries
/// it, so the engine's target filter keeps every endpoint this table's
/// direct-run comparisons read — and the label is the statement
/// differential's engagement premise: a labeled target prices as a
/// measured endpoint estimate, so the cost gate can engage the bounded
/// shapes (a bare `(y)` prices at the unmeasured rank sentinel, and the
/// bounded vocabulary would decline on cost everywhere).
pub(super) const VOCAB: &[&str] = &[
    "(x:N {id: {seed}})-[:KNOWS*1..2]-(y:N)",
    "(x:N {id: {seed}})-[:KNOWS*2..3]-(y:N)",
    "(x:N {id: {seed}})-[:KNOWS*2..2]-(y:N)",
    "(x:N {id: {seed}})-[:KNOWS|LIKES*1..3]-(y:N)",
    "(x:N {id: {seed}})-[:KNOWS|LIKES*2..3]-(y:N)",
    "(x:N {id: {seed}})-[:KNOWS*0..2]-(y:N)",
    "(x:N {id: {seed}})((a)-[:KNOWS]-(b)-[:LIKES]-(c)){1,2}(y:N)",
    "(x:N {id: {seed}})-[:KNOWS]->(m)-[:KNOWS*1..2]-(y:N)",
    "(x:N {id: {seed}})-[:LIKES]->(m)-[:KNOWS|LIKES*1..2]-(y:N)",
    "(x:N {id: {seed}})-[:KNOWS*1..3]->(y:N)",
    // The untyped var-length shapes (LABS-1434): an empty type list
    // compiles as the any-type set, so these run the same differential as
    // the typed ones — the evidence that "any type" is the walker's own
    // answer and not a silently wider match. Undirected and directed.
    //
    // The single untyped hop (`-[]-`) is deliberately absent, and not
    // because of the type list: a pattern with no quantifier is outside
    // this NFA's vocabulary entirely — `compile` requires exactly one
    // top-level `Quantified` element (`exactly_one_quantified`) and
    // declines at that lookup, long before the cost gate. `plain_rel`
    // admits the empty type list for the *clamped inner rel* a
    // var-length hop normalizes into; the standalone one-hop spelling
    // never reaches it.
    "(x:N {id: {seed}})-[*1..2]-(y:N)",
    "(x:N {id: {seed}})-[*1..3]->(y:N)",
];

/// One vocabulary pattern with its seed anchor substituted (the
/// statement differential shares the substitution).
pub(super) fn pattern_body(pattern: usize, seed: usize) -> String {
    VOCAB
        .get(pattern)
        .unwrap_or_else(|| panic!("pattern index {pattern} outside the vocabulary"))
        .replace("{seed}", &seed.to_string())
}

/// One generated case: the node count, the rel edges (two endpoint
/// slots and a kind slot), the seed index, the vocabulary pattern
/// index.
pub(super) type MultigraphCase = (usize, Vec<(usize, usize, usize)>, usize, usize);

/// The generated-multigraph strategy (the shared generator): 3–6
/// nodes, 2–14 rels over six endpoint slots and three kind slots
/// (`KNOWS` twice, `LIKES` once — the duplicate kind slot makes
/// parallel rels as likely as singles), self-loops and parallel rels
/// reachable through the modulo mapping, any seed below the node
/// count, any vocabulary pattern, both rel types always interned (the
/// alternation patterns must compile). Both differentials draw from
/// it — this suite's endpoint sets and the statement differential's
/// whole statements — one generator, never a second copy.
pub(super) fn multigraph_case() -> BoxedStrategy<MultigraphCase> {
    (
        3usize..=6,
        collection::vec((0usize..6, 0usize..6, 0usize..3usize), 2..=14),
        0usize..6,
        0usize..VOCAB.len(),
    )
        .prop_filter("seed within the node count", |&(n, _, seed, _)| seed < n)
        .prop_filter(
            "both rel types interned (the alternation patterns must compile)",
            |(_, edges, _, _): &MultigraphCase| {
                edges.iter().any(|&(_, _, kind)| rel_type(kind) == "KNOWS")
                    && edges.iter().any(|&(_, _, kind)| rel_type(kind) == "LIKES")
            },
        )
        .boxed()
}

/// One generated case's graph: every node labeled `N` with `id: i`,
/// kind < 2 a `KNOWS` rel and kind 2 a `LIKES` rel, endpoints taken
/// modulo the node count — the mapping both differentials build from a
/// drawn case.
pub(super) fn multigraph(n: usize, edges: &[(usize, usize, usize)]) -> Built {
    let next: &[&str] = &["N"];
    let labels = vec![next; n];
    let rels: Vec<(usize, usize, &str)> = edges
        .iter()
        .map(|&(a, b, kind)| (a % n, b % n, rel_type(kind)))
        .collect();
    build(&labels, &rels)
}

/// The rel kind slot's type name — the one kind partition both the
/// graph mapping and the strategy's interned-both-types filter consult:
/// slots 0 and 1 are both `KNOWS` (the duplicate kind slot makes
/// parallel rels as likely as singles), slot 2 `LIKES`.
pub(super) fn rel_type(kind: usize) -> &'static str {
    if kind < 2 {
        "KNOWS"
    } else {
        "LIKES"
    }
}

// The core acceptance: on generated multigraphs — parallel rels,
// self-loops, mixed directions, both types — each pool's endpoint set
// equals the engine's exactly under that pool's own spelling, every
// vocabulary shape, every seed: the default pool against the DFS
// engine's default pool, the walk pool against its REPEATABLE
// ELEMENTS pool. (A plain comment: rustdoc does not document macro
// invocations.)
proptest! {
    #![proptest_config(config_with_floor(256))]
    #[test]
    fn differential_endpoint_sets_match_the_engine_per_pool(
        (n, edges, seed, pattern) in multigraph_case(),
    ) {
        let body = pattern_body(pattern, seed);
        let built = multigraph(n, &edges);
        let anchor = built.nodes[seed];

        let engine = engine_endpoints(&built.graph, &body);
        let default = frontier_endpoints(&built.graph, &body, anchor, Pool::Default);
        prop_assert_eq!(engine, default, "default pool vs engine: {}", body);
        let walk_engine = engine_walk_endpoints(&built.graph, &body);
        let walk = frontier_endpoints(&built.graph, &body, anchor, Pool::Walk);
        prop_assert_eq!(walk_engine, walk, "walk pool vs engine REPEATABLE: {}", body);
    }
}

/// The recycled buffers' per-run verdict isolation (the allocation
/// bound's memo half): two rules over the same [`RuleBuffers`] — the
/// consecutive runs of one clause, different seeds — never share a
/// verdict. The memo is stamped per run, and a verdict is seed-shaped:
/// the first rule's found witness must not answer the second rule's
/// question. On the 3-node chain, seed 0 holds a 2-trail to node 2 and
/// seed 1 holds none (every 2-walk from 1 returns to 1).
#[test]
fn recycled_buffers_keep_verdicts_per_run() {
    let built = build(
        &[&["N"], &["N"], &["N"]],
        &[(0, 1, "KNOWS"), (1, 2, "KNOWS")],
    );
    let nfa = compile(
        &built.graph,
        &patterns("MATCH (x)-[:KNOWS*2..2]-(y)")[0],
        PatternEngine::Auto,
    )
    .unwrap();
    let view = ExecGraph::read(&built.graph);
    let arms = arms(&view, &nfa);

    let mut bufs = RuleBuffers::new();
    let mut from_zero =
        RearivalRule::new(&view, &nfa, &arms, built.nodes[0], Pool::Default, &mut bufs);
    assert!(
        matches!(
            from_zero.pool_witness(built.nodes[2]).unwrap(),
            Verdict::Proven(_)
        ),
        "seed 0 holds the 2-trail 0-1-2"
    );
    let mut from_one =
        RearivalRule::new(&view, &nfa, &arms, built.nodes[1], Pool::Default, &mut bufs);
    assert_eq!(
        from_one.pool_witness(built.nodes[2]).unwrap(),
        Verdict::Disproven,
        "seed 1 holds no 2-trail to node 2 — the first run's verdict never leaks"
    );
}

/// A guard whose evaluation errors is the error, never a rejection: a
/// class-K interior guard whose property map carries a row-invariant,
/// parameter-free value that fails at evaluation (`{k: 1/0}` — the
/// compile admits it) must surface from the re-arrival search exactly
/// as the walker's own `node_ok` raises it. A guard error mapped to
/// "guard fails" prunes the branch and answers `Disproven` — silently
/// dropping endpoints the walker would abort on. Both search drivers
/// carry it: the pool search (the trail policy) and the node-simple
/// mode search.
#[test]
fn a_guard_evaluation_error_surfaces_from_the_search_never_a_disproof() {
    let built = build(
        &[&["N"], &["N"], &["N"]],
        &[(0, 1, "KNOWS"), (1, 2, "KNOWS")],
    );
    let seed = built.nodes[0];
    let cypher = "MATCH (x:N {id: 0}) ((a)-[:KNOWS]->(b:N {k: 1/0})){1,2} (y) RETURN y";
    let nfa = compile(&built.graph, &patterns(cypher)[0], PatternEngine::Nfa)
        .expect("the row-invariant property-map guard compiles forced");
    assert!(
        nfa.state_guards.iter().any(|guards| !guards.is_empty()),
        "the fixture's guard rides a state"
    );
    let view = ExecGraph::read(&built.graph);
    let arms = arms(&view, &nfa);

    let mut bufs = RuleBuffers::new();
    let mut rule = RearivalRule::new(&view, &nfa, &arms, seed, Pool::Default, &mut bufs);
    let error = rule
        .pool_witness(built.nodes[2])
        .expect_err("the trail search raises the guard's evaluation error");
    assert!(
        error.to_string().contains("zero"),
        "the walker's own arithmetic error, got: {error}"
    );
    let error = rule
        .mode_witness::<false>(built.nodes[2])
        .expect_err("the node-simple search raises the guard's evaluation error");
    assert!(
        error.to_string().contains("zero"),
        "the walker's own arithmetic error, got: {error}"
    );
}
