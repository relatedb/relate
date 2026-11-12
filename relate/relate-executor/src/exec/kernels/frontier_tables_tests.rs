//! The sparse frontier tables' own pins (ADR-0478 Decision 6's
//! allocation half, the sparse layout): the per-seed reset leaks no
//! state from one seed into the next — at the table level and through a
//! real recycled run — the allocation follows the touched pairs, never
//! the node-id span, and the allocation ceiling's run-time check
//! declines a run whose exact table bytes cross it.

use std::collections::BTreeMap;

use relate_core::NodeId;

use super::frontier::{run as run_frontier, RunOutcome, WalkBudget};
use super::frontier_tables::{fresh_tables, within_table_budget, FrontierTables, Parent, Step};
use super::nfa::{compile, Pool};
use super::rearival::RuleBuffers;
use super::with_frontier_table_budget;
use crate::ctx::ExecCtx;
use crate::exec::test_support::{build, endpoint_nodes, patterns};
use crate::frontier_decline::FrontierRunDecline;
use crate::overlay::ExecGraph;
use crate::pattern_engine::PatternEngine;

/// A rel-step parent from `(node, state)` over rel `rel`.
fn rel_parent(node: NodeId, state: u32, rel: i64) -> Parent {
    Parent {
        node,
        state,
        step: Step::Rel(rel),
    }
}

/// The table-level reset pin: seed 1 writes every kind of per-node and
/// per-pair fact — two states of one node (a chained pair list), an
/// endpoint mark, a candidate mark — and after `begin_seed` seed 2
/// observes none of them: the pairs read unseen, the parents absent,
/// the marks clear, and re-arriving the same pairs records seed 2's own
/// parents, not seed 1's. A reset that kept any record (or any ledger
/// entry reachable from one) fails here.
#[test]
fn begin_seed_leaks_no_parent_mark_or_pair_into_the_next_seed() {
    let mut tables = FrontierTables::new(2);
    // Seed 1: seed 10, then node 20 in both states, node 30 marked.
    assert!(tables.arrive(10, 0, None).expect("the ledger has room").new);
    assert!(
        tables
            .arrive(20, 0, Some(rel_parent(10, 0, 100)))
            .expect("the ledger has room")
            .new
    );
    assert!(
        tables
            .arrive(20, 1, Some(rel_parent(20, 0, 101)))
            .expect("the ledger has room")
            .new
    );
    assert!(
        !tables
            .arrive(20, 1, Some(rel_parent(10, 0, 102)))
            .expect("the ledger has room")
            .new
    );
    tables.mark_endpoint(20);
    tables.mark_candidate(30);
    // The arrival's endpoint fact is the mark as it stood before it.
    assert!(
        tables
            .arrive(20, 0, None)
            .expect("the ledger has room")
            .endpoint
    );
    assert!(
        !tables
            .arrive(10, 0, None)
            .expect("the ledger has room")
            .endpoint
    );
    assert!(tables.pair_seen(20, 1));
    assert_eq!(
        tables.parents().parent_of(20, 1),
        Some(rel_parent(20, 0, 101))
    );
    assert_eq!(
        tables.parents().parent_of(20, 0),
        Some(rel_parent(10, 0, 100))
    );
    assert_eq!(tables.parents().parent_of(10, 0), None);
    assert_eq!(tables.pair_count(), 3);
    assert_eq!(tables.touched_nodes(), 3);

    tables.begin_seed();

    // Seed 2 sees a clean set.
    assert_eq!(tables.pair_count(), 0);
    assert_eq!(tables.touched_nodes(), 0);
    for (node, state) in [(10, 0), (20, 0), (20, 1)] {
        assert!(!tables.pair_seen(node, state), "({node}, {state}) unseen");
        assert_eq!(tables.parents().parent_of(node, state), None);
    }
    assert!(!tables.endpoint_marked(20));
    assert!(!tables.candidate_marked(30));

    // Seed 2 starts from 20 and re-arrives 10: its own parents only.
    assert!(tables.arrive(20, 0, None).expect("the ledger has room").new);
    assert!(
        tables
            .arrive(10, 1, Some(rel_parent(20, 0, 200)))
            .expect("the ledger has room")
            .new
    );
    assert_eq!(tables.parents().parent_of(20, 0), None);
    assert_eq!(tables.parents().parent_of(20, 1), None);
    assert_eq!(
        tables.parents().parent_of(10, 1),
        Some(rel_parent(20, 0, 200))
    );
    assert_eq!(tables.parents().parent_of(10, 0), None);
    assert_eq!(tables.pair_at(0), (20, 0));
    assert_eq!(tables.pair_at(1), (10, 1));
}

/// The run-level reset pin: two seeds `a` and `c` both reach `b`, over
/// ONE recycled table set and rule buffer. Seed `a`'s run marks `b` an
/// endpoint; under a reset that leaked that mark, seed `c`'s
/// accept-on-arrival check would find `b` already marked and drop it —
/// the correct reset answers `b` for `c` too, with `c`'s own parent.
#[test]
fn a_recycled_second_seed_does_not_observe_the_first_seeds_endpoints() {
    let built = build(&[&["N"], &["N"], &["N"]], &[(0, 1, "T"), (2, 1, "T")]);
    let (a, b, c) = (built.nodes[0], built.nodes[1], built.nodes[2]);
    let nfa = compile(
        &built.graph,
        &patterns("MATCH p = (x)-[:T*1..2]->(y) RETURN p")[0],
        PatternEngine::Auto,
    )
    .unwrap();
    let view = ExecGraph::read(&built.graph);
    let mut tables = fresh_tables(&nfa);
    let mut bufs = RuleBuffers::new();
    let mut run_from = |seed: NodeId, tables: FrontierTables| {
        let mut tables = tables;
        run_frontier(
            ExecCtx::new(&view, &BTreeMap::new()),
            seed,
            &nfa,
            WalkBudget::standard(),
            Pool::Default,
            &mut tables,
            &mut bufs,
        )
        .unwrap()
        .unwrap_served()
    };
    let first = run_from(a, std::mem::take(&mut tables));
    assert_eq!(endpoint_nodes(&first), vec![b]);
    tables = first.reclaim();
    let second = run_from(c, std::mem::take(&mut tables));
    assert_eq!(endpoint_nodes(&second), vec![b]);
    let parent = second
        .parents()
        .parent_of(b, second.endpoints[0].state)
        .expect("b's first arrival in c's run has a parent");
    assert_eq!(parent.node, c, "b's parent is c's own arrival, not a's");
    assert_eq!(second.parents().parent_of(a, 0), None, "a is untouched");
}

/// The sparse-sizing pin: tables over node ids near 10⁷ (and past
/// 2³²) with a tiny frontier hold a few hundred bytes — the allocation
/// follows the touched pairs, never the node-id span. The dense layout
/// held `span × states` parent slots, ~170 B per node of span; here a
/// 10⁷ span would be 1.7 GB, and the set holds under 4 KiB. The rule's
/// buffers follow the same rule: before any verdict they hold nothing.
#[test]
fn table_allocation_follows_touched_pairs_not_the_node_id_span() {
    let mut tables = FrontierTables::new(3);
    assert_eq!(tables.heap_bytes(), 0, "nothing allocated up front");
    let far: [NodeId; 5] = [9_999_999, 9_999_998, 5_000_000, 1 << 33, 7];
    assert!(
        tables
            .arrive(far[0], 0, None)
            .expect("the ledger has room")
            .new
    );
    for pair in far.windows(2) {
        assert!(
            tables
                .arrive(pair[1], 1, Some(rel_parent(pair[0], 0, 1)))
                .expect("the ledger has room")
                .new
        );
    }
    tables.mark_endpoint(far[4]);
    assert_eq!(tables.touched_nodes(), far.len());
    let small = tables.heap_bytes();
    assert!(
        small > 0 && small < 4096,
        "a 5-node frontier holds {small} B"
    );

    // A thousand touched nodes scale the set by the touched count only:
    // under 200 B per touched pair however far apart the ids sit.
    tables.begin_seed();
    assert!(tables.arrive(0, 0, None).expect("the ledger has room").new);
    for i in 1..1000 {
        assert!(
            tables
                .arrive(i * 10_007, 0, Some(rel_parent(0, 0, i)))
                .expect("the ledger has room")
                .new
        );
    }
    let thousand = tables.heap_bytes();
    assert!(
        thousand < 1000 * 200,
        "1000 touched pairs hold {thousand} B, not a span-sized table"
    );
    // The recycled set keeps its capacity across the O(1) reset.
    tables.begin_seed();
    assert_eq!(tables.heap_bytes(), thousand);

    assert_eq!(RuleBuffers::new().heap_bytes(), 0);
}

/// The ceiling's arithmetic is exact bytes, not an estimate: the check
/// admits exactly up to the held ceiling.
#[test]
fn the_table_ceiling_admits_exact_bytes_up_to_the_budget() {
    with_frontier_table_budget(1024, || {
        assert!(within_table_budget(0));
        assert!(within_table_budget(1024));
        assert!(!within_table_budget(1025));
    });
}

/// The ceiling's run-time decline at the core: a run whose exact table
/// bytes cross the held ceiling at a level boundary answers
/// `RunOutcome::Declined` with `FrontierRunDecline::TableBudget` — never a
/// partial endpoint set — and the same run under the production
/// constant is served.
#[test]
fn a_run_over_the_table_ceiling_declines_with_the_table_budget_reason() {
    let built = build(&[&["N"], &["N"], &["N"]], &[(0, 1, "T"), (1, 2, "T")]);
    let nfa = compile(
        &built.graph,
        &patterns("MATCH p = (x)-[:T*1..]->(y) RETURN p")[0],
        PatternEngine::Auto,
    )
    .unwrap();
    let view = ExecGraph::read(&built.graph);
    let run = |tables: &mut FrontierTables| {
        run_frontier(
            ExecCtx::new(&view, &BTreeMap::new()),
            built.nodes[0],
            &nfa,
            WalkBudget::standard(),
            Pool::Default,
            tables,
            &mut RuleBuffers::new(),
        )
        .unwrap()
    };
    let served = run(&mut fresh_tables(&nfa)).unwrap_served();
    assert_eq!(
        endpoint_nodes(&served),
        vec![built.nodes[1], built.nodes[2]]
    );
    let declined = with_frontier_table_budget(1, || run(&mut fresh_tables(&nfa)));
    assert!(
        matches!(
            declined,
            RunOutcome::Declined {
                reason: FrontierRunDecline::TableBudget,
                ..
            }
        ),
        "the over-ceiling run declines: {declined:?}"
    );
}
