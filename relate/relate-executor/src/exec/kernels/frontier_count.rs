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
//! A filter on the target (labels, a property map, a MATCH-WHERE
//! predicate decoded onto it) is not a guard but an exact shape: it
//! depends only on the arrival node, never on the walk prefix, so the
//! last leg counts the out-neighbours the admission passes per reached
//! (node, state) pair instead of the plain degree — and an unfiltered
//! target admits every arrival without a check.
//!
//! The equivalence is otherwise guarded, every guard a decline to the
//! unchanged plan (answers identical):
//!
//! - the WALK pool only — a trail-enforcing mode's rel-uniqueness makes
//!   one walk's continuation depend on its own prefix, so the count does
//!   not contract through per-pair multiplicities;
//! - a deterministic NFA (the flat and group families plus their
//!   concatenation context), so no walk counts twice;
//! - a filter on an interior pattern node (a guarded group state);
//! - the answer is exactly one non-DISTINCT `count` over the walk rows
//!   (`count(*)`, or `count` of a boundary variable — always bound, so
//!   always counted) with no grouping, ordering, paging, or DISTINCT
//!   projection;
//! - a count past `i64` declines (the DFS keeps its own semantics).

use relate_core::{Error, NodeId, Result};

use crate::hash_seed::IdMap;
use relate_ir::{Clause, MatchMode};

use crate::ctx::ExecCtx;
use crate::metrics::FrontierCounters;
use crate::row::Row;

use super::super::agg_fold::FoldedReturn;
use super::super::graph_stats::GraphStats;
use super::super::match_folds::FoldCtx;
use super::super::pushdown::{flatten_and_owned, match_scope, Residual};
use super::frontier_match::{compile_frontier, Consumer, FrontierContract};
use super::frontier_run::{seeds, single_quantified};
use super::frontier_tables::arms;
use super::memo::Memoized;
use super::nfa::FrontierNfa;
use super::program::{Admission, NodePred};

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
    // The compile runs the row's own proofs first, then the clause's — the
    // row entry's order.
    let Ok(contract) = compile_frontier(
        graph.base(),
        patterns,
        MatchMode::RepeatableElements,
        &conjuncts,
        std::slice::from_ref(entering),
        Consumer::WalkCount,
        &stats,
    ) else {
        return Ok(None);
    };
    // The compile layer already declines every non-WALK pool for this
    // consumer, so only determinism is checked; the target filter rides
    // the last leg's arrival check.
    if !deterministic(&contract.nfa) {
        return Ok(None);
    }
    let ctx = ExecCtx::new(graph, params);
    let residual = Residual {
        conjuncts: &conjuncts,
        scope: &match_scope(entering, patterns),
    };
    // The target's admission is stage-fixed, so its prepared form (and the
    // memo's table) lives across the seeds. An unfiltered target admits
    // every arrival, so it takes the constant-true arm bare — the memo's
    // probe would cost the work — and a filtered one its compiled program,
    // memoized across the seeds.
    let walks = if contract.target.unfiltered() {
        count_seeds(ctx, &contract, entering, &residual, Unfiltered)
    } else {
        count_seeds(
            ctx,
            &contract,
            entering,
            &residual,
            Memoized(Admission::of(&contract.target)),
        )
    };
    let Some(total) = walks? else {
        return Ok(None);
    };
    #[cfg(test)]
    super::kernel_engaged();
    Ok(super::count_fold_tail(graph, total, item))
}

/// Every seed's [`count_walks`], summed. The target's one prepared
/// admission — the memo's table included — is held across the seeds: it
/// is a stage-fixed fact. `None` is an overflow decline.
fn count_seeds<P: NodePred>(
    ctx: ExecCtx<'_>,
    contract: &FrontierContract<'_>,
    entering: &Row,
    residual: &Residual<'_>,
    pred: P,
) -> Result<Option<u128>> {
    let mut prepared: Option<P::Prepared> = None;
    let mut total: u128 = 0;
    for seed in seeds(ctx, contract, entering, residual)? {
        let mut facts = FrontierCounters::default();
        let walks = count_walks(ctx, &contract.nfa, seed, pred, &mut prepared, &mut facts)?;
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
    Ok(Some(total))
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

/// The unfiltered target's arrival check: every node admits, without the
/// label arm's visibility read — sound because an arrival exists by
/// construction. It is not a compiled program (no [`Prog`] decodes to it),
/// and this fold is its one consumer; used bare, never memoised, because
/// the memo's probe would cost the work.
#[derive(Clone, Copy)]
struct Unfiltered;

impl NodePred for Unfiltered {
    type Prepared = ();

    fn prepare(&self, _ctx: ExecCtx<'_>) -> Result<()> {
        Ok(())
    }

    fn check(&self, _prepared: &mut (), _ctx: ExecCtx<'_>, _node: NodeId) -> Result<bool> {
        Ok(true)
    }
}

/// The accepted walks from `seed`: per-level (node, state)
/// multiplicities, each level's accepting arrivals counted as the
/// level's multiplicity times the pair's degree over the accepting
/// transition — the last level contracted to that degree sum, never
/// expanded. `None` is an overflow decline. The WALK pool is bounded
/// (the matcher's mode gate declines an unbounded REPEATABLE quantifier).
/// `facts` gathers the run's frontier counters: levels (level 0
/// included), the (node, state) cells, the incident-edge visits, and
/// the accepted walks as the endpoint tally. An accepting arrival counts
/// only when `pred` admits it (the zero-length seed arrival included).
fn count_walks<P: NodePred>(
    ctx: ExecCtx<'_>,
    nfa: &FrontierNfa,
    seed: NodeId,
    pred: P,
    prepared: &mut Option<P::Prepared>,
    facts: &mut FrontierCounters,
) -> Result<Option<u128>> {
    let max = nfa
        .max_hops
        .expect("the WALK pool is bounded: the mode gate declines an unbounded quantifier");
    let view = ctx.graph;
    let arms = arms(view, nfa);
    let mut total: u128 = u128::from(
        nfa.min_hops == 0
            && nfa.accepting[nfa.start as usize]
            && pred.admits(prepared, ctx, seed)?,
    );
    let mut level: IdMap<(NodeId, u32), u128> = IdMap::from_iter([((seed, nfa.start), 1)]);
    facts.levels = 1;
    facts.cells = 1;
    for hop in 1..=max {
        if level.is_empty() {
            break;
        }
        view.cancel_tick()?;
        facts.levels += 1;
        // The last level is contracted, never expanded: only its accepting
        // transitions are walked and no next level is built — the degree
        // sum is the answer's final contribution. The hop bound is at
        // least the lower bound, so `accepting` covers the bound here.
        let last = hop == max;
        let mut next: IdMap<(NodeId, u32), u128> = IdMap::default();
        let mut overflow = false;
        for (&(u, s), &walks) in &level {
            for arm in &arms[s as usize] {
                let t = &nfa.transitions[arm.transition];
                // One fact, one spelling: a transition contributes only
                // when it accepts within the lower bound.
                let accepting = t.accepts && hop >= nfa.min_hops;
                if last && !accepting {
                    continue;
                }
                let mut degree: u128 = 0;
                view.try_for_each_hop_resolved::<Error>(u, t.direction, &arm.types, |_, v| {
                    facts.edge_visits += 1;
                    if accepting && pred.admits(prepared, ctx, v)? {
                        degree += 1;
                    }
                    if !last {
                        let slot = next.entry((v, t.to)).or_insert(0);
                        match slot.checked_add(walks) {
                            Some(sum) => *slot = sum,
                            None => overflow = true,
                        }
                    }
                    Ok(())
                })?;
                if accepting {
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

/// One level's cell count, as the counters' unit.
fn level_len(level: &IdMap<(NodeId, u32), u128>) -> u64 {
    u64::try_from(level.len()).expect("a level's cells fit u64")
}
