//! Frontier vocabulary: the pattern-lowering and IR-constructor helpers every frontier suite reuses.

// The frontier family's shared test fixtures (stellar structure-4 hoist):
// the pattern-lowering and IR-constructor helpers every frontier suite
// reuses — the one canonical spelling of "lower this Cypher to patterns"
// and "build this IR shape" (`nfa_tests`' own assertions read them like
// every other suite).

use std::collections::BTreeMap;

use relate_core::{NodeId, Value};
use relate_front::parse_and_lower;
use relate_ir::{
    Clause, Direction, Expr, MatchMode, NodePattern, PathPattern, PatternElement,
    QuantifiedPathPattern, Query, RelPattern,
};

use super::super::graph_stats::GraphStats;
use super::super::kernels::frontier::{run, FrontierRun, WalkBudget};
use super::super::kernels::frontier_match::{
    compile_frontier, decode_consumer, Consumer, FrontierContract,
};
use super::super::kernels::frontier_tables::fresh_tables;
use super::super::kernels::nfa::{compile, FrontierNfa, Pool};
use super::super::kernels::rearival::RuleBuffers;
use super::super::pushdown::flatten_and_owned;
use super::{build, profiled, served, Builder, Built};
use crate::ctx::ExecCtx;
use crate::frontier_decline::FrontierDecline;
use crate::graph::Graph;
use crate::overlay::ExecGraph;
use crate::pattern_engine::PatternEngine;
use crate::row::Row;

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
        types: types
            .iter()
            .map(|name| ((*name).into(), Default::default()))
            .collect(),
        type_expr: None,
        direction: Direction::Both,
        properties: None,
        prop_keys: Vec::new(),
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
        ExecCtx::new(&view, &BTreeMap::new()),
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

// The matcher arm's shared plan-time harness (the matcher, pair and decline
// suites): fixtures and the one planning/decision prologue.

/// A line graph (degree ~1): paths ≈ endpoints, the bounded
/// acyclic-schema recursion the cost gate leaves on the DFS.
pub(crate) fn line_graph() -> Graph {
    let b = Builder::new();
    let nodes: Vec<_> = (0..4)
        .map(|i| b.node(&format!("n{i}"), &["Person"]))
        .collect();
    for pair in nodes.windows(2) {
        b.rel(pair[0], pair[1], "KNOWS", &[]);
    }
    b.done()
}

/// The complete graph on five Persons (ten `knows` rels): the degree
/// machinery's path estimate outruns the endpoint pool by the factor.
pub(crate) fn complete_graph() -> Graph {
    let b = Builder::new();
    let nodes: Vec<_> = (0..5)
        .map(|i| b.node(&format!("n{i}"), &["Person"]))
        .collect();
    for i in 0..5 {
        for j in (i + 1)..5 {
            b.rel(nodes[i], nodes[j], "KNOWS", &[]);
        }
    }
    b.done()
}

/// One statement's planned first part: the clauses the plan-time
/// decisions read (the one planning prologue every matcher test shares).
pub(crate) fn planned_clauses(cypher: &str) -> Vec<Clause> {
    let query = parse_and_lower(cypher).unwrap();
    relate_planner::plan(query, &mut |_| false).unwrap().parts[0].clone()
}

/// A planned statement's leading `MATCH` and the clauses after it — the
/// facts the rung's entry reads.
pub(crate) struct LeadingMatch<'p> {
    pub(crate) match_mode: MatchMode,
    pub(crate) patterns: &'p [PathPattern],
    pub(crate) where_clause: Option<&'p Expr>,
    pub(crate) suffix: &'p [Clause],
}

/// The leading `MATCH` of [`planned_clauses`]' output.
pub(crate) fn leading_match<'p>(clauses: &'p [Clause], cypher: &str) -> LeadingMatch<'p> {
    let Clause::Match {
        match_mode,
        patterns,
        where_clause,
        ..
    } = &clauses[0]
    else {
        panic!(
            "{cypher}: the first planned clause is not a MATCH: {:?}",
            clauses[0]
        );
    };
    LeadingMatch {
        match_mode: *match_mode,
        patterns,
        where_clause: where_clause.as_ref(),
        suffix: &clauses[1..],
    }
}

/// The full plan-time decision on one statement: its planned `MATCH`
/// facts (patterns, mode, flattened conjuncts) and first-of-suffix
/// consumer decode against `graph`, then the matcher arm. `probe` marks
/// the EXISTS wiring (the consumer is constructed, not decoded); `row`
/// is the entering row the wiring site hands the arm. The decision is
/// consumed inside `k` while the plan is still alive — the contract
/// borrows the planned patterns.
pub(crate) fn decide<R>(
    cypher: &str,
    graph: &Graph,
    probe: bool,
    row: &Row,
    k: impl FnOnce(Result<FrontierContract<'_>, FrontierDecline>) -> R,
) -> R {
    let clauses = planned_clauses(cypher);
    let LeadingMatch {
        match_mode,
        patterns,
        where_clause,
        suffix,
    } = leading_match(&clauses, cypher);
    let conjuncts = where_clause.map(flatten_and_owned).unwrap_or_default();
    let view = ExecGraph::read(graph);
    let params: BTreeMap<String, Value> = BTreeMap::new();
    let stats = GraphStats::new(graph, &params);
    let consumer = if probe {
        // The probe arm's wiring constructs the verdict shape directly.
        Consumer::Exists
    } else {
        match decode_consumer(suffix, &patterns[0], &view, &params) {
            Ok(consumer) => consumer,
            Err(decline) => return k(Err(decline)),
        }
    };
    k(compile_frontier(
        graph, patterns, match_mode, row, &conjuncts, consumer, &stats,
    ))
}

/// The statement declines with exactly this named reason.
pub(crate) fn declines_with(cypher: &str, graph: &Graph, want: FrontierDecline) {
    decide(cypher, graph, false, &Row::new(), |decision| {
        assert_eq!(decision.err(), Some(want), "{cypher}");
    });
}

/// The three-node `File` triangle over `T` rels — the cycle that brings
/// every seed back to itself in three hops.
pub(crate) fn file_triangle() -> Built {
    build(
        &[&["File"], &["File"], &["File"]],
        &[(0, 1, "T"), (1, 2, "T"), (2, 0, "T")],
    )
}

/// Why the frontier family did not serve `cypher`'s `MATCH`, as the
/// PROFILE record names it — a PROFILED re-run of the statement, so the
/// reason is read where users read it (the ladder's returned fact). The
/// re-run's engagement tick and frontier counters are drained, so a
/// caller's own engagement assertions never see them.
pub(crate) fn frontier_decline_of(
    g: &Graph,
    cypher: &str,
) -> Option<crate::frontier_decline::FrontierDeclineReason> {
    let reason = served(&profiled(g, cypher)).frontier_decline;
    reset_frontier_facts();
    reason
}

/// Drain the frontier family's thread-local test facts — the kernel
/// engagement tick and the run counters — so the next statement's
/// assertions read only their own. The one spelling of the reset every
/// frontier suite performs before (and after) a run it measures.
pub(crate) fn reset_frontier_facts() {
    let _ = super::super::kernels::take_kernel_engagements();
    let _ = super::super::take_frontier_counters();
}
