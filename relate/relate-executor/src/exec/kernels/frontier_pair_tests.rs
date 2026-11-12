//! The boundary-pair comparison (`n <> p`, LABS-1433) the frontier's
//! distinct-endpoint consumer admits, and the plan-time decline reason
//! the PROFILE record carries (LABS-1437).
//!
//! Three layers: the matcher's decisions (which statements carry the
//! comparison, which keep the `Conjunct` decline), the differential of
//! the engaged answers against the per-row walker (the diagnostic
//! switch's `walk`, and the cost-gated `auto`, over generated
//! multigraphs with self-loops, parallel rels, cycles back to the seed
//! and multi-label endpoints, plus the sqlite-gate's own statement
//! shapes), and the PROFILE record's typed reason.

use std::collections::BTreeMap;

use proptest::prelude::*;
use relate_core::Value;

use crate::ctx::ExecCtx;
use crate::exec::test_support::{
    build, complete_graph, decide, declines_with, exec_cypher, file_triangle, int_cell,
    leading_match, line_graph, planned_clauses, profiled, reset_frontier_facts, same_rows, served,
    Built, LeadingMatch,
};
use crate::frontier_decline::{FrontierDecline, FrontierDeclineReason};
use crate::graph::Graph;
use crate::overlay::ExecGraph;
use crate::pattern_engine::PatternEngine;
use crate::row::Row;
use crate::test_support::config_with_floor;
use crate::QueryResult;

use super::super::take_frontier_counters;
use super::frontier_match::{BoundaryPair, Consumer};
use super::frontier_run::try_frontier_rows;
use super::take_kernel_engagements;

/// One run of `cypher` under `engine`, the store's routing switch held
/// for the run: the answer, the kernel engagements and the frontier
/// run's level count.
fn run_under(graph: &mut Graph, engine: PatternEngine, cypher: &str) -> (QueryResult, u32, u64) {
    graph.configure_pattern_engine(engine);
    reset_frontier_facts();
    let answer = exec_cypher(graph, cypher);
    let engagements = take_kernel_engagements();
    let levels = take_frontier_counters().levels;
    graph.configure_pattern_engine(PatternEngine::Auto);
    (answer, engagements, levels)
}

/// The three routings agree on `cypher`: the walker (`walk`, the oracle),
/// the cost-gated default (`auto`), and the forced rung (`nfa`, every
/// proof but the cost gate). Returns the forced run's engagements and
/// frontier levels — the caller decides whether this shape must have
/// ridden the rung — and the walker's answer.
fn agree(graph: &mut Graph, cypher: &str) -> (u32, u64, QueryResult) {
    let (walker, walk_engaged, walk_levels) = run_under(graph, PatternEngine::Walk, cypher);
    assert_eq!(
        (walk_engaged, walk_levels),
        (0, 0),
        "the walker oracle never rides a kernel: {cypher}"
    );
    let (auto, _, _) = run_under(graph, PatternEngine::Auto, cypher);
    assert!(same_rows(&auto, &walker), "auto vs walker: {cypher}");
    let (forced, engaged, levels) = run_under(graph, PatternEngine::Nfa, cypher);
    assert!(same_rows(&forced, &walker), "nfa vs walker: {cypher}");
    (engaged, levels, walker)
}

/// The one count cell of a one-row result.
fn count_of(answer: &QueryResult) -> i64 {
    let [row] = answer.rows.as_slice() else {
        panic!("one row, got {:?}", answer.rows);
    };
    let [cell] = row.as_slice() else {
        panic!("one cell, got {row:?}");
    };
    int_cell(cell).unwrap_or_else(|| panic!("an integer cell, got {cell:?}"))
}

// ---------------------------------------------------------------------
// The matcher's decisions.

/// The comparison rides the distinct-endpoint consumer, either operand
/// order, beside a property conjunct on a boundary variable.
#[test]
fn the_distinct_endpoint_consumer_admits_the_boundary_pair_comparison() {
    for cypher in [
        "MATCH (x:Person)-[:KNOWS*1..3]-(y:Person) WHERE x <> y RETURN DISTINCT x, y",
        "MATCH (x:Person)-[:KNOWS*1..3]-(y:Person) WHERE y <> x RETURN DISTINCT x, y",
        "MATCH (x:Person)-[:KNOWS*1..3]-(y:Person) WHERE x <> y AND y.id > 1 \
         RETURN DISTINCT x, y",
        "MATCH (x:Person)-[:KNOWS*1..3]-(y:Person) WHERE x <> y \
         WITH DISTINCT x, y RETURN count(*) AS pairs",
    ] {
        decide(cypher, &complete_graph(), false, &Row::new(), |decision| {
            let contract = decision.unwrap_or_else(|e| panic!("{cypher} engages: {e:?}"));
            assert_eq!(contract.consumer, Consumer::DistinctEndpoints, "{cypher}");
            assert_eq!(contract.pair, BoundaryPair::Distinct, "{cypher}");
        });
    }
    // No comparison, no constraint.
    decide(
        "MATCH (x:Person)-[:KNOWS*1..3]-(y:Person) RETURN DISTINCT x, y",
        &complete_graph(),
        false,
        &Row::new(),
        |decision| {
            let contract = decision.unwrap_or_else(|e| panic!("engages: {e:?}"));
            assert_eq!(contract.pair, BoundaryPair::Unconstrained);
        },
    );
}

/// Every consumer whose answer is not the pair set keeps the old decline,
/// and every comparison that is not `<>` between the two boundary
/// variables is still outside the closed vocabulary.
#[test]
fn every_other_shape_keeps_the_conjunct_decline() {
    let graph = complete_graph();
    let declines = |cypher: &str| declines_with(cypher, &graph, FrontierDecline::Conjunct);
    // The capped consumer declines the comparison.
    declines("MATCH (x:Person)-[:KNOWS*1..3]-(y:Person) WHERE x <> y RETURN x, y LIMIT 3");
    // So does the EXISTS verdict, whose consumer the wiring constructs.
    decide(
        "MATCH (x:Person)-[:KNOWS*1..3]-(y:Person) WHERE x <> y RETURN y",
        &graph,
        true,
        &Row::new(),
        |decision| assert_eq!(decision.err(), Some(FrontierDecline::Conjunct)),
    );
    // Not `<>`: equality keeps only the self pair.
    declines("MATCH (x:Person)-[:KNOWS*1..3]-(y:Person) WHERE x = y RETURN DISTINCT x, y");
    // Not between the two boundary variables.
    declines("MATCH (x:Person)-[:KNOWS*1..3]-(y:Person) WHERE x <> x RETURN DISTINCT x, y");
    declines("MATCH (x:Person)((a)-[:KNOWS]-(b)){1,2}(y:Person) WHERE a <> y RETURN DISTINCT x, y");
    // Not a conjunct of the closed vocabulary (the comparison under OR).
    declines(
        "MATCH (x:Person)-[:KNOWS*1..3]-(y:Person) WHERE x <> y OR y.id > 1 \
         RETURN DISTINCT x, y",
    );
    // Property against property is not a row-invariant bound.
    declines("MATCH (x:Person)-[:KNOWS*1..3]-(y:Person) WHERE x.id <> y.id RETURN DISTINCT x, y");
}

/// The cost gate is untouched: the comparison does not engage a clause
/// the gate prices as output-bound.
#[test]
fn the_cost_gate_still_decides_a_comparison_clause() {
    decide(
        "MATCH (x:Person)-[:KNOWS*1..2]-(y:Person) WHERE x <> y RETURN DISTINCT x, y",
        &line_graph(),
        false,
        &Row::new(),
        |decision| assert_eq!(decision.err(), Some(FrontierDecline::Cost)),
    );
}

// ---------------------------------------------------------------------
// The differential against the walker.

/// Hand-built fixtures with a closed-form answer: each is the shape a
/// naive "drop the comparison" or "drop the seed from the BFS" would get
/// wrong.
#[test]
fn the_seed_reached_again_is_excluded_per_pair_not_per_graph() {
    // A triangle: from any seed the 3-trail walks back to the seed, so
    // the comparison drops exactly that pair; without it nine pairs.
    let mut triangle = file_triangle();
    let with = "MATCH (a:File)-[:T*1..3]-(b:File) WHERE a <> b \
                WITH DISTINCT a, b RETURN count(*) AS pairs";
    let without = "MATCH (a:File)-[:T*1..3]-(b:File) \
                   WITH DISTINCT a, b RETURN count(*) AS pairs";
    let (engaged, levels, walker) = agree(&mut triangle.graph, with);
    assert_eq!((engaged, levels > 0), (1, true), "the comparison rides");
    assert_eq!(count_of(&walker), 6, "the self pair of every seed drops");
    let (engaged, _, walker) = agree(&mut triangle.graph, without);
    assert_eq!(engaged, 1);
    assert_eq!(
        count_of(&walker),
        9,
        "without the comparison the seed stays"
    );

    // A self-loop on the seed and a parallel pair: the loop re-reaches
    // the seed at length 1, the parallel rels make a length-2 trail
    // back and forth; neither adds an endpoint the walker lacks.
    let mut loops = build(
        &[&["File"], &["File"]],
        &[(0, 0, "T"), (0, 1, "T"), (0, 1, "T")],
    );
    let (engaged, _, walker) = agree(&mut loops.graph, with);
    assert_eq!(engaged, 1);
    assert_eq!(count_of(&walker), 2, "(0,1) and (1,0), the loops dropped");
    // The zero-length arrival of `*0..` is the seed itself: dropped.
    let zero = "MATCH (a:File)-[:T*0..2]-(b:File) WHERE a <> b \
                WITH DISTINCT a, b RETURN count(*) AS pairs";
    let (engaged, _, walker) = agree(&mut loops.graph, zero);
    assert_eq!(engaged, 1);
    assert_eq!(count_of(&walker), 2);
}

/// The comparison is per (seed, endpoint) pair: a node that is no seed's
/// own endpoint but another seed's far endpoint still answers, in a
/// `RETURN DISTINCT b` that never names the seed.
#[test]
fn a_far_endpoint_equal_to_another_seed_still_answers() {
    let mut line = build(
        &[&["File"], &["File"], &["File"]],
        &[(0, 1, "T"), (1, 2, "T")],
    );
    let query = "MATCH (a:File)-[:T*1..2]-(b:File) WHERE a <> b RETURN DISTINCT b.id AS b";
    let (engaged, _, walker) = agree(&mut line.graph, query);
    assert_eq!(engaged, 1);
    assert_eq!(
        walker.rows.len(),
        3,
        "every node is another seed's endpoint"
    );
}

/// A pinned seed through the carrying-row path of the exists-free rung:
/// the seed side is an inline-property start and the comparison drops the
/// seed from its own endpoint set at `min = 0`.
#[test]
fn an_anchored_seed_drops_itself() {
    let mut tri = build(
        &[&["File"], &["File", "Hot"], &["File"], &["File"]],
        &[
            (0, 1, "T"),
            (1, 2, "T"),
            (2, 0, "T"),
            (2, 2, "T"),
            (2, 3, "U"),
        ],
    );
    for cypher in [
        "MATCH (a:File {id: 0})-[:T*0..3]-(b) WHERE b <> a RETURN count(DISTINCT b) AS c",
        "MATCH (a:File {id: 0})-[:T|U*0..3]->(b) WHERE a <> b RETURN count(DISTINCT b) AS c",
        "MATCH (a:File {id: 2})-[:T*1..3]-(b:Hot) WHERE a <> b RETURN count(DISTINCT b) AS c",
    ] {
        let (engaged, _, _) = agree(&mut tri.graph, cypher);
        assert_eq!(engaged, 1, "{cypher}");
    }
}

/// The rung's own filter, with no reduce tail behind it: the dispatch
/// always hands the rung a sink projection or fold whose `reduce_row`
/// re-applies the `WHERE`, so the comparison's per-pair drop is observable
/// only on the rung called bare — the rows it returns are exactly the
/// pairs the clause states, the seed never its own endpoint.
#[test]
fn the_rung_drops_the_self_pair_without_a_reduce_tail() {
    let mut built = file_triangle();
    // The forced rung: the three-node triangle is under the cost gate.
    built.graph.configure_pattern_engine(PatternEngine::Nfa);
    let cypher = "MATCH (a:File)-[:T*1..3]-(b:File) WHERE a <> b \
                  WITH DISTINCT a, b RETURN count(*) AS pairs";
    let clauses = planned_clauses(cypher);
    let LeadingMatch {
        match_mode,
        patterns,
        where_clause,
        suffix,
    } = leading_match(&clauses, cypher);
    let view = ExecGraph::read(&built.graph);
    let params = BTreeMap::new();
    let ctx = ExecCtx::new(&view, &params, None);
    let rows = try_frontier_rows(
        ctx,
        &[Row::new()],
        false,
        match_mode,
        patterns,
        where_clause,
        suffix,
        None,
        None,
    )
    .unwrap()
    .served()
    .expect("the rung serves the clause");
    assert_eq!(rows.len(), 6, "two endpoints per seed, the seed never one");
    for row in &rows {
        let (Some(Value::Node(a)), Some(Value::Node(b))) = (row.get("a"), row.get("b")) else {
            panic!("both boundary variables are bound: {row:?}");
        };
        assert_ne!(a.id, b.id, "no row pairs a seed with itself");
    }
}

/// A consumer outside the comparison's reach answers the walker's rows
/// with no kernel: the count of paths, and the walk-count arm.
#[test]
fn consumers_outside_the_pair_set_answer_as_the_walker() {
    let mut tri = file_triangle();
    for cypher in [
        "MATCH (a:File)-[:T*1..3]-(b:File) WHERE a <> b RETURN count(*) AS paths",
        "MATCH REPEATABLE ELEMENTS (a:File)-[:T*1..3]-(b:File) WHERE a <> b \
         RETURN count(*) AS walks",
        "MATCH (a:File)-[:T*1..3]-(b:File) WHERE a <> b RETURN a.id AS a, b.id AS b",
    ] {
        let (engaged, levels, _) = agree(&mut tri.graph, cypher);
        assert_eq!((engaged, levels), (0, 0), "{cypher}");
    }
}

/// The node label slots of the generated graphs: single, multi-label
/// (`File` plus `Hot`, `File` plus `Prompt`) and a non-`File` node.
const LABEL_SETS: [&[&str]; 4] = [
    &["File"],
    &["File", "Hot"],
    &["Prompt"],
    &["File", "Prompt"],
];

/// One generated case: the label slot per node, the rels (two endpoint
/// slots and a kind slot — `T` twice, `U` once, so parallel rels are as
/// likely as singles; the modulo mapping reaches self-loops and cycles),
/// and the hop bound.
type PairCase = (Vec<usize>, Vec<(usize, usize, usize)>, u32);

fn pair_case() -> BoxedStrategy<PairCase> {
    (
        proptest::collection::vec(0usize..LABEL_SETS.len(), 3..=7),
        proptest::collection::vec((0usize..7, 0usize..7, 0usize..3), 2..=12),
        1u32..=4,
    )
        .prop_filter(
            "two File nodes, both rel types interned",
            |(labels, edges, _)| {
                labels
                    .iter()
                    .filter(|slot| LABEL_SETS[**slot].contains(&"File"))
                    .count()
                    >= 2
                    && ["T", "U"]
                        .iter()
                        .all(|name| edges.iter().any(|&(_, _, kind)| rel_type(kind) == *name))
            },
        )
        .boxed()
}

/// The relationship type of a generated rel's kind slot — the one
/// partition the strategy's coverage filter and the graph builder both
/// consult: kind slots 0 and 1 are `T` (parallel rels as likely as
/// singles), slot 2 is `U`.
fn rel_type(kind: usize) -> &'static str {
    if kind < 2 {
        "T"
    } else {
        "U"
    }
}

/// The generated case's graph, endpoints taken modulo the node count.
fn pair_graph(labels: &[usize], edges: &[(usize, usize, usize)]) -> Built {
    let sets: Vec<&[&str]> = labels.iter().map(|slot| LABEL_SETS[*slot]).collect();
    let rels: Vec<(usize, usize, &str)> = edges
        .iter()
        .map(|&(a, b, kind)| (a % labels.len(), b % labels.len(), rel_type(kind)))
        .collect();
    build(&sets, &rels)
}

/// The statement shapes the comparison rides: each must engage under the
/// forced rung (the typed, labeled-or-bare boundary, the NFA compiles),
/// and answer the walker's rows under all three routings.
const ENGAGING: &[&str] = &[
    // The co-change gate shape, undirected, typed.
    "MATCH (a:File)-[:T*1..{k}]-(b:File) WHERE a <> b \
     WITH DISTINCT a, b RETURN count(*) AS pairs",
    // The rows themselves, not a count.
    "MATCH (a:File)-[:T*1..{k}]-(b:File) WHERE a <> b RETURN DISTINCT a.id AS a, b.id AS b",
    // Directed, both ways, a union of types.
    "MATCH (a:File)-[:T|U*1..{k}]->(b:File) WHERE a <> b \
     WITH DISTINCT a, b RETURN count(*) AS pairs",
    "MATCH (a:File)<-[:T*1..{k}]-(b) WHERE b <> a RETURN DISTINCT a.id AS a, b.id AS b",
    // A property conjunct beside the comparison.
    "MATCH (a:File)-[:T*1..{k}]-(b:File) WHERE a <> b AND b.id > 1 \
     WITH DISTINCT a, b RETURN count(*) AS pairs",
    // Zero-length arrivals and a raised minimum.
    "MATCH (a:File)-[:T*0..{k}]-(b:File) WHERE a <> b \
     WITH DISTINCT a, b RETURN count(*) AS pairs",
    "MATCH (a:File)-[:T*2..{k1}]-(b:File) WHERE a <> b \
     WITH DISTINCT a, b RETURN count(*) AS pairs",
    // A quantified group spelling.
    "MATCH (a:File)((u)-[:T]-(v)){1,{k}}(b:File) WHERE a <> b \
     WITH DISTINCT a, b RETURN count(*) AS pairs",
    // The walk pool.
    "MATCH REPEATABLE ELEMENTS (a:File)-[:T*1..{k}]-(b:File) WHERE a <> b \
     WITH DISTINCT a, b RETURN count(*) AS pairs",
    // The rung's rows flow onward with no sink projection or fold to
    // re-filter them (the next clause is not a projection the sink can
    // fuse): the comparison must already have dropped the self pair.
    "MATCH (a:File)-[:T*1..{k}]-(b:File) WHERE a <> b WITH DISTINCT a, b \
     MATCH (b)-[:U]->(c) RETURN count(*) AS n",
    "MATCH (a:File)-[:T*1..{k}]-(b:File) WHERE a <> b WITH DISTINCT a, b \
     RETURN a.id AS a, b.id AS b ORDER BY a, b",
    "MATCH (a:File)-[:T*1..{k}]-(b:File) WHERE a <> b RETURN DISTINCT b \
     ORDER BY b.id SKIP 1",
    // An aggregate over the endpoint set from an anchored seed.
    "MATCH (a {id: 0})-[:T*0..{k}]-(b) WHERE b <> a RETURN count(DISTINCT b) AS c",
];

/// The shapes outside the engine's reach (the untyped hop declines the
/// NFA compile, the path-multiset consumers decline the answer shape):
/// the answers are the walker's under every routing, and the rung never
/// engages.
const DECLINING: &[&str] = &[
    "MATCH (a:File)-[*1..{k}]-(b) WHERE a <> b WITH DISTINCT a, b RETURN count(*) AS pairs",
    "MATCH (a:File)-[:T*1..{k}]-(b:File) WHERE a <> b RETURN count(*) AS paths",
];

proptest! {
    #![proptest_config(config_with_floor(192))]
    #[test]
    fn generated_multigraphs_answer_as_the_walker(
        (labels, edges, k) in pair_case(),
    ) {
        let mut built = pair_graph(&labels, &edges);
        for template in ENGAGING {
            let cypher = template
                .replace("{k}", &k.to_string())
                .replace("{k1}", &(k + 1).to_string());
            let (engaged, levels, _) = agree(&mut built.graph, &cypher);
            prop_assert_eq!(engaged, 1, "the forced rung takes {}", cypher);
            prop_assert!(levels > 0, "the frontier ran for {}", cypher);
        }
        for template in DECLINING {
            let cypher = template
                .replace("{k}", &k.to_string())
                .replace("{k1}", &(k + 1).to_string());
            let (engaged, levels, _) = agree(&mut built.graph, &cypher);
            prop_assert_eq!((engaged, levels), (0, 0), "no rung takes {}", cypher);
        }
    }
}

/// A deterministic multigraph of `nodes` nodes and about `1.5 * nodes`
/// rels (xorshift): self-loops and parallel rels included, every third
/// node a `Prompt`, every node a `File`, so the sqlite-gate's two
/// statement shapes both have seeds.
fn gate_graph(nodes: usize) -> Built {
    let mut state = 0x9E37_79B9_7F4A_7C15_u64;
    let mut next = move |bound: usize| {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        (state % bound as u64) as usize
    };
    let labels: Vec<&[&str]> = (0..nodes)
        .map(|i| {
            if i % 3 == 0 {
                &["File", "Prompt"][..]
            } else {
                &["File"][..]
            }
        })
        .collect();
    let rels: Vec<(usize, usize, &str)> = (0..nodes * 3 / 2)
        .map(|_| (next(nodes), next(nodes), "TOUCHES"))
        .collect();
    build(&labels, &rels)
}

/// The sqlite-gate's own statement shapes: co-change at `d = 1..4`
/// (`*1..2d`) answers the walker under every routing, riding the forced
/// rung; the k-hop reach shape (an untyped hop, LABS-1434's ticket) stays
/// on the walker with the same answer, and its decline names the NFA
/// compile.
#[test]
fn the_gate_statements_answer_as_the_walker() {
    let mut built = gate_graph(36);
    for d in 1..=4u32 {
        let cypher = format!(
            "MATCH (a:File)-[:TOUCHES*1..{}]-(b:File) WHERE a <> b \
             WITH DISTINCT a, b RETURN count(*) AS pairs",
            2 * d
        );
        let (engaged, levels, _) = agree(&mut built.graph, &cypher);
        assert_eq!(
            (engaged, levels > 0),
            (1, true),
            "d={d} rides the forced rung"
        );
    }
    for k in 3..=5u32 {
        let cypher = format!(
            "MATCH (p:Prompt)-[*1..{k}]-(n) WHERE n <> p \
             WITH DISTINCT p, n RETURN count(*) AS pairs"
        );
        let (engaged, levels, _) = agree(&mut built.graph, &cypher);
        assert_eq!(
            (engaged, levels),
            (0, 0),
            "k={k}: the untyped hop stays on the walker"
        );
        let records = profiled(&built.graph, &cypher);
        assert_eq!(
            served(&records).frontier_decline,
            Some(FrontierDeclineReason::Plan(FrontierDecline::NfaShape)),
            "k={k}: the decline names the NFA compile"
        );
    }
}
