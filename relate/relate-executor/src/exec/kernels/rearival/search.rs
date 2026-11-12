//! The re-arrival rule's bounded-search core (ADR-0478 §3's remedy (a)):
//! the search policies — the one axis the rule's three searches differ
//! on — the immutable tables every search reads ([`SeekTables`]), and
//! the step core ([`seek`]). Split from the rule's own module (the
//! file-size rule), and reached only through it: the rule
//! (`super`) drives the search, owns the budget, the memo,
//! and the tri-state verdict (ADR-0516); this module answers per step.
//!
//! The early-stop channel is [`seek`]'s return type, spelled once:
//! `Err` carries which stop this is — a proven path ([`Stop::Found`];
//! the branch vector holds the witness, so nothing rides the unwind),
//! the budget's end ([`Stop::Exhausted`] — the cut is the search's
//! answer, never a miss), or a guard's evaluation error
//! ([`Stop::Failed`] — carried out of the run, the walker's own
//! abort) — and `Ok(())` is a fully-explored miss. The
//! recursion and the per-rel closure both propagate a stop with `?`,
//! and the rule's driver maps the channel onto the verdict exactly
//! once: the one tri-state spelling between [`Stop`] here and the
//! rule's `Verdict`.

use std::collections::HashSet;

use relate_core::{Error, NodeId, RelId, Result};

use crate::exec::candidates::node_ok;
use crate::overlay::ExecGraph;
use crate::row::Row;

use super::super::frontier::Arm;
use super::super::frontier_tables::Chain;
use super::super::nfa::FrontierNfa;

/// The immutable tables every bounded search reads: the overlay view,
/// the NFA, the runner's per-state expansion table (name-resolved
/// transition types — the one resolver, shared with the runner, never a
/// second copy), the per-state epsilon closure, and the search's hop
/// window. Built once per rule (per run); each search borrows it as one
/// value, so the step core takes six parameters instead of a dozen.
pub(super) struct SeekTables<'a, 'g> {
    pub(super) view: &'a ExecGraph<'g>,
    pub(super) nfa: &'a FrontierNfa,
    pub(super) arms: &'a [Vec<Arm>],
    /// Per-state epsilon closure (`{s}` plus epsilon-reachable states):
    /// the search's zero-cost positions, the runner's `close_epsilons`
    /// in to-indexed form.
    pub(super) eps_closure: Vec<Vec<u32>>,
    pub(super) min_hops: u32,
    pub(super) depth_cap: u32,
}

impl SeekTables<'_, '_> {
    /// Whether `node` satisfies the state's guards — the runner's own
    /// per-state check (the forced rung's class K) in the search's
    /// no-parameters form: the compile admits only context-free guards
    /// (static label expressions, literal property maps), so the empty
    /// row and parameter set are exact. A guard whose evaluation errors
    /// (a row-invariant `{k: 1/0}` map value the compile admits) is the
    /// error, never a rejection: the walker aborts on the same
    /// evaluation, so a pruned branch would change the answer.
    pub(super) fn state_admits(&self, state: u32, node: NodeId) -> Result<bool> {
        let guards = &self.nfa.state_guards[state as usize];
        let row = Row::new();
        for guard in guards {
            if !node_ok(self.view, node, guard, &row, &EMPTY_PARAMS)? {
                return Ok(false);
            }
        }
        Ok(true)
    }
}

/// The search's parameter set: empty, exact for the context-free guards
/// the compile admits (`SeekTables::state_admits`).
static EMPTY_PARAMS: std::collections::BTreeMap<String, relate_core::Value> =
    std::collections::BTreeMap::new();

/// One bounded search's per-pool policy — the one axis the rule's three
/// searches differ on, everything else shared
/// ([`super::RearivalRule::search`], [`seek`]): what state a
/// search carries, which rels it may traverse, which arrivals it may
/// descend into, and whether a fully-explored miss is a memoizable dead
/// end.
pub(super) trait SearchPolicy {
    /// The policy's per-search state (the walk-length check's
    /// failed-position memo, the node-simple search's visited set).
    type State;

    /// The state a fresh search from `seed` starts with.
    fn init(seed: NodeId) -> Self::State;

    /// Whether the position `(node, state)` at `hops` is a proven dead
    /// end before any expansion — the walk-length check's
    /// failed-position memo.
    fn dead_end(state: &Self::State, node: NodeId, state_id: u32, hops: u32) -> bool;

    /// What traversing `rid` from the current position to `next` does:
    /// skipped, descended into, or — the SIMPLE close only — accepted
    /// as the path's final hop without descending. `Err` is a guard's
    /// evaluation error on the close's acceptance check.
    fn arrive(
        state: &mut Self::State,
        ctx: &ArriveCtx<'_, '_>,
        rid: RelId,
        next: NodeId,
    ) -> Result<Arrive>;

    /// Undo a descent's bookkeeping once the branch below `next`
    /// explored without a hit — the node-visited set's removal.
    fn depart(state: &mut Self::State, next: NodeId);

    /// Record a position whose every arm explored without a hit — the
    /// walk-length check's dead-end memoization. Never called on the
    /// budget's early exit: an unproven miss is never a dead end.
    fn record_miss(state: &mut Self::State, node: NodeId, state_id: u32, hops: u32);
}

/// One search step's position: the `(node, state)` pair `hops` rel
/// steps into the branch — the step core's one shape for the three
/// facts it recurses on.
#[derive(Clone, Copy)]
pub(super) struct Position {
    pub(super) node: NodeId,
    pub(super) state: u32,
    pub(super) hops: u32,
}

/// What one rel traversal does under a policy.
pub(super) enum Arrive {
    /// The traversal is not admissible at this position (a reused rel
    /// under the trail-enforcing policies, a repeated node under the
    /// node-simple one).
    Skip,
    /// Descend: the rel joins the branch and the landing state's
    /// epsilon closure expands.
    Descend,
    /// The SIMPLE close: the traversal is the path's final hop back to
    /// the seed, accepted without descending — the closed cycle is the
    /// path's end.
    Close,
}

/// What one [`SearchPolicy::arrive`] decision reads: the branch's rels
/// (the distinct-rels state) and its first node (the seed the SIMPLE
/// close returns to), the search's target and current hop count, the
/// landing state's epsilon closure, and the search's tables — the
/// close's acceptance check reads the hop window, the acceptance table,
/// and the landing closure's guards off them (the closure's states must
/// admit the closing node, not merely accept it).
pub(super) struct ArriveCtx<'a, 'g> {
    pub(super) branch_rels: &'a [RelId],
    pub(super) branch_seed: NodeId,
    pub(super) target: NodeId,
    /// The current position's hop count — the close lands at `hops + 1`.
    pub(super) hops: u32,
    pub(super) eps_closure: &'a [u32],
    pub(super) tables: &'a SeekTables<'a, 'g>,
}

/// The trail-existence search's policy (the default pool): the branch's
/// rels are the distinct-rels state — a reused rel is skipped — and no
/// position is ever a proven dead end (the rel-set state makes every
/// branch's future different, which is what forbids the memo the
/// walk-length check takes). The distinct-rels state and the witness
/// are the one vector: the depth ceiling bounds the scan, and a found
/// leaf's vector is the trail. A miss under this policy is "no trail
/// within the bounds" — exact when the search ran to exhaustion, the
/// documented conservative corner when the depth ceiling cut it.
pub(super) struct TrailPolicy;

impl SearchPolicy for TrailPolicy {
    type State = ();

    fn init(_: NodeId) {}

    fn dead_end(_: &(), _: NodeId, _: u32, _: u32) -> bool {
        false
    }

    fn arrive(_: &mut (), ctx: &ArriveCtx<'_, '_>, rid: RelId, _: NodeId) -> Result<Arrive> {
        Ok(if ctx.branch_rels.contains(&rid) {
            Arrive::Skip
        } else {
            Arrive::Descend
        })
    }

    fn depart(_: &mut (), _: NodeId) {}

    fn record_miss(_: &mut (), _: NodeId, _: u32, _: u32) {}
}

/// The walk-length check's policy (the walk pool, ADR-0478 §3's
/// walk-pool candidate rule): any rel reusable, so the position
/// `(node, state, hops)` is the whole state and a fully-explored miss
/// is a memoized dead end — the search is polynomial in pairs × hop
/// bound, strictly cheaper than the trail search, whose rel-set state
/// forbids position memoization. A miss under this policy is "no
/// in-bounds walk within the bounds" — exact when the search ran to
/// exhaustion (every production walk-pool shape is bounded: the matcher
/// declines unbounded REPEATABLE, ADR-0478 §5), the documented
/// conservative corner when the depth ceiling cut it.
pub(super) struct WalkPolicy;

impl SearchPolicy for WalkPolicy {
    type State = HashSet<(NodeId, u32, u32)>;

    fn init(_: NodeId) -> Self::State {
        HashSet::new()
    }

    fn dead_end(failed: &Self::State, node: NodeId, state_id: u32, hops: u32) -> bool {
        failed.contains(&(node, state_id, hops))
    }

    fn arrive(_: &mut Self::State, _: &ArriveCtx<'_, '_>, _: RelId, _: NodeId) -> Result<Arrive> {
        Ok(Arrive::Descend)
    }

    fn depart(_: &mut Self::State, _: NodeId) {}

    fn record_miss(failed: &mut Self::State, node: NodeId, state_id: u32, hops: u32) {
        failed.insert((node, state_id, hops));
    }
}

/// The node-simple search's policy (the capped ACYCLIC/SIMPLE
/// emission's deferred ask): every node pairwise distinct — a
/// node-simple path is rel-distinct (the chain proof's argument), but
/// the rel check rides too (the DFS's own walk enforces rel-uniqueness
/// for every trail-enforcing mode, `enforce_unique`, and the SIMPLE
/// close can repeat the seed NODE over a rel the path already used).
/// `CLOSE = true` is the SIMPLE mode's one exception: a final hop back
/// to the seed, admitted when it lands the accepting position and never
/// explored past.
pub(super) struct NodeSimple<const CLOSE: bool>;

impl<const CLOSE: bool> SearchPolicy for NodeSimple<CLOSE> {
    type State = HashSet<NodeId>;

    fn init(seed: NodeId) -> Self::State {
        HashSet::from([seed])
    }

    fn dead_end(_: &Self::State, _: NodeId, _: u32, _: u32) -> bool {
        false
    }

    fn arrive(
        visited: &mut Self::State,
        ctx: &ArriveCtx<'_, '_>,
        rid: RelId,
        next: NodeId,
    ) -> Result<Arrive> {
        // A repeated rel is skipped before any node check: the node
        // checks alone would admit a closed walk reusing its own rel, a
        // row the DFS never emits.
        if ctx.branch_rels.contains(&rid) {
            return Ok(Arrive::Skip);
        }
        if !visited.contains(&next) {
            visited.insert(next);
            return Ok(Arrive::Descend);
        }
        // The one reopen is the SIMPLE close: the seed, as the accepting
        // landing — the core's depth guard pins `hops + 1 <= depth_cap`
        // (it checks the cap before expanding this position), and the
        // arrival state's closure decides acceptance.
        if CLOSE
            && next == ctx.target
            && next == ctx.branch_seed
            && ctx.hops + 1 >= ctx.tables.min_hops
        {
            for &state in ctx.eps_closure {
                if ctx.tables.nfa.accepting[state as usize]
                    && ctx.tables.state_admits(state, next)?
                {
                    return Ok(Arrive::Close);
                }
            }
        }
        Ok(Arrive::Skip)
    }

    fn depart(visited: &mut Self::State, next: NodeId) {
        visited.remove(&next);
    }

    fn record_miss(_: &mut Self::State, _: NodeId, _: u32, _: u32) {}
}

/// The early-stop channel: `try_for_each_neighbor_resolved` only halts
/// on an error, so the visitor's `Err` carries which stop this is — a
/// proven path (unwind with success; the branch vector holds the
/// witness, so nothing rides the unwind), the budget's end (unwind as
/// the cut; the driver turns it into the rule's `Exhausted` verdict and
/// the run's decline, ADR-0516 — never a miss), or a guard's
/// evaluation error (unwind and carry it out of the run — the walker
/// aborts on the same evaluation, never a pruned branch).
pub(super) enum Stop {
    /// A path was proven deeper in the branch: unwind, succeed.
    Found,
    /// The shared budget ran out mid-branch: unwind, cut.
    Exhausted,
    /// A guard's evaluation errored: unwind, raise.
    Failed(Error),
}

impl From<Error> for Stop {
    fn from(error: Error) -> Self {
        Stop::Failed(error)
    }
}

/// One search step — the bounded-DFS skeleton every pool shares: the
/// position `(node, state)` at `hops` rel steps into the branch whose
/// rels are `branch`'s. A free function over the shared tables and the
/// policy's state — the recursion mutates only `branch` and `work`, so
/// no `&mut self` fights the borrows of the tables it reads.
///
/// The early-stop channel is the return type: `Err` carries which stop
/// this is — a proven path ([`Stop::Found`]; the branch vector holds
/// the witness, so nothing rides the unwind), the budget's end
/// ([`Stop::Exhausted`] — the cut is the search's answer, never a miss,
/// and no miss is recorded on the way out: an unproven miss was already
/// never a dead end), or a guard's evaluation error ([`Stop::Failed`]).
/// `Ok(())` is a fully-explored miss. The recursion
/// and the per-rel closure both propagate a stop with `?` — one
/// channel, one spelling, mapped onto the verdict exactly once by the
/// rule's driver (ADR-0516).
pub(super) fn seek<P: SearchPolicy>(
    tables: &SeekTables<'_, '_>,
    policy: &mut P::State,
    branch: &mut Chain,
    work: &mut u64,
    target: NodeId,
    at: Position,
) -> std::result::Result<(), Stop> {
    let Position { node, state, hops } = at;
    if node == target && hops >= tables.min_hops && tables.nfa.accepting[state as usize] {
        return Err(Stop::Found);
    }
    if P::dead_end(policy, node, state, hops) || hops >= tables.depth_cap {
        return Ok(());
    }
    for arm in &tables.arms[state as usize] {
        let transition = &tables.nfa.transitions[arm.transition];
        let direction = transition.direction;
        let to = transition.to;
        let walked =
            tables
                .view
                .try_for_each_hop_resolved(node, direction, &arm.types, |rid, next| {
                    if *work == 0 {
                        return Err(Stop::Exhausted);
                    }
                    *work -= 1;
                    // The landing state's guards: a node the state's
                    // pattern rejects is a position the DFS never binds,
                    // whatever the policy's rel bookkeeping allows.
                    if !tables.state_admits(to, next)? {
                        return Ok(());
                    }
                    let ctx = ArriveCtx {
                        branch_rels: &branch.rels,
                        branch_seed: branch.nodes[0],
                        target,
                        hops,
                        eps_closure: &tables.eps_closure[to as usize],
                        tables,
                    };
                    match P::arrive(policy, &ctx, rid, next)? {
                        Arrive::Skip => return Ok(()),
                        // The close is the path's end: the branch holds the
                        // closed cycle, nothing descends past it.
                        Arrive::Close => {
                            branch.rels.push(rid);
                            branch.nodes.push(next);
                            return Err(Stop::Found);
                        }
                        Arrive::Descend => {}
                    }
                    branch.rels.push(rid);
                    branch.nodes.push(next);
                    for &state in &tables.eps_closure[to as usize] {
                        if !tables.state_admits(state, next)? {
                            continue;
                        }
                        let next_position = Position {
                            node: next,
                            state,
                            hops: hops + 1,
                        };
                        // A stop deeper in the branch unwinds through here:
                        // the found leaf's witness is the branch itself, and
                        // the cut ends every frame above it — no more work
                        // to do, no miss to record.
                        seek::<P>(tables, policy, branch, work, target, next_position)?;
                    }
                    branch.rels.pop();
                    branch.nodes.pop();
                    P::depart(policy, next);
                    Ok(())
                });
        walked?;
    }
    P::record_miss(policy, node, state, hops);
    Ok(())
}
