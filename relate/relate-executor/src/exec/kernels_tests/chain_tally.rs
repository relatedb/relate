//! The two-hop tally fold's suite (REL8-2214): the fold answers what the
//! kernels-stood-down walker answers, engages exactly where its gates say,
//! visits edges in proportion to the first-hop edges and the distinct
//! middle nodes (never the path count), and keeps counting exactly when
//! its memo is full.

use crate::graph::Graph;

use super::super::take_neighbor_walks;
use super::super::test_support::{
    assert_expression_cap, count_of, exec_autocommit, try_exec_cypher, Builder,
};
use super::{adversarial_graph, differential, ExpectedEngagement};

/// One hub with `n` in-spokes and `n` out-spokes: `n * n` two-hop paths
/// through one middle node.
fn hub(n: usize) -> Graph {
    let g = Builder::new().done();
    exec_autocommit(
        &g,
        &format!(
            "CREATE (m:Mid {{k: 1}})
             WITH m UNWIND range(1, {n}) AS i
             CREATE (:S {{i: i}})-[:R]->(m), (m)-[:T]->(:E {{i: i}})"
        ),
    )
    .unwrap();
    g
}

/// The hub graph's one statement.
const HUB_QUERY: &str = "MATCH (a:S)-[:R]->(m:Mid)-[:T]->(b:E) RETURN count(*)";

#[test]
fn the_tally_matches_the_walker_across_types_directions_and_predicates() {
    let g = adversarial_graph();
    let mut nonzero = 0usize;
    for q in [
        // same type, both ways, with the head and the middle labelled
        "MATCH (a:U)-[:F]->(m:U)-[:F]->(b) RETURN count(*)",
        "MATCH (a:U)<-[:F]-(m:U)<-[:F]-(b) RETURN count(*)",
        // different types per hop
        "MATCH (a:U)-[:F]->(m)-[:G]->(b) RETURN count(*)",
        "MATCH (a:U)-[:G]->(m)-[:F]->(b) RETURN count(b)",
        // a predicate at the start, the middle and the end
        "MATCH (a:U {id:'a'})-[:F]->(m)-[:F]->(b) RETURN count(*)",
        "MATCH (a:U)-[:F]->(m)-[:F]->(b) WHERE m.id <> 'a' RETURN count(*)",
        "MATCH (a:U)-[:F]->(m)-[:F]->(b) WHERE b.id <> 'a' RETURN count(*)",
        "MATCH (a:U)-[:F]->(m)-[:F]->(b) WHERE m.id <> 'a' AND b.id <> 'b' RETURN count(*)",
        // named rels and an unlabelled head
        "MATCH (a)-[r1:F]->(m:U)-[r2:F]->(b:U) RETURN count(*)",
        // the exact `*2` spelling unrolls to the same two hops
        "MATCH (a:U)-[:F*2]->(b) RETURN count(*)",
    ] {
        let result = differential(&g, q, ExpectedEngagement::Engaged);
        nonzero += usize::from(count_of(&result) > 0);
    }
    assert!(nonzero > 0, "the corpus never answered nonzero");
}

#[test]
fn a_self_loop_at_the_start_excludes_its_own_rel() {
    // `a` carries a self-loop; the first hop may arrive back at `a`, and
    // its rel must not be reused by the second hop. Two nodes, so the
    // answer is small enough to state.
    let g = Builder::new().done();
    exec_autocommit(
        &g,
        "CREATE (a:N {k: 1}), (b:N {k: 2}),
                (a)-[:L]->(a), (a)-[:L]->(b), (b)-[:L]->(a)",
    )
    .unwrap();
    // Walk-by-walk, rels pairwise distinct: from `a` the first hops are
    // loop, a->b; from `b` the first hop is b->a.
    //   a -loop-> a, second hop over {a->b, a->loop excluded}: 1
    //   a -a->b-> b, second hop over {b->a}: 1
    //   b -b->a-> a, second hop over {loop, a->b}: 2
    let result = differential(
        &g,
        "MATCH (x:N)-[:L]->(y:N)-[:L]->(z:N) RETURN count(*)",
        ExpectedEngagement::Engaged,
    );
    assert_eq!(count_of(&result), 4);
}

#[test]
fn opposite_directions_and_undirected_hops_keep_their_rungs() {
    let g = adversarial_graph();
    for q in [
        // the second hop's candidates can include the first hop's own rel
        "MATCH (a:U)-[:F]->(m)<-[:F]-(b) RETURN count(*)",
        "MATCH (a:U)<-[:F]-(m)-[:F]->(b) RETURN count(*)",
        // an undirected hop keeps the lineage the walker settles
        "MATCH (a:U)-[:F]-(m)-[:F]-(b) RETURN count(*)",
        // a disjunctive type list is outside the static type proof
        "MATCH (a:U)-[:F|G]->(m)-[:F|G]->(b) RETURN count(*)",
    ] {
        differential(&g, q, ExpectedEngagement::Declined);
    }
}

#[test]
fn edge_visits_follow_the_edges_not_the_paths() {
    // 400 x 400 = 160,000 paths through one middle node; past the memo's
    // short warm-up the tally is read, not re-walked, so the visits stay
    // an order of magnitude under the path count.
    let g = hub(400);
    let result = differential(&g, HUB_QUERY, ExpectedEngagement::Engaged);
    assert_eq!(count_of(&result), 160_000);
    // `differential` ran both the kernel and the walker; run the kernel
    // path alone for the visit count.
    take_neighbor_walks();
    let again = try_exec_cypher(&g, HUB_QUERY).unwrap();
    assert_eq!(count_of(&again), 160_000);
    let walks = take_neighbor_walks();
    assert!(
        walks <= 10_000,
        "the tally must visit edges, not paths: {walks} walked rels for 160,000 paths"
    );
}

#[test]
fn a_bypassed_memo_keeps_counting_exactly() {
    // A zero memo budget allocates no table (ADR-0657): every first-hop
    // edge re-tallies its middle node. The answer is the walker's all the
    // same, and the statement does not fail.
    let mut g = hub(20);
    g.configure_expression_memo_budget(0);
    let result = differential(&g, HUB_QUERY, ExpectedEngagement::Engaged);
    assert_eq!(count_of(&result), 400);
    let mut adversarial = adversarial_graph();
    adversarial.configure_expression_memo_budget(0);
    differential(
        &adversarial,
        "MATCH (a:U)-[:F]->(m)-[:F]->(b) RETURN count(*)",
        ExpectedEngagement::Engaged,
    );
}

#[test]
fn the_start_list_is_charged_to_the_expression_budget() {
    // 50 start candidates are 400 bytes of retained ids: a cap below that
    // refuses the statement with the typed expression-capacity error, as
    // every retaining container does (ADR-0463).
    let mut g = hub(50);
    g.configure_expression_memory_limit(Some(100));
    let error = try_exec_cypher(&g, HUB_QUERY)
        .err()
        .expect("the retained start list must cross a 100-byte cap");
    assert_expression_cap(&error, 100);
}
