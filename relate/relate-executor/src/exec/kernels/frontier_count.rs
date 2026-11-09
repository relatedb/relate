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

use std::collections::BTreeMap;

use relate_core::{Error, NodeId, Result, Value};

use crate::hash_seed::FxMap;
use relate_ir::{classify_aggregate, AggregateKind, Clause, Expr, MatchMode, Projection};

use crate::ctx::{ExecCtx, FrontierCounters};
use crate::overlay::ExecGraph;
use crate::row::Row;

use super::super::agg_fold::FoldedReturn;
use super::super::graph_stats::GraphStats;
use super::super::pushdown::{flatten_and_owned, match_scope, Residual};
use super::frontier::arms;
use super::frontier_match::{boundary, boundary_vars, compile_frontier, Consumer};
use super::frontier_run::{seeds, single_quantified};
use super::nfa::FrontierNfa;

/// The fold entry, tried first among the `MATCH` folds: `clauses` starts
/// at the `MATCH` and its `RETURN` follows. `Ok(None)` keeps every other
/// fold and the operator ladder exactly as before.
pub(crate) fn try_fold_walk_count(
    graph: &ExecGraph<'_>,
    rows: &[Row],
    clauses: &[Clause],
    params: &BTreeMap<String, Value>,
    cypher_version: Option<u32>,
) -> Result<Option<FoldedReturn>> {
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
    let Some(item) = counted_return(clauses.get(1), pattern) else {
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
    let ctx = ExecCtx::new(graph, params, cypher_version);
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
    let Ok(count) = i64::try_from(total) else {
        return Ok(None);
    };
    #[cfg(test)]
    super::kernel_engaged();
    let name = item.output_name(0);
    let mut out = Row::new();
    out.insert(name.clone(), Value::Int(count));
    Ok(Some(FoldedReturn {
        result: super::super::QueryResult::intermediate(
            vec![name],
            vec![vec![Value::Int(count)]],
            graph.stats(),
        ),
        rows: vec![out],
        consumed_after: 1,
    }))
}

/// The one projection item when the suffix is exactly `RETURN
/// count(*)` / `RETURN count(<boundary var>)` — non-DISTINCT, one item,
/// no grouping, ordering, or paging: the answer is the walk count.
fn counted_return<'c>(
    clause: Option<&'c Clause>,
    pattern: &relate_ir::PathPattern,
) -> Option<&'c Projection> {
    let Some(Clause::Return {
        distinct: false,
        group_by: None,
        items,
        order,
        skip: None,
        limit: None,
    }) = clause
    else {
        return None;
    };
    let ([item], true) = (items.as_slice(), order.is_empty()) else {
        return None;
    };
    match &item.expr {
        Expr::CountStar => Some(item),
        Expr::Function {
            name,
            distinct: false,
            args,
        } if name.with_str(classify_aggregate) == Some(AggregateKind::Count) => {
            let [Expr::Variable(var)] = args.as_slice() else {
                return None;
            };
            let (start, target) = boundary(pattern)?;
            boundary_vars(start, target)
                .into_iter()
                .flatten()
                .any(|name| name == var)
                .then_some(item)
        }
        _ => None,
    }
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
    let mut level: FxMap<(NodeId, u32), u128> = FxMap::from_iter([((seed, nfa.start), 1)]);
    facts.levels = 1;
    facts.cells = 1;
    for hop in 1..=max {
        if level.is_empty() {
            break;
        }
        view.cancel_tick()?;
        facts.levels += 1;
        let expand = hop < max;
        let mut next: FxMap<(NodeId, u32), u128> = FxMap::default();
        let mut overflow = false;
        for (&(u, s), &walks) in &level {
            for arm in &arms[s as usize] {
                let t = &nfa.transitions[arm.transition];
                let accept = t.accepts && hop >= nfa.min_hops;
                if !accept && !expand {
                    continue;
                }
                let mut degree: u128 = 0;
                view.try_for_each_hop_resolved::<Error>(u, t.direction, &arm.types, |_, v| {
                    facts.edge_visits += 1;
                    degree += 1;
                    if expand {
                        let slot = next.entry((v, t.to)).or_insert(0);
                        match slot.checked_add(walks) {
                            Some(sum) => *slot = sum,
                            None => overflow = true,
                        }
                    }
                    Ok(())
                })?;
                if accept {
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
    // A saturating tally: the PROFILE counter, never the answer.
    facts.endpoints = u64::try_from(total.min(u128::from(u64::MAX))).expect("clamped to u64");
    Ok(Some(total))
}

/// One level's cell count, as the counters' unit.
fn level_len(level: &FxMap<(NodeId, u32), u128>) -> u64 {
    u64::try_from(level.len()).expect("a level's cells fit u64")
}
