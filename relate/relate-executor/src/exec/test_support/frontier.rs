//! Frontier vocabulary: the pattern-lowering and IR-constructor helpers every frontier suite reuses.

// The frontier family's shared test fixtures (stellar structure-4 hoist):
// the pattern-lowering and IR-constructor helpers every frontier suite
// reuses — the one canonical spelling of "lower this Cypher to patterns"
// and "build this IR shape" (`nfa_tests`' own assertions read them like
// every other suite).

use std::collections::BTreeMap;

use relate_core::NodeId;
use relate_front::parse_and_lower;
use relate_ir::{
    Clause, Direction, NodePattern, PathPattern, PatternElement, QuantifiedPathPattern, Query,
    RelPattern,
};

use super::super::kernels::frontier::{run, FrontierRun, WalkBudget};
use super::super::kernels::frontier_tables::fresh_tables;
use super::super::kernels::nfa::{compile, FrontierNfa, Pool};
use super::super::kernels::rearival::RuleBuffers;
use crate::ctx::ExecCtx;
use crate::graph::Graph;
use crate::overlay::ExecGraph;
use crate::pattern_engine::PatternEngine;

/// The lowered `MATCH` patterns of `cypher` — the front end's own
/// shapes, the coverage harness's route (no planner orientation):
/// these tests compile what lowering actually produces, the shapes the
/// scan-segment coverage report pins.
pub(crate) fn patterns(cypher: &str) -> Vec<PathPattern> {
    let query: Query = parse_and_lower(cypher).unwrap();
    query
        .parts
        .iter()
        .flat_map(|part| {
            part.clauses.iter().filter_map(|clause| match clause {
                Clause::Match { patterns, .. } => Some(patterns.as_slice()),
                _ => None,
            })
        })
        .flatten()
        .cloned()
        .collect()
}

/// A named unconstrained node — the IR-constructor vocabulary's base.
pub(crate) fn named_node(name: &str) -> NodePattern {
    NodePattern {
        variable: Some(name.into()),
        ..NodePattern::anonymous()
    }
}

/// A plain one-hop undirected rel over a static type list.
pub(crate) fn one_hop_rel(types: &[&str]) -> RelPattern {
    RelPattern {
        variable: None,
        types: types.iter().map(|name| (*name).into()).collect(),
        type_expr: None,
        direction: Direction::Both,
        properties: None,
        var_length: false,
        min_hops: 1,
        max_hops: Some(1),
        where_clause: None,
        quantified: false,
    }
}

/// A var-length rel as the front end lowers it inside a group: the
/// `*m..n` bounds on the rel element itself (`quantify_var_length`
/// runs top level only).
pub(crate) fn var_length_rel(types: &[&str], min: u32, max: Option<u32>) -> RelPattern {
    RelPattern {
        var_length: true,
        min_hops: min,
        max_hops: max,
        ..one_hop_rel(types)
    }
}

/// A quantified path part over `elements` with the given bounds.
pub(crate) fn quantified(
    elements: Vec<PatternElement>,
    min: u32,
    max: Option<u32>,
) -> QuantifiedPathPattern {
    QuantifiedPathPattern {
        subpath_variable: None,
        elements,
        where_clause: None,
        min,
        max,
    }
}

/// A path pattern over `elements` with the compact projections cleared
/// (lowering clears them when a `Quantified` element is present; the
/// compilers read the lossless `elements`).
pub(crate) fn path_pattern(elements: Vec<PatternElement>) -> PathPattern {
    PathPattern {
        variable: None,
        subpath_variable: None,
        elements,
        nodes: Vec::new(),
        rels: Vec::new(),
        where_clause: None,
        mode: None,
        selector: None,
        is_shortest_path_function: false,
    }
}

/// The endpoint nodes of `run`, sorted — the distinct-endpoint answer.
/// The frontier family's one spelling (the runner suite and the
/// re-arrival suite share it).
pub(crate) fn endpoint_nodes(run: &FrontierRun) -> Vec<NodeId> {
    let mut nodes: Vec<NodeId> = run.endpoints.iter().map(|e| e.node).collect();
    nodes.sort_unstable();
    nodes
}

/// Compile `cypher`'s first `MATCH` pattern against `graph`'s store on
/// the auto rung and run the frontier from `seed` over a read view, in
/// `pool`, under the standard budget with fresh tables and a served run —
/// the frontier suites' one run-harness spelling (the runner suite's
/// pins, the re-arrival suite's endpoint comparisons, and its
/// direct-rule pins' served runs): an authored panic on either failure,
/// the decline named. The low-level budget, recycling, and
/// overlay-visibility pins drive `kernels::frontier::run` directly
/// instead — this is the happy-path spelling, never the only one.
pub(crate) fn served_frontier_run(
    cypher: &str,
    graph: &Graph,
    seed: NodeId,
    pool: Pool,
) -> FrontierRun {
    let nfa = compile(graph, &patterns(cypher)[0], PatternEngine::Auto)
        .unwrap_or_else(|| panic!("{cypher} should compile"));
    served_nfa_run(&nfa, graph, seed, pool)
}

/// [`served_frontier_run`]'s run half over an already-compiled `nfa` —
/// the forced-rung suites compile with the rung they pin
/// (`PatternEngine::Nfa`) and serve the run through the same harness.
pub(crate) fn served_nfa_run(
    nfa: &FrontierNfa,
    graph: &Graph,
    seed: NodeId,
    pool: Pool,
) -> FrontierRun {
    let view = ExecGraph::read(graph);
    run(
        ExecCtx::new(&view, &BTreeMap::new(), None),
        seed,
        nfa,
        WalkBudget::standard(),
        pool,
        &mut fresh_tables(nfa),
        &mut RuleBuffers::new(),
    )
    .unwrap_or_else(|error| panic!("the frontier should run: {error}"))
    .unwrap_served()
}
