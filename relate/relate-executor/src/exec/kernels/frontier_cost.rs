//! The frontier family's cost gate (ADR-0473's `engages()` gate extended
//! to the frontier, ADR-0478): whether the per-(node, state) frontier
//! beats the per-row DFS for one bounded quantified clause.
//!
//! The decision is a break-even between two measured unit costs, not a
//! ratio threshold: the DFS pays per *trail* it enumerates, the frontier
//! per distinct *(seed, endpoint) pair* it reports plus per adjacency
//! *probe* it issues. The inputs are estimates from the planner's stats:
//!
//! - **trails** — the first hop fans out over the start label's mean
//!   out-degree, every later hop over the size-biased excess degree
//!   `sum(d^2) / sum(d) - 1` of the rel types it crosses (a trail arrives
//!   at a node in proportion to its degree); an undirected hop sees each
//!   rel from both ends, so it counts twice. Summed over every hop count
//!   up to the bound, since the DFS enumerates the shorter trails too.
//!   The excess-degree model overestimates actual trails; it does not
//!   underestimate them, so a short bound that the model prices cheap is
//!   cheap.
//! - **pairs** — at most the trails, and at most seeds x the target's
//!   endpoint population.
//! - **probes** — at most the trails, and at most seeds x the
//!   adjacency entries of the first hop's rel types.
//!
//! An unbounded quantifier engages on shape alone (the DFS terminates
//! only at its rel-count cap). Statistics that cannot price the hops (no
//! histogram) keep the DFS.

use relate_ir::{Direction, Expr, NodePattern, PatternElement, RelPattern};
use relate_planner::{endpoint_estimate, seed_rows, DegreeMoments, PatternStats};

/// The DFS's measured cost per enumerated trail, in nanoseconds.
const WALKER_NS_PER_TRAIL: f64 = 1490.0;
/// The frontier's measured cost per distinct (seed, endpoint) pair.
const FRONTIER_NS_PER_PAIR: f64 = 2240.0;
/// The frontier's measured cost per adjacency probe.
const FRONTIER_NS_PER_PROBE: f64 = 65.0;

/// The estimate's hop ceiling: a bounded quantifier whose maximum
/// expansion passes it has a multiplicity saturated beyond pricing (a
/// degree tail chained per hop), so the estimate engages rather than
/// underestimates it.
const MAX_ESTIMATE_HOPS: usize = 1024;

/// What the DFS would enumerate and what the frontier would pay for it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct CostEstimate {
    /// Trails the DFS enumerates, over every hop count up to the bound.
    pub(super) trails: f64,
    /// Distinct (seed, endpoint) pairs the frontier reports.
    pub(super) pairs: f64,
    /// Adjacency probes the frontier issues.
    pub(super) probes: f64,
}

impl CostEstimate {
    /// Whether the frontier's priced work undercuts the DFS's.
    pub(super) fn frontier_wins(self) -> bool {
        WALKER_NS_PER_TRAIL * self.trails
            > FRONTIER_NS_PER_PAIR * self.pairs + FRONTIER_NS_PER_PROBE * self.probes
    }
}

/// The frontier family's cost gate — see the module doc. An unbounded
/// spine engages on shape alone, so the caller skips the gate for one
/// (it has no maximum hop count to price). `population` is
/// the store's node-id span, the ceiling on any endpoint count (an
/// unlabeled target's estimate is the ladder's rank sentinel, an ordering
/// fact no path count can reach).
pub(super) fn frontier_engages(
    stats: &dyn PatternStats,
    population: u128,
    conjuncts: &[Expr],
    start: &NodePattern,
    target: &NodePattern,
    elements: &[PatternElement],
) -> bool {
    let Some(hops) = expand_max_hops(elements) else {
        // The expansion passed the estimate's hop ceiling.
        return true;
    };
    let unbound = |_: &str| false;
    let seeds = seed_rows(endpoint_estimate(start, &unbound, conjuncts.iter(), stats));
    let endpoints =
        seed_rows(endpoint_estimate(target, &unbound, conjuncts.iter(), stats)).min(population);
    estimate(stats, population, seeds, endpoints, start, &hops)
        .is_some_and(CostEstimate::frontier_wins)
}

/// The cost estimate of `hops` (every quantifier expanded to its maximum
/// count) from `seeds` start rows, `None` when the statistics keep no
/// degree histogram to price them with.
pub(super) fn estimate(
    stats: &dyn PatternStats,
    population: u128,
    seeds: u128,
    endpoints: u128,
    start: &NodePattern,
    hops: &[&RelPattern],
) -> Option<CostEstimate> {
    let (first, rest) = hops.split_first()?;
    let start_population = start
        .labels
        .iter()
        .filter_map(|label| stats.label_rows(label))
        .min()
        .map_or(population, u128::from);
    let first_moments = stats.degree_moments(&start.labels, &first.types)?;
    let seeds = seeds as f64;
    let mut level = seeds * mean_degree(first_moments, start_population) * sides(first);
    let mut trails = level;
    // A quantifier repeats the same rels up to 1024 times, and each
    // degree-moments consult scans the histograms: price each distinct
    // rel once, so a crafted pattern costs consults in proportion to its
    // size, never to its expanded hop count.
    let mut priced: Vec<(&RelPattern, f64)> = Vec::new();
    for rel in rest {
        let fanout = match priced.iter().find(|(seen, _)| std::ptr::eq(*seen, *rel)) {
            Some(&(_, fanout)) => fanout,
            None => {
                let landing = stats.degree_moments(&[], &rel.types)?;
                let fanout = excess_degree(landing) * sides(rel);
                priced.push((rel, fanout));
                fanout
            }
        };
        level *= fanout;
        trails += level;
    }
    let adjacency = stats.degree_moments(&[], &first.types)?.sum as f64 * sides(first);
    Some(CostEstimate {
        trails,
        pairs: trails.min(seeds * endpoints as f64),
        probes: trails.min(seeds * adjacency),
    })
}

/// How many times a hop sees each rel: an undirected hop reads it from
/// both endpoints.
fn sides(rel: &RelPattern) -> f64 {
    match rel.direction {
        Direction::Both => 2.0,
        Direction::Outgoing | Direction::Incoming => 1.0,
    }
}

/// The mean out-degree over `nodes` nodes (zero for none).
pub(super) fn mean_degree(moments: DegreeMoments, nodes: u128) -> f64 {
    if nodes == 0 {
        return 0.0;
    }
    moments.sum as f64 / nodes as f64
}

/// The size-biased excess degree `sum(d^2) / sum(d) - 1` a trail finds at
/// the node it just arrived at (zero when no node has an edge).
pub(super) fn excess_degree(moments: DegreeMoments) -> f64 {
    if moments.sum == 0 {
        return 0.0;
    }
    (moments.sum_sq as f64 / moments.sum as f64 - 1.0).max(0.0)
}

/// The rels of `elements` with every quantifier expanded to its maximum
/// count — the trail estimate's input for the DFS's path multiplicity.
/// `None` when the expansion cannot be priced — it passes
/// [`MAX_ESTIMATE_HOPS`] (the multiplicity is saturated) or a quantifier
/// is unbounded — and the caller engages rather than underestimates it.
fn expand_max_hops(elements: &[PatternElement]) -> Option<Vec<&RelPattern>> {
    let mut hops = Vec::new();
    for element in elements {
        if hops.len() > MAX_ESTIMATE_HOPS {
            return None;
        }
        match element {
            // Only the rels matter to the estimate: nodes carry no
            // fanout of their own.
            PatternElement::Node(_) => {}
            PatternElement::Relationship(rel) => hops.push(rel),
            PatternElement::Quantified(qpp) => {
                let max = qpp.max?;
                // The body expands once; a body with no rel adds no
                // hops, and every repetition of one that does grows the
                // spine, so the ceiling bounds the loop.
                let body = expand_max_hops(&qpp.elements)?;
                if body.is_empty() {
                    continue;
                }
                for _ in 0..max {
                    hops.extend_from_slice(&body);
                    if hops.len() > MAX_ESTIMATE_HOPS {
                        return None;
                    }
                }
            }
        }
    }
    Some(hops)
}
