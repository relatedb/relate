//! The two-hop tally fold (REL8-2214, ADR-0693): `MATCH (a …)-[:R]->(b …)-[:S]->(c …)
//! RETURN count(*)` (or `count(<pattern var>)`) answered by one tally per
//! distinct middle node instead of one visit per path.
//!
//! The path count factors through the middle node: for a start `a` and an
//! admitted first-hop arrival `b`, the number of completions is the count of
//! admitted second-hop arrivals of `b`, a number that does not depend on `a`.
//! The fold therefore tallies each distinct `b` once and adds the tally once
//! per first-hop edge — `O(E1 + E2)` edge visits rather than `O(paths)`.
//!
//! **Why the memo is exact: the one lineage case.** The default match mode
//! requires the two rels of a path to be different. For two hops of the same
//! direction, `r1` is a rel of `a` and the candidates of the second hop are
//! rels of `b`, so `r1` can be a second-hop candidate only when `a == b` (a
//! self-loop). That one case — first hop arriving back at its own start — is
//! tallied explicitly with `r1` skipped, never read from the memo. Hops of
//! opposite directions, undirected hops and every other chain length decline
//! to the existing rungs.
//!
//! **The memo** is the family's own ([`Memo`], ADR-0657): a warm-up, the
//! statement's shared memo budget and ceiling, the hit-rate review and the
//! PROFILE counters all come from it, and a refusal or a bypass never fails
//! the query — the fold simply tallies the middle node again, back at the
//! per-path cost.
//!
//! **Memory.** Beyond the memo, the fold retains the admitted start
//! candidates (eight bytes each, the scan's own output, collected as the
//! chain-count fold does) and the widest start's first-hop buffer. Both are
//! charged to the statement's expression budget like every retaining
//! container (ADR-0463). Nothing grows with the path count. Every decline
//! keeps the unchanged plan.

use std::mem::size_of;

use relate_core::{Error, NodeId, RelId, Result};
use relate_ir::Direction;

use crate::ctx::ExecCtx;
use crate::overlay::{ExecGraph, RetainedTotal};

use super::super::agg_fold::FoldedReturn;
use super::super::match_folds::FoldCtx;
use super::super::WalkTallies;
use super::count_entry::CountEntry;
use super::count_fold_tail;
use super::matcher::Hop;
use super::memo::{Memo, MemoPolicy, MemoTable, Memoized};
use super::node_map::NodeMap;
use super::program::{Admission, NodePred};

/// The tally memo's table: a middle node to its completion count.
#[derive(Default)]
struct CompletionTable(NodeMap<u128>);

impl MemoTable for CompletionTable {
    type Key = NodeId;
    type Value = u128;

    fn get(&self, key: NodeId) -> Option<u128> {
        self.0.get(key).copied()
    }

    fn insert(&mut self, key: NodeId, value: u128) {
        self.0.insert(key, value);
    }

    fn heap_bytes(&self) -> usize {
        self.0.heap_bytes()
    }
}

/// The memo, folded into the PROFILE memo total when the run ends (the
/// way the node-label memo's state does, ADR-0654).
struct RecordedMemo(Memo<CompletionTable>);

impl Drop for RecordedMemo {
    fn drop(&mut self) {
        crate::exec::record_memos(|| self.0.stats().counters());
    }
}

/// Inputs tallied unmemoised before the memo starts: a tally is an
/// adjacency walk, so a few repeats already pay for the table.
const TALLY_WARM_UP: u32 = 16;

/// A byte count as the charge ledger's `u64`: a heap size cannot exceed
/// `u64` on any supported target, so the conversion states that invariant
/// rather than inventing a figure.
fn byte_count(bytes: usize) -> u64 {
    u64::try_from(bytes).expect("a heap byte count fits u64 on every supported target")
}

/// The fold entry: `clauses` starts at the `MATCH`, its `RETURN` follows.
pub(crate) fn try_fold_two_hop_tally(ctx: &FoldCtx<'_>) -> Result<Option<FoldedReturn>> {
    let Some(entry) = CountEntry::open(ctx) else {
        return Ok(None);
    };
    // The cheap structural gate before the compile, so a clause this fold
    // cannot take (every longer chain) does not compile its contract twice.
    if entry.hop_count() != 2 {
        return Ok(None);
    }
    let Some(contract) = entry.chain() else {
        return Ok(None);
    };
    let [first, second] = contract.hops.as_slice() else {
        return Ok(None);
    };
    if !same_directed(first, second) {
        return Ok(None);
    }
    let (exec, starts) = entry.scan(&contract)?;
    let count = tally(exec, first, second, &starts)?;
    Ok(count_fold_tail(exec.graph, count, entry.item))
}

/// Whether both hops walk the same, directed way: the one condition under
/// which the first hop's rel can reach the second hop's candidates only
/// through a self-loop (see the module doc).
fn same_directed(first: &Hop<'_>, second: &Hop<'_>) -> bool {
    first.traverse == second.traverse && !matches!(first.traverse, Direction::Both)
}

/// The run's per-statement state: the second hop's admission with its
/// lazily prepared form.
struct MiddleTallies<'a, 'g> {
    ctx: ExecCtx<'g>,
    second: &'a Hop<'a>,
    types2: crate::store::RelTypes,
    end_pred: Memoized<Admission<'a>>,
    end_prepared: Option<<Memoized<Admission<'a>> as NodePred>::Prepared>,
    tallies: WalkTallies,
}

impl MiddleTallies<'_, '_> {
    /// Count one admitted first-hop arrival in the walk counters. The fold
    /// reports walked rels only, never `hops` expansions — the PROFILE
    /// shape of the path fold it replaces for this clause.
    fn first_hop_arrived(&self) {
        self.tallies.walk();
    }

    /// The admitted second-hop arrivals of `middle`, skipping `skip` (the
    /// first hop's rel in the self-loop case, `None` otherwise).
    fn completions(&mut self, middle: NodeId, skip: Option<RelId>) -> Result<u128> {
        let graph: &ExecGraph<'_> = self.ctx.graph;
        let (ctx, end_pred) = (self.ctx, self.end_pred);
        let (end_prepared, tallies) = (&mut self.end_prepared, &self.tallies);
        let mut admitted: u128 = 0;
        graph.try_for_each_hop_resolved(
            middle,
            self.second.traverse,
            &self.types2,
            |rid, end| {
                graph.cancel_tick()?;
                if skip == Some(rid) || !end_pred.admits(end_prepared, ctx, end)? {
                    return Ok::<(), Error>(());
                }
                tallies.walk();
                admitted += 1;
                Ok(())
            },
        )?;
        Ok(admitted)
    }
}

/// The path count: for every start, every admitted first-hop arrival adds
/// its tally. The first-hop candidates of one start are buffered before
/// they are tallied, so no adjacency walk nests inside another; the
/// buffer's high-water allocation is charged like the start list.
fn tally(ctx: ExecCtx<'_>, first: &Hop<'_>, second: &Hop<'_>, starts: &[NodeId]) -> Result<u128> {
    let graph = ctx.graph;
    // The start candidates are a retaining container: charged up front.
    let mut retained: RetainedTotal =
        graph.retain_bytes(byte_count(std::mem::size_of_val(starts)))?;
    let types1 = graph.rel_types_of(first.rel);
    let mid_pred = Memoized(Admission::of(&first.target));
    let mut mid_prepared = None;
    let mut run = MiddleTallies {
        ctx,
        second,
        types2: graph.rel_types_of(second.rel),
        end_pred: Memoized(Admission::of(&second.target)),
        end_prepared: None,
        tallies: WalkTallies::default(),
    };
    let live = graph.memo_live();
    // The memoised function is a whole adjacency walk, far costlier than a
    // probe, so the memo starts after a handful of inputs rather than the
    // node-check policy's thousand (the break-even rule, ADR-0640).
    let policy = MemoPolicy {
        warm_up: TALLY_WARM_UP,
        ..MemoPolicy::moderate(&live)
    };
    let mut memo = RecordedMemo(Memo::new(policy, live));
    let mut total: u128 = 0;
    let mut arrivals: Vec<(RelId, NodeId)> = Vec::new();
    let mut buffer_charged = 0usize;
    for &start in starts {
        graph.cancel_tick()?;
        arrivals.clear();
        graph.try_for_each_hop_resolved(start, first.traverse, &types1, |rid, mid| {
            graph.cancel_tick()?;
            if arrivals.len() == arrivals.capacity() {
                // The buffer is about to grow: charge the growth before it
                // is allocated, so a capped statement refuses ahead of the
                // allocation, never after it.
                let grown = arrivals.capacity().max(4) * 2 * size_of::<(RelId, NodeId)>();
                if grown > buffer_charged {
                    retained.add_bytes(graph, byte_count(grown - buffer_charged))?;
                    buffer_charged = grown;
                }
            }
            arrivals.push((rid, mid));
            Ok::<(), Error>(())
        })?;
        for &(rid, mid) in &arrivals {
            graph.cancel_tick()?;
            if !mid_pred.admits(&mut mid_prepared, ctx, mid)? {
                continue;
            }
            run.first_hop_arrived();
            total += if mid == start {
                // The one lineage case: the first hop's rel is also a
                // candidate of the second hop from `mid`.
                run.completions(mid, Some(rid))?
            } else {
                memo.0.answer(graph, mid, || run.completions(mid, None))?
            };
        }
    }
    Ok(total)
}
