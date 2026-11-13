//! The frontier family's walk-count arm (ADR-0523): `MATCH REPEATABLE
//! ELEMENTS (s)-[:R*m..n]->(d) RETURN count(*)` (or `count(<boundary
//! var>)`, no DISTINCT) answered without enumerating a single walk.
//!
//! The contraction: under the WALK pool a match is a walk — an edge
//! sequence the pattern's NFA accepts within the hop bounds — and a
//! deterministic NFA (one transition per state, no epsilon skips, no
//! guards) maps every walk to exactly one state run. The number of
//! accepted walks of length `k + 1` is then the sum, over the (node,
//! state) pairs reached at level `k` with their walk multiplicities, of
//! each pair's degree over its state's accepting transition: the count
//! contracts to per-level multiplicities, and the last leg to a degree
//! sum — the last level is never materialized. The work is `O(levels ×
//! reached pairs × degree)`, never the walk count.
//!
//! The equivalence is guarded, every guard a decline to the unchanged
//! plan (answers identical):
//!
//! - the WALK pool only — a trail-enforcing mode's rel-uniqueness makes
//!   one walk's continuation depend on its own prefix, so the count does
//!   not contract through per-pair multiplicities;
//! - the target unfiltered (no labels, label expression, property map,
//!   inline or MATCH-WHERE predicate) — the degree sum counts every
//!   arrival, so an admission on the arrival node would be skipped;
//! - a deterministic NFA (the flat and group families plus their
//!   concatenation context), so no walk counts twice;
//! - the answer is exactly one non-DISTINCT `count` over the walk rows
//!   (`count(*)`, or `count` of a boundary variable — always bound, so
//!   always counted) with no grouping, ordering, paging, or DISTINCT
//!   projection;
//! - a count past `i64` declines (the DFS keeps its own semantics).

use relate_core::{Error, NodeId, Result};

use crate::hash_seed::IdMap;
use relate_ir::{Clause, MatchMode};

use crate::ctx::{ExecCtx, FrontierCounters};
use crate::overlay::ExecGraph;

use super::super::agg_fold::FoldedReturn;
use super::super::graph_stats::GraphStats;
use super::super::match_folds::FoldCtx;
use super::super::pushdown::{flatten_and_owned, match_scope, Residual};
use super::frontier_match::{compile_frontier, Consumer};
use super::frontier_run::{seeds, single_quantified};
use super::frontier_tables::{arms, Arm};
use super::nfa::FrontierNfa;

/// The fold entry, tried first among the `MATCH` folds: `clauses` starts
/// at the `MATCH` and its `RETURN` follows. `Ok(None)` keeps every other
/// fold and the operator ladder exactly as before.
pub(crate) fn try_fold_walk_count(ctx: &FoldCtx<'_>) -> Result<Option<FoldedReturn>> {
    let (graph, rows, clauses, params) = (&ctx.graph, ctx.rows, ctx.clauses, ctx.params);
    let Some(Clause::Match {
        optional: false,
        match_mode: MatchMode::RepeatableElements,
        patterns,
        where_clause,
        search: None,
        using_index: _,
        using_join: _,
    }) = clauses.first()
    else {
        return Ok(None);
    };
    let [entering] = rows else {
        return Ok(None);
    };
    let Some(pattern) = single_quantified(graph.base(), patterns) else {
        return Ok(None);
    };
    let Some(item) = super::counted_return(clauses.get(1), pattern) else {
        return Ok(None);
    };
    let conjuncts = where_clause
        .as_ref()
        .map(flatten_and_owned)
        .unwrap_or_default();
    let stats = GraphStats::new(graph.base(), params);
    let Ok(contract) = compile_frontier(
        graph.base(),
        patterns,
        MatchMode::RepeatableElements,
        entering,
        &conjuncts,
        Consumer::WalkCount,
        &stats,
    ) else {
        return Ok(None);
    };
    // The compile layer already declines every non-WALK pool for this
    // consumer, so only the target filter and determinism are checked.
    if !contract.target.unfiltered() || !deterministic(&contract.nfa) {
        return Ok(None);
    }
    let ctx = ExecCtx::new(graph, params);
    let residual = Residual {
        conjuncts: &conjuncts,
        scope: &match_scope(entering, patterns),
    };
    let mut total: u128 = 0;
    for seed in seeds(ctx, &contract, entering, &residual)? {
        let mut facts = FrontierCounters::default();
        let walks = count_walks(graph, &contract.nfa, seed, &mut facts)?;
        // The run's accounting, either way — the rider's one helper, so
        // the PROFILE `[frontier]` counters and the neighbor-walk read
        // mix cannot drift between the two arms.
        super::frontier_run::record_run(facts);
        let Some(walks) = walks else {
            return Ok(None);
        };
        let Some(sum) = total.checked_add(walks) else {
            return Ok(None);
        };
        total = sum;
    }
    #[cfg(test)]
    super::kernel_engaged();
    Ok(super::count_fold_tail(graph, total, item))
}

/// Whether every walk maps to exactly one state run: no epsilon skips,
/// no per-state guards or transition predicates, at most one transition
/// leaving each state, and no type repeated within a transition's union
/// (each rel then matches a transition at most once).
fn deterministic(nfa: &FrontierNfa) -> bool {
    let mut leaving = vec![0u32; nfa.states as usize];
    for t in &nfa.transitions {
        leaving[t.from as usize] += 1;
        let mut types = t.types.clone();
        types.sort_unstable();
        types.dedup();
        if types.len() != t.types.len() {
            return false;
        }
    }
    nfa.epsilons.is_empty()
        && nfa.state_guards.iter().all(Vec::is_empty)
        && nfa.predicates.iter().all(Option::is_none)
        && leaving.iter().all(|&n| n <= 1)
}

/// The accepted walks from `seed`: per-level (node, state)
/// multiplicities, each level's accepting arrivals counted as the
/// level's multiplicity times the pair's degree over the accepting
/// transition — the last level contracted to that degree sum, never
/// expanded. `None` is an overflow decline. The WALK pool is bounded
/// (the matcher's mode gate declines an unbounded REPEATABLE quantifier).
/// `facts` gathers the run's frontier counters: levels (level 0
/// included), the (node, state) cells, the incident-edge visits, and
/// the accepted walks as the endpoint tally.
fn count_walks(
    view: &ExecGraph<'_>,
    nfa: &FrontierNfa,
    seed: NodeId,
    facts: &mut FrontierCounters,
) -> Result<Option<u128>> {
    let max = nfa
        .max_hops
        .expect("the WALK pool is bounded: the mode gate declines an unbounded quantifier");
    let arms = arms(view, nfa);
    let mut total: u128 = u128::from(nfa.min_hops == 0 && nfa.accepting[nfa.start as usize]);
    let mut level: IdMap<(NodeId, u32), u128> = IdMap::from_iter([((seed, nfa.start), 1)]);
    facts.levels = 1;
    facts.cells = 1;
    for hop in 1..=max {
        if level.is_empty() {
            break;
        }
        view.cancel_tick()?;
        facts.levels += 1;
        if hop == max {
            // The last level is contracted, never expanded: acceptance only.
            return match accept_last_level(view, nfa, &arms, &level, hop, total, facts)? {
                Some(sum) => {
                    facts.endpoints = tally(sum);
                    Ok(Some(sum))
                }
                None => Ok(None),
            };
        }
        let mut next: IdMap<(NodeId, u32), u128> = IdMap::default();
        let mut overflow = false;
        for (&(u, s), &walks) in &level {
            for arm in &arms[s as usize] {
                let t = &nfa.transitions[arm.transition];
                let mut degree: u128 = 0;
                view.try_for_each_hop_resolved::<Error>(u, t.direction, &arm.types, |_, v| {
                    facts.edge_visits += 1;
                    degree += 1;
                    let slot = next.entry((v, t.to)).or_insert(0);
                    match slot.checked_add(walks) {
                        Some(sum) => *slot = sum,
                        None => overflow = true,
                    }
                    Ok(())
                })?;
                if t.accepts && hop >= nfa.min_hops {
                    match walks.checked_mul(degree).and_then(|w| total.checked_add(w)) {
                        Some(sum) => total = sum,
                        None => overflow = true,
                    }
                }
            }
        }
        if overflow {
            return Ok(None);
        }
        facts.cells += level_len(&next);
        level = next;
    }
    facts.endpoints = tally(total);
    Ok(Some(total))
}

/// A saturating tally: the PROFILE counter, never the answer.
fn tally(total: u128) -> u64 {
    u64::try_from(total.min(u128::from(u64::MAX))).expect("clamped to u64")
}

/// The last level's accepting arrivals: each pair's multiplicity times its
/// degree over the accepting transition, summed onto `total` — no next
/// level is built. `None` is an overflow decline.
fn accept_last_level(
    view: &ExecGraph<'_>,
    nfa: &FrontierNfa,
    arms: &[Vec<Arm>],
    level: &IdMap<(NodeId, u32), u128>,
    hop: u32,
    mut total: u128,
    facts: &mut FrontierCounters,
) -> Result<Option<u128>> {
    if hop < nfa.min_hops {
        return Ok(Some(total));
    }
    for (&(u, s), &walks) in level {
        for arm in &arms[s as usize] {
            let t = &nfa.transitions[arm.transition];
            if !t.accepts {
                continue;
            }
            let mut degree: u128 = 0;
            view.try_for_each_hop_resolved::<Error>(u, t.direction, &arm.types, |_, _| {
                facts.edge_visits += 1;
                degree += 1;
                Ok(())
            })?;
            match walks.checked_mul(degree).and_then(|w| total.checked_add(w)) {
                Some(sum) => total = sum,
                None => return Ok(None),
            }
        }
    }
    Ok(Some(total))
}

/// One level's cell count, as the counters' unit.
fn level_len(level: &IdMap<(NodeId, u32), u128>) -> u64 {
    u64::try_from(level.len()).expect("a level's cells fit u64")
}
